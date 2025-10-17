use std::collections::HashMap;
use std::collections::HashSet;
use std::mem;
use std::sync::Arc;
use std::time::Duration;
use std::time::SystemTime;
use std::time::UNIX_EPOCH;

use futures::future::Either;
use futures::future::join_all;
use futures::future::select;
use futures::pin_mut;
use futures::stream::repeat;
use futures::stream::select_all;
use http::HeaderMap;
use http::StatusCode;
use http::header::ACCEPT;
use jsonwebtoken::Algorithm;
use jsonwebtoken::DecodingKey;
use jsonwebtoken::TokenData;
use jsonwebtoken::Validation;
use jsonwebtoken::decode;
use jsonwebtoken::jwk::AlgorithmParameters;
use jsonwebtoken::jwk::EllipticCurve;
use jsonwebtoken::jwk::Jwk;
use jsonwebtoken::jwk::JwkSet;
use jsonwebtoken::jwk::KeyAlgorithm;
use jsonwebtoken::jwk::KeyOperations;
use jsonwebtoken::jwk::PublicKeyUse;
use mime::APPLICATION_JSON;
use parking_lot::RwLock;
use tokio::fs::read_to_string;
use tokio::sync::oneshot;
use tower::BoxError;
use tracing_futures::Instrument;
use url::Url;

use super::APOLLO_AUTHENTICATION_JWT_CLAIMS;
use super::CLIENT;
use super::DEFAULT_AUTHENTICATION_NETWORK_TIMEOUT;
use super::Header;
use super::Source;
use crate::Context;
use crate::plugins::authentication::error::AuthenticationError;

#[derive(Clone)]
pub(super) struct JwksManager {
    list: Vec<JwksConfig>,
    jwks_map: Arc<RwLock<HashMap<Url, JwkSet>>>,
    _drop_signal: Arc<oneshot::Sender<()>>,
}

pub(super) type Issuers = HashSet<String>;
pub(super) type Audiences = HashSet<String>;

#[derive(Clone)]
pub(super) struct JwksConfig {
    pub(super) url: Url,
    pub(super) issuers: Option<Issuers>,
    pub(super) audiences: Option<Audiences>,
    pub(super) algorithms: Option<HashSet<Algorithm>>,
    pub(super) poll_interval: Duration,
    pub(super) headers: Vec<Header>,
}

#[derive(Clone)]
pub(super) struct JwkSetInfo {
    pub(super) jwks: JwkSet,
    pub(super) issuers: Option<Issuers>,
    pub(super) audiences: Option<Audiences>,
    pub(super) algorithms: Option<HashSet<Algorithm>>,
}

impl JwksManager {
    pub(super) async fn new(list: Vec<JwksConfig>) -> Result<Self, BoxError> {
        use futures::FutureExt;

        let downloads = list
            .iter()
            .cloned()
            .map(|JwksConfig { url, headers, .. }| {
                let span = tracing::info_span!("fetch jwks", url = %url);
                get_jwks(url.clone(), headers.clone())
                    .map(|opt_jwks| opt_jwks.map(|jwks| (url, jwks)))
                    .instrument(span)
            })
            .collect::<Vec<_>>();

        let jwks_map: HashMap<_, _> = join_all(downloads).await.into_iter().flatten().collect();

        let jwks_map = Arc::new(RwLock::new(jwks_map));
        let (_drop_signal, drop_receiver) = oneshot::channel::<()>();

        tokio::task::spawn(poll(list.clone(), jwks_map.clone(), drop_receiver));

        Ok(JwksManager {
            list,
            jwks_map,
            _drop_signal: Arc::new(_drop_signal),
        })
    }

    #[cfg(test)]
    pub(super) fn new_test(list: Vec<JwksConfig>, jwks: HashMap<Url, JwkSet>) -> Self {
        let (_drop_signal, _) = oneshot::channel::<()>();

        JwksManager {
            list,
            jwks_map: Arc::new(RwLock::new(jwks)),
            _drop_signal: Arc::new(_drop_signal),
        }
    }

    pub(super) fn iter_jwks(&self) -> Iter<'_> {
        Iter {
            list: self.list.clone(),
            manager: self,
        }
    }
}

