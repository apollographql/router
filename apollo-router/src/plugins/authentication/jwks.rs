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
use serde_json::Value;
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
    pub(super) allow_missing_exp: bool,
    pub(super) headers: Vec<Header>,
}

#[derive(Clone)]
pub(super) struct JwkSetInfo {
    pub(super) jwks: JwkSet,
    pub(super) issuers: Option<Issuers>,
    pub(super) audiences: Option<Audiences>,
    pub(super) allow_missing_exp: bool,
    pub(super) algorithms: Option<HashSet<Algorithm>>,
}

impl JwksManager {
    pub(super) async fn new(list: Vec<JwksConfig>) -> Result<Self, BoxError> {
        use futures::FutureExt;

        let downloads = list
            .iter()
            .map(|JwksConfig { url, headers, .. }| {
                let url = url.clone();
                let headers = headers.clone();
                let span = tracing::info_span!("fetch jwks", url = %url);
                get_jwks(url.clone(), headers)
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
                let url = e.url().map(Url::as_str).unwrap_or("<unknown>");
                let msg = if e.is_timeout() {
                    "JWKS request timed out"
                } else if e.is_connect() {
                    "JWKS request connection failed"
                } else {
                    "JWKS request failed"
                };
                tracing::error!(url, %e, "{msg}");
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
                            allow_missing_exp: config.allow_missing_exp,
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

pub(super) struct SearchResult {
    pub(super) issuers: Option<Issuers>,
    pub(super) audiences: Option<Audiences>,
    pub(super) jwk: Jwk,
    pub(super) allow_missing_exp: bool,
}

/// A key that matched the search criteria, tagged with *why* it matched so `search_jwks` can
/// filter results (by kid) and order them (by alg).
struct Candidate {
    /// The token carried a kid and it equalled this key's kid.
    kid_matched: bool,
    /// The token's alg explicitly matched this key's declared algorithm.
    alg_matched: bool,
    result: SearchResult,
}

/// Search the list of JWKS to find a key we can use to decode a JWT.
///
/// The search criteria allow us to match a variety of keys depending on which criteria are provided
/// by the JWT header. The only mandatory parameter is "alg".
/// Note: "none" is not implemented by jsonwebtoken, so it can't be part of the [`Algorithm`] enum.
pub(super) fn search_jwks(
    jwks_manager: &JwksManager,
    criteria: &JWTCriteria,
) -> Option<Vec<SearchResult>> {
    // For each key we track two independent signals: whether the token's kid matched the key's
    // kid, and whether the token's alg matched the key's algorithm. A kid match is more specific
    // than an alg match: per RFC 7517 §4.5 the "kid" is "used to match a specific key" (and
    // RFC 7515 §4.1.4 makes it a hint to which key signed the JWS), whereas "alg" (RFC 7517 §4.4)
    // only identifies an algorithm category that many keys can share. We use the kid signal to
    // decide which candidates to keep (see below) and the alg signal only to order them.
    let mut candidates: Vec<Candidate> = vec![];
    for JwkSetInfo {
        jwks,
        issuers,
        audiences,
        allow_missing_exp,
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
            // Let's see if we have a specified kid and if they match
            let kid_matched = criteria.kid.is_some() && key.common.key_id == criteria.kid;

            // Furthermore, we would like our algorithms to match, or at least the kty.
            // Record whether the algorithm explicitly matched so we can order candidates by it.
            let alg_matched = match key.common.key_algorithm {
                Some(algorithm) => {
                    if convert_key_algorithm(algorithm) != Some(criteria.alg) {
                        continue;
                    }
                    true
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
                None => {
                    match (criteria.alg, &key.algorithm) {
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
                    }
                    // A key without a declared "alg" that matches by family is usable, but this
                    // is not an explicit algorithm match.
                    false
                }
            };

            // If we get here we have a key that:
            //  - may be used for signature verification
            //  - has a matching algorithm, or if JWT has no algorithm, a matching key type
            //  - may have a matching kid, if the JWT has a kid and it matches the key's kid
            candidates.push(Candidate {
                kid_matched,
                alg_matched,
                result: SearchResult {
                    issuers: issuers.clone(),
                    audiences: audiences.clone(),
                    jwk: key,
                    allow_missing_exp,
                },
            });
        }
    }

    tracing::debug!(
        "jwk candidates: {:?}",
        candidates
            .iter()
            .map(|candidate| (
                candidate.kid_matched,
                candidate.alg_matched,
                &candidate.result.jwk.common.key_id,
                candidate.result.jwk.common.key_algorithm
            ))
            .collect::<Vec<(bool, bool, &Option<String>, Option<KeyAlgorithm>)>>()
    );

    // The token's kid, when present and matched, identifies the specific key(s) intended to
    // validate it (RFC 7517 §4.5). If any candidate matched the kid, drop the ones that did not,
    // so a matched kid-specific entry (and its issuer/audience constraints) can't be bypassed by
    // falling through to a key the kid never matched (ROUTER-1990).
    //
    // We keep *all* kid-matched candidates rather than a single "best" one: the same key may be
    // duplicated across entries with different constraints (e.g. multi-tenant IdPs), so decode_jwt
    // must be able to retry each in turn (ROUTER-1691). This holds even when the duplicates differ
    // in algorithm specificity — one declares "alg", another does not — since "alg" only names a
    // category, not a specific key. When the kid matched nothing, every candidate is kept,
    // preserving the pre-existing algorithm-based matching.
    if candidates.iter().any(|candidate| candidate.kid_matched) {
        candidates.retain(|candidate| candidate.kid_matched);
    }

    // The surviving candidates now share the same `kid_matched` value, so order them (stably) by
    // `alg_matched` alone — least- to most-specific — so callers that take the last element get the
    // best match, while the relative order decode_jwt retries in is otherwise preserved.
    if candidates.len() > 1 {
        candidates.sort_by_key(|candidate| candidate.alg_matched);
    }

    // Return None rather than an empty list when nothing matched, so the caller reports that no
    // suitable key was found.
    let results: Vec<SearchResult> = candidates
        .into_iter()
        .map(|candidate| candidate.result)
        .collect();
    (!results.is_empty()).then_some(results)
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

pub(super) type DecodedClaims = TokenData<serde_json::Value>;

pub(super) fn decode_jwt(
    jwt: &str,
    search_results: Vec<SearchResult>,
    criteria: JWTCriteria,
) -> Result<DecodedClaims, (AuthenticationError, StatusCode)> {
    let mut error = None;
    for search_result in search_results.into_iter() {
        match validate_jwk_against_jwt(jwt, search_result) {
            Ok(result) => return Ok(result),
            Err(err) => {
                error = Some(err);
            }
        }
    }

    match error {
        Some(e) => Err(e),
        None => {
            // We can't find a key to process this JWT.
            let err = match criteria.kid {
                Some(kid) => AuthenticationError::CannotFindKID(kid),
                None => AuthenticationError::CannotFindSuitableKey(criteria.alg, None),
            };
            Err((err, StatusCode::UNAUTHORIZED))
        }
    }
}

fn validate_jwk_against_jwt(
    jwt: &str,
    search_result: SearchResult,
) -> Result<DecodedClaims, (AuthenticationError, StatusCode)> {
    let SearchResult {
        issuers,
        audiences,
        jwk,
        allow_missing_exp,
    } = search_result;

    let decoding_key = match DecodingKey::from_jwk(&jwk) {
        Ok(k) => k,
        Err(e) => {
            return Err((
                AuthenticationError::CannotCreateDecodingKey(e),
                StatusCode::INTERNAL_SERVER_ERROR,
            ));
        }
    };

    let key_algorithm = match jwk.common.key_algorithm {
        Some(a) => a,
        None => {
            return Err((
                AuthenticationError::JWKHasNoAlgorithm,
                StatusCode::INTERNAL_SERVER_ERROR,
            ));
        }
    };

    let algorithm = match convert_key_algorithm(key_algorithm) {
        Some(a) => a,
        None => {
            return Err((
                AuthenticationError::UnsupportedKeyAlgorithm(key_algorithm),
                StatusCode::INTERNAL_SERVER_ERROR,
            ));
        }
    };

    let mut validation = Validation::new(algorithm);
    validation.validate_nbf = true;
    // if set to true, it will reject tokens containing an `aud` claim if the validation does not specify an audience
    // we don't validate audience yet, so this is deactivated
    validation.validate_aud = false;
    if allow_missing_exp {
        validation.required_spec_claims.remove("exp");
    }

    let token_data = match decode::<serde_json::Value>(jwt, &decoding_key, &validation) {
        Ok(v) => v,
        Err(e) => {
            tracing::trace!("JWT decoding failed with error `{e}`");
            return Err((
                AuthenticationError::CannotDecodeJWT(e),
                StatusCode::UNAUTHORIZED,
            ));
        }
    };

    if let Some(configured_issuers) = issuers {
        let maybe_token_issuers = token_data.claims.as_object().and_then(|o| o.get("iss"));
        if let Err(err) = validate_issuers(&configured_issuers, maybe_token_issuers) {
            return Err((err, StatusCode::INTERNAL_SERVER_ERROR));
        }
    }

    if let Some(configured_audiences) = audiences {
        let maybe_token_audiences = token_data.claims.as_object().and_then(|o| o.get("aud"));
        if let Err(err) = validate_audiences(&configured_audiences, maybe_token_audiences) {
            return Err((err, StatusCode::UNAUTHORIZED));
        }
    }

    Ok(token_data)
}

fn validate_issuers(
    configured_issuers: &Issuers,
    token_issuer: Option<&serde_json::Value>,
) -> Result<(), AuthenticationError> {
    let issuer_error = |actual: String| {
        // Standardize issuer - sort it and join the elements with a comma
        let mut issuers: Vec<String> = configured_issuers.iter().cloned().collect();
        issuers.sort();

        let expected = issuers.join(", ");
        Err(AuthenticationError::InvalidIssuer {
            expected,
            token: actual,
        })
    };

    if configured_issuers.is_empty() {
        // No issuers to compare against
        return Ok(());
    }

    match token_issuer {
        // With an issuers allowlist configured, a token that omits `iss` cannot satisfy it, so
        // reject it rather than letting it through. Mirrors `validate_audiences`. `iss` is OPTIONAL
        // per RFC 7519 §4.1.1, which leaves the required/optional decision to the application;
        // configuring an allowlist is that decision.
        None => issuer_error("<none>".to_string()),

        Some(Value::String(token_issuer)) => {
            // Check if this issuer is in our list
            if configured_issuers.contains(token_issuer) {
                Ok(())
            } else {
                issuer_error(token_issuer.to_string())
            }
        }

        Some(unexpected_value) => {
            // A null or otherwise non-string `iss` cannot match any configured issuer.
            issuer_error(unexpected_value.to_string())
        }
    }
}

fn validate_audiences(
    configured_audiences: &Audiences,
    token_audiences: Option<&serde_json::Value>,
) -> Result<(), AuthenticationError> {
    let audience_error = |actual: String| {
        // Standardize audience - sort it and join the elements with a comma
        let mut audiences: Vec<String> = configured_audiences.iter().cloned().collect();
        audiences.sort();

        let expected = audiences.join(", ");
        Err(AuthenticationError::InvalidAudience { expected, actual })
    };

    if configured_audiences.is_empty() {
        // No audiences to compare against
        return Ok(());
    }

    let Some(token_audiences) = token_audiences else {
        return audience_error("<none>".to_string());
    };

    match token_audiences {
        Value::String(token_audience) => {
            // Check if this audience exists in our list
            if configured_audiences.contains(token_audience) {
                Ok(())
            } else {
                audience_error(token_audience.to_string())
            }
        }

        Value::Array(token_audiences_arr) => {
            // Check if any of these audiences is in our list
            for token_audience in token_audiences_arr.iter().filter_map(|aud| aud.as_str()) {
                if configured_audiences.contains(token_audience) {
                    return Ok(());
                }
            }

            // No matches, so return an error
            audience_error(token_audiences.to_string())
        }

        unexpected_value => {
            // If the token has incorrectly configured audiences, we cannot validate it against
            // the configured audiences.
            audience_error(unexpected_value.to_string())
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
        KeyAlgorithm::RSA1_5
        | KeyAlgorithm::RSA_OAEP
        | KeyAlgorithm::RSA_OAEP_256
        | KeyAlgorithm::UNKNOWN_ALGORITHM => return None,
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

    /// Verifies that a connection failure to a JWKS URL produces an actionable error log
    /// that includes both the URL and the failure category (timeout, connection error, etc.)
    /// rather than a generic "could not get url" message.
    #[tokio::test]
    async fn test_get_jwks_unreachable_url_logs_helpful_error() {
        let _guard = tracing_test::dispatcher_guard();

        let url = "http://127.0.0.1:1/jwks.json".parse().expect("valid URL");

        let result = super::get_jwks(url, vec![]).await;
        assert!(result.is_none());

        assert!(tracing_test::logs_contain("JWKS request connection failed"));
        assert!(tracing_test::logs_contain("127.0.0.1:1"));
    }
}