async fn poll(
    list: Vec<JwksConfig>,
    jwks_map: Arc<RwLock<HashMap<Url, JwkSet>>>,
    drop_receiver: oneshot::Receiver<()>,
) {
    use futures::stream::StreamExt;

    let mut streams = select_all(list.into_iter().map(move |config| {
        let jwks_map = jwks_map.clone();
        Box::pin(
            repeat((config, jwks_map)).then(|(config, jwks_map)| async move {
                tokio::time::sleep(config.poll_interval).await;

                if let Some(jwks) = get_jwks(config.url.clone(), config.headers.clone()).await {
                    jwks_map.write().insert(config.url, jwks);
                }
            }),
        )
    }));

    pin_mut!(drop_receiver);

    loop {
        let next = streams.next();
        pin_mut!(next);

        match select(drop_receiver, next).await {
            // the _drop_signal was dropped, we must shut down the task
            Either::Left((_res, _)) => return,
            // another JWKS download was performed
            Either::Right((Some(()), receiver)) => {
                drop_receiver = receiver;
            }
            Either::Right((None, _)) => return,
        };
    }
}

// This function is expected to return an Optional value, but we'd like to let
// users know the various failure conditions. Hence, the various clumsy map_err()
// scattered through the processing.
pub(super) async fn get_jwks(url: Url, headers: Vec<Header>) -> Option<JwkSet> {
    let data = if url.scheme() == "file" {
        let path = url
            .to_file_path()
            .inspect_err(|_| {
                tracing::error!("url cannot be converted to filesystem path");
            })
            .ok()?;
        read_to_string(path)
            .await
            .inspect_err(|e| {
                tracing::error!(%e, "could not read JWKS path");
            })
            .ok()?
    } else {
        let my_client = CLIENT
            .as_ref()
            .inspect_err(|e| {
                tracing::error!(%e, "could not activate authentication feature");
            })
            .ok()?
            .clone();

        let mut builder = my_client
            .get(url)
            .header(ACCEPT, APPLICATION_JSON.essence_str());

        for header in headers.into_iter() {
            builder = builder.header(header.name, header.value);
        }

        builder
            .timeout(DEFAULT_AUTHENTICATION_NETWORK_TIMEOUT)
            .send()
            .await
            .map_err(|e| {
                tracing::error!(%e, "could not get url");
                e
            })
            .ok()?
            .text()
            .await
            .map_err(|e| {
                tracing::error!(%e, "could not process url content");
                e
            })
            .ok()?
    };

    let jwks = parse_jwks(&data)?;
    Some(jwks)
}

pub(crate) fn parse_jwks(data: &str) -> Option<JwkSet> {
    // Some JWKS contain algorithms which are not supported by the jsonwebtoken library. That means
    // we can't just deserialize from the retrieved data and proceed. Any unrecognised
    // algorithms will cause deserialization to fail.
    //
    // Try to identify any entries which contain algorithms which are not supported by
    // jsonwebtoken and exclude them
    tracing::debug!(data, "parsing JWKS");

    let mut raw_json: serde_json::Value = serde_json::from_str(data)
        .map_err(|e| {
            tracing::error!(%e, "could not create JSON Value from url content, enable debug logs to see content");
            e
        })
        .ok()?;

    // remove any keys that can't be parsed
    raw_json.get_mut("keys").and_then(|keys| {
        keys.as_array_mut().map(|array| {
            *array = mem::take(array).into_iter().enumerate().filter(|(index, key)| {
                if let Err(err) = serde_json::from_value::<Jwk>(key.clone()) {
                    let alg = key.get("alg").and_then(|alg|alg.as_str()).unwrap_or("<unknown>");
                    tracing::warn!(%err, alg, index, "ignoring a key since it is not valid, enable debug logs to full content");
                    return false;
                }
                true
            }).map(|(_, key)| key).collect();
        })
    });
    let jwks: JwkSet = serde_json::from_value(raw_json)
        .map_err(|e| {
            tracing::error!(%e, "could not create JWKS from url content, enable debug logs to see content");
            e
        })
        .ok()?;
    Some(jwks)
}

pub(super) struct Iter<'a> {
    manager: &'a JwksManager,
    list: Vec<JwksConfig>,
}

impl Iterator for Iter<'_> {
    type Item = JwkSetInfo;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            match self.list.pop() {
                None => return None,
                Some(config) => {
                    let map = self.manager.jwks_map.read();
                    if let Some(jwks) = map.get(&config.url) {
                        return Some(JwkSetInfo {
                            jwks: jwks.clone(),
                            issuers: config.issuers.clone(),
                            audiences: config.audiences.clone(),
                            algorithms: config.algorithms.clone(),
                        });
                    }
                }
            }
        }
    }
}

#[derive(Debug, Default)]
pub(super) struct JWTCriteria {
    pub(super) alg: Algorithm,
    pub(super) kid: Option<String>,
}

pub(super) type SearchResult = (Option<Issuers>, Option<Audiences>, Jwk);

/// Search the list of JWKS to find a key we can use to decode a JWT.
///
/// The search criteria allow us to match a variety of keys depending on which criteria are provided
/// by the JWT header. The only mandatory parameter is "alg".
/// Note: "none" is not implemented by jsonwebtoken, so it can't be part of the [`Algorithm`] enum.
pub(super) fn search_jwks(
    jwks_manager: &JwksManager,
    criteria: &JWTCriteria,
) -> Option<Vec<SearchResult>> {
    const HIGHEST_SCORE: usize = 2;
    let mut candidates = vec![];
    let mut found_highest_score = false;
    for JwkSetInfo {
        jwks,
        issuers,
        audiences,
        algorithms,
    } in jwks_manager.iter_jwks()
    {
        // filter accepted algorithms
        if let Some(algs) = algorithms
            && !algs.contains(&criteria.alg)
        {
            continue;
        }

        // Try to figure out if our jwks contains a candidate key (i.e.: a key which matches our
        // criteria)
        for mut key in jwks.keys.into_iter().filter(|key| {
            // We are only interested in keys which are used for signature verification
            match (&key.common.public_key_use, &key.common.key_operations) {
                // "use" https://datatracker.ietf.org/doc/html/rfc7517#section-4.2 and
                // "key_ops" https://datatracker.ietf.org/doc/html/rfc7517#section-4.3 are both optional
                (None, None) => true,
                (None, Some(purpose)) => purpose.contains(&KeyOperations::Verify),
                (Some(key_use), None) => key_use == &PublicKeyUse::Signature,
                // The "use" and "key_ops" JWK members SHOULD NOT be used together;
                // however, if both are used, the information they convey MUST be
                // consistent
                (Some(key_use), Some(purpose)) => {
                    key_use == &PublicKeyUse::Signature && purpose.contains(&KeyOperations::Verify)
                }
            }
        }) {
            let mut key_score = 0;

            // Let's see if we have a specified kid and if they match
            if criteria.kid.is_some() && key.common.key_id == criteria.kid {
                key_score += 1;
            }

            // Furthermore, we would like our algorithms to match, or at least the kty
            // If we have an algorithm that matches, boost the score
            match key.common.key_algorithm {
                Some(algorithm) => {
                    if convert_key_algorithm(algorithm) != Some(criteria.alg) {
                        continue;
                    }
                    key_score += 1;
                }
                // If a key doesn't have an algorithm, then we match the "alg" specified in the
                // search criteria against all of the algorithms that we support.  If the
                // key.algorithm parameters match the type of parameters for the "family" of the
                // criteria "alg", then we update the key to use the value of "alg" provided in
                // the search criteria.
                // If not, then this is not a usable key for this JWT
                // Note: Matching algorithm parameters may seem unusual, but the appropriate
                // algorithm details are not structured for easy consumption in jsonwebtoken and
                // this is the simplest way to determine algorithm family.
                None => match (criteria.alg, &key.algorithm) {
                    (
                        Algorithm::HS256 | Algorithm::HS384 | Algorithm::HS512,
                        AlgorithmParameters::OctetKey(_),
                    ) => {
                        key.common.key_algorithm = Some(convert_algorithm(criteria.alg));
                    }
                    (
                        Algorithm::RS256
                        | Algorithm::RS384
                        | Algorithm::RS512
                        | Algorithm::PS256
                        | Algorithm::PS384
                        | Algorithm::PS512,
                        AlgorithmParameters::RSA(_),
                    ) => {
                        key.common.key_algorithm = Some(convert_algorithm(criteria.alg));
                    }
                    (Algorithm::ES256, AlgorithmParameters::EllipticCurve(params)) => {
                        if params.curve == EllipticCurve::P256 {
                            key.common.key_algorithm = Some(convert_algorithm(criteria.alg));
                        }
                    }
                    (Algorithm::ES384, AlgorithmParameters::EllipticCurve(params)) => {
                        if params.curve == EllipticCurve::P384 {
                            key.common.key_algorithm = Some(convert_algorithm(criteria.alg));
                        }
                    }
                    (Algorithm::EdDSA, AlgorithmParameters::EllipticCurve(params)) => {
                        if params.curve == EllipticCurve::Ed25519 {
                            key.common.key_algorithm = Some(convert_algorithm(criteria.alg));
                        }
                    }
                    _ => {
                        // We'll ignore combinations we don't recognise
                        continue;
                    }
                },
            };

            // If we get here we have a key that:
            //  - may be used for signature verification
            //  - has a matching algorithm, or if JWT has no algorithm, a matching key type
            // It may have a matching kid if the JWT has a kid and it matches the key kid
            //
            // Multiple keys may meet the matching criteria, but they have a score. They get 1
            // point for having an explicitly matching algorithm and 1 point for an explicitly
            // matching kid. We will sort our candidates and pick the key with the highest score.

            // If we find a key with a HIGHEST_SCORE, we will filter the list to only keep those
            // with that score
            if key_score == HIGHEST_SCORE {
                found_highest_score = true;
            }

            candidates.push((key_score, (issuers.clone(), audiences.clone(), key)));
        }
    }

    tracing::debug!(
        "jwk candidates: {:?}",
        candidates
            .iter()
            .map(|(score, (_, _, candidate))| (
                score,
                &candidate.common.key_id,
                candidate.common.key_algorithm
            ))
            .collect::<Vec<(&usize, &Option<String>, Option<KeyAlgorithm>)>>()
    );

    if candidates.is_empty() {
        None
    } else {
        // Only sort if we need to
        if candidates.len() > 1 {
            candidates.sort_by(|a, b| a.0.cmp(&b.0));
        }

        if found_highest_score {
            Some(
                candidates
                    .into_iter()
                    .filter_map(|(score, candidate)| {
                        if score == HIGHEST_SCORE {
                            Some(candidate)
                        } else {
                            None
                        }
                    })
                    .collect(),
            )
        } else {
            Some(
                candidates
                    .into_iter()
                    .map(|(_score, candidate)| candidate)
                    .collect(),
            )
        }
    }
}

pub(super) fn extract_jwt<'a, 'b: 'a>(
    source: &'a Source,
    ignore_other_prefixes: bool,
    headers: &'b HeaderMap,
) -> Option<Result<&'b str, AuthenticationError>> {
    match source {
        Source::Header { name, value_prefix } => {
            // The http_request is stored in a `Router::Request` context.
            // We are going to check the headers for the presence of the configured header
            let jwt_value_result = headers
                .get(name)?
                .to_str()
                .map_err(|_err| AuthenticationError::CannotConvertToString);

            // If we find the header, but can't convert it to a string, let the client know
            let jwt_value_untrimmed = match jwt_value_result {
                Ok(value) => value,
                Err(err) => {
                    return Some(Err(err));
                }
            };

            // Let's trim out leading and trailing whitespace to be accommodating
            let jwt_value = jwt_value_untrimmed.trim();

            // Make sure the format of our message matches our expectations
            // Technically, the spec is case-sensitive, but let's accept
            // case variations
            let prefix_len = value_prefix.len();
            if jwt_value.len() < prefix_len
                || !&jwt_value[..prefix_len].eq_ignore_ascii_case(value_prefix)
            {
                return if ignore_other_prefixes {
                    None
                } else {
                    Some(Err(AuthenticationError::InvalidJWTPrefix(
                        name.to_owned(),
                        value_prefix.to_owned(),
                    )))
                };
            }
            // If there's no header prefix, we avoid splitting the header
            let jwt = if value_prefix.is_empty() {
                // check for whitespace — we've already trimmed, so this means the request has a
                // prefix that shouldn't exist
                if jwt_value.contains(' ') {
                    return Some(Err(AuthenticationError::InvalidJWTPrefix(
                        name.to_owned(),
                        value_prefix.to_owned(),
                    )));
                }

                // we can simply assign the jwt to the jwt_value; we'll validate down below
                jwt_value
            } else {
                // Otherwise, we need to split our string in (at most 2) sections.
                let jwt_parts: Vec<&str> = jwt_value.splitn(2, ' ').collect();
                if jwt_parts.len() != 2 {
                    return Some(Err(AuthenticationError::MissingJWTToken(
                        name.to_owned(),
                        value_prefix.to_owned(),
                    )));
                }

                // We have our jwt
                jwt_parts[1]
            };
            Some(Ok(jwt))
        }
        Source::Cookie { name } => {
            for header in headers.get_all("cookie") {
                let value = match header.to_str() {
                    Ok(value) => value,
                    Err(_not_a_string_error) => {
                        return Some(Err(AuthenticationError::CannotConvertToString));
                    }
                };
                for cookie in cookie::Cookie::split_parse(value) {
                    match cookie {
                        Err(_) => continue,
                        Ok(cookie) => {
                            if cookie.name() == name
                                && let Some(value) = cookie.value_raw()
                            {
                                return Some(Ok(value));
                            }
                        }
                    }
                }
            }

            None
        }
    }
}

pub(super) type DecodedClaims = (
    Option<Issuers>,
    Option<Audiences>,
    TokenData<serde_json::Value>,
);

pub(super) fn decode_jwt(
    jwt: &str,
    keys: Vec<SearchResult>,
    criteria: JWTCriteria,
) -> Result<DecodedClaims, (AuthenticationError, StatusCode)> {
    let mut error = None;
    for (issuers, audiences, jwk) in keys.into_iter() {
        let decoding_key = match DecodingKey::from_jwk(&jwk) {
            Ok(k) => k,
            Err(e) => {
                error = Some((
                    AuthenticationError::CannotCreateDecodingKey(e),
                    StatusCode::INTERNAL_SERVER_ERROR,
                ));
                continue;
            }
        };

        let key_algorithm = match jwk.common.key_algorithm {
            Some(a) => a,
            None => {
                error = Some((
                    AuthenticationError::JWKHasNoAlgorithm,
                    StatusCode::INTERNAL_SERVER_ERROR,
                ));
                continue;
            }
        };

        let algorithm = match convert_key_algorithm(key_algorithm) {
            Some(a) => a,
            None => {
                error = Some((
                    AuthenticationError::UnsupportedKeyAlgorithm(key_algorithm),
                    StatusCode::INTERNAL_SERVER_ERROR,
                ));
                continue;
            }
        };

        let mut validation = Validation::new(algorithm);
        validation.validate_nbf = true;
        // if set to true, it will reject tokens containing an `aud` claim if the validation does not specify an audience
        // we don't validate audience yet, so this is deactivated
        validation.validate_aud = false;

        match decode::<serde_json::Value>(jwt, &decoding_key, &validation) {
            Ok(v) => return Ok((issuers, audiences, v)),
            Err(e) => {
                tracing::trace!("JWT decoding failed with error `{e}`");
                error = Some((
                    AuthenticationError::CannotDecodeJWT(e),
                    StatusCode::UNAUTHORIZED,
                ));
            }
        };
    }

    match error {
        Some(e) => Err(e),
        None => {
            // We can't find a key to process this JWT.
            Err((
                criteria.kid.map_or_else(
                    || AuthenticationError::CannotFindSuitableKey(criteria.alg, None),
                    AuthenticationError::CannotFindKID,
                ),
                StatusCode::UNAUTHORIZED,
            ))
        }
    }
}

pub(crate) fn jwt_expires_in(context: &Context) -> Duration {
    context
        .get(APOLLO_AUTHENTICATION_JWT_CLAIMS)
        .unwrap_or_else(|err| {
            tracing::error!("could not read JWT claims: {err}");
            None
        })
        .flatten()
        .and_then(|claims_value: Option<serde_json::Value>| {
            let claims_obj = claims_value.as_ref()?.as_object();
            // Extract the expiry claim from the JWT
            let exp = match claims_obj {
                Some(exp) => exp.get("exp"),
                None => {
                    tracing::error!("expected JWT claims to be an object");
                    None
                }
            };
            // Ensure the expiry claim is an integer
            match exp.and_then(|it| it.as_i64()) {
                Some(ts) => Some(ts),
                None => {
                    tracing::error!("expected JWT 'exp' (expiry) claim to be an integer");
                    None
                }
            }
        })
        .map(|exp| {
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("no time travel allowed")
                .as_secs() as i64;
            if now < exp {
                Duration::from_secs((exp - now) as u64)
            } else {
                Duration::ZERO
            }
        })
        .unwrap_or(Duration::MAX)
}

// Apparently the `jsonwebtoken` crate now has 2 different enums for algorithms
pub(crate) fn convert_key_algorithm(algorithm: KeyAlgorithm) -> Option<Algorithm> {
    Some(match algorithm {
        KeyAlgorithm::HS256 => Algorithm::HS256,
        KeyAlgorithm::HS384 => Algorithm::HS384,
        KeyAlgorithm::HS512 => Algorithm::HS512,
        KeyAlgorithm::ES256 => Algorithm::ES256,
        KeyAlgorithm::ES384 => Algorithm::ES384,
        KeyAlgorithm::RS256 => Algorithm::RS256,
        KeyAlgorithm::RS384 => Algorithm::RS384,
        KeyAlgorithm::RS512 => Algorithm::RS512,
        KeyAlgorithm::PS256 => Algorithm::PS256,
        KeyAlgorithm::PS384 => Algorithm::PS384,
        KeyAlgorithm::PS512 => Algorithm::PS512,
        KeyAlgorithm::EdDSA => Algorithm::EdDSA,
        // We don't use these encryption algorithms
        KeyAlgorithm::RSA1_5 | KeyAlgorithm::RSA_OAEP | KeyAlgorithm::RSA_OAEP_256 => return None,
    })
}

fn convert_algorithm(algorithm: Algorithm) -> KeyAlgorithm {
    match algorithm {
        Algorithm::HS256 => KeyAlgorithm::HS256,
        Algorithm::HS384 => KeyAlgorithm::HS384,
        Algorithm::HS512 => KeyAlgorithm::HS512,
        Algorithm::ES256 => KeyAlgorithm::ES256,
        Algorithm::ES384 => KeyAlgorithm::ES384,
        Algorithm::RS256 => KeyAlgorithm::RS256,
        Algorithm::RS384 => KeyAlgorithm::RS384,
        Algorithm::RS512 => KeyAlgorithm::RS512,
        Algorithm::PS256 => KeyAlgorithm::PS256,
        Algorithm::PS384 => KeyAlgorithm::PS384,
        Algorithm::PS512 => KeyAlgorithm::PS512,
        Algorithm::EdDSA => KeyAlgorithm::EdDSA,
    }
}

#[cfg(test)]
mod test {
    use std::time::Duration;
    use std::time::UNIX_EPOCH;

    use serde_json_bytes::json;

    use super::APOLLO_AUTHENTICATION_JWT_CLAIMS;
    use super::Context;
    use super::jwt_expires_in;
    use crate::test_harness::tracing_test;

    #[test]
    fn test_exp_defaults_to_max_when_no_jwt_claims_present() {
        let context = Context::new();
        let expiry = jwt_expires_in(&context);
        assert_eq!(expiry, Duration::MAX);
    }

    #[test]
    fn test_jwt_claims_not_object() {
        let _guard = tracing_test::dispatcher_guard();

        let context = Context::new();
        context.insert_json_value(APOLLO_AUTHENTICATION_JWT_CLAIMS, json!("not an object"));

        let expiry = jwt_expires_in(&context);
        assert_eq!(expiry, Duration::MAX);

        assert!(tracing_test::logs_contain(
            "expected JWT claims to be an object"
        ));
    }

    #[test]
    fn test_expiry_claim_not_integer() {
        let _guard = tracing_test::dispatcher_guard();

        let context = Context::new();
        context.insert_json_value(
            APOLLO_AUTHENTICATION_JWT_CLAIMS,
            json!({
                "exp": "\"not an integer\""
            }),
        );

        let expiry = jwt_expires_in(&context);
        assert_eq!(expiry, Duration::MAX);

        assert!(tracing_test::logs_contain(
            "expected JWT 'exp' (expiry) claim to be an integer"
        ));
    }

    #[test]
    fn test_expiry_claim_is_valid_but_expired() {
        let context = Context::new();
        context.insert_json_value(
            APOLLO_AUTHENTICATION_JWT_CLAIMS,
            json!({
                "exp": 0
            }),
        );

        let expiry = jwt_expires_in(&context);
        assert_eq!(expiry, Duration::ZERO);
    }

    #[test]
    fn test_expiry_claim_is_valid() {
        let context = Context::new();
        let exp = UNIX_EPOCH.elapsed().unwrap().as_secs() + 3600;
        context.insert_json_value(
            APOLLO_AUTHENTICATION_JWT_CLAIMS,
            json!({
                "exp": exp
            }),
        );

        let expiry = jwt_expires_in(&context);
        assert_eq!(expiry, Duration::from_secs(3600));
    }
}
