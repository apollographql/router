//! Authentication plugin
// With regards to ELv2 licensing, this entire file is license key functionality

use std::ops::ControlFlow;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

use deduplicate::Deduplicate;
use deduplicate::DeduplicateFuture;
use http::header::ACCEPT;
use http::header::CONTENT_TYPE;
use http::StatusCode;
use jsonwebtoken::decode;
use jsonwebtoken::decode_header;
use jsonwebtoken::jwk::JwkSet;
use jsonwebtoken::DecodingKey;
use jsonwebtoken::EncodingKey;
use jsonwebtoken::Header;
use jsonwebtoken::Validation;
use once_cell::sync::Lazy;
use reqwest::Client;
use schemars::JsonSchema;
use serde::Deserialize;
use tokio::fs::read_to_string;
use tower::BoxError;
use tower::ServiceBuilder;
use tower::ServiceExt;
use url::Url;

use crate::error::LicenseError;
use crate::graphql;
use crate::layers::ServiceBuilderExt;
use crate::plugin::Plugin;
use crate::plugin::PluginInit;
use crate::register_plugin;
use crate::services::apollo_graph_reference;
use crate::services::apollo_key;
use crate::services::router;
use crate::Context;

type SharedDeduplicate = Arc<
    Deduplicate<Box<dyn Fn(Url) -> DeduplicateFuture<JwkSet> + Send + Sync + 'static>, Url, JwkSet>,
>;

pub(crate) const AUTHENTICATION_SPAN_NAME: &str = "authentication plugin";

const DEFAULT_AUTHENTICATION_TIMEOUT: Duration = Duration::from_secs(15);

static CLIENT: Lazy<Result<Client, BoxError>> = Lazy::new(|| {
    apollo_graph_reference().ok_or(LicenseError::MissingGraphReference)?;
    apollo_key().ok_or(LicenseError::MissingKey)?;
    Ok(Client::new())
});

struct AuthenticationPlugin {
    configuration: Conf,
    jwks: SharedDeduplicate,
    jwks_url: Url,
}

#[derive(Clone, Debug, Default, Deserialize, JsonSchema)]
struct Conf {
    // Retrieve our JWK Set from here
    jwks_url: String,
    // HTTP header expected to contain JWT
    #[serde(default = "default_header_name")]
    header_name: String,
    // Header prefix
    #[serde(default = "default_header_prefix")]
    header_prefix: String,
    // Key retention policy
    #[serde(default)]
    retain_keys: bool,
}

fn default_header_name() -> String {
    http::header::AUTHORIZATION.to_string()
}

fn default_header_prefix() -> String {
    "Bearer".to_string()
}

#[async_trait::async_trait]
impl Plugin for AuthenticationPlugin {
    type Config = Conf;

    async fn new(init: PluginInit<Self::Config>) -> Result<Self, BoxError> {
        let url: Url = Url::from_str(&init.config.jwks_url)?;
        let getter: Box<dyn Fn(Url) -> DeduplicateFuture<JwkSet> + Send + Sync + 'static> =
            Box::new(|url: Url| -> DeduplicateFuture<JwkSet> {
                let fut = async {
                    let data = if url.scheme() == "file" {
                        // TODO: Uncomment to make this commercial only before devcomplete
                        /*
                        apollo_graph_reference()
                            .ok_or(LicenseError::MissingGraphReference)
                            .ok()?;
                        apollo_key().ok_or(LicenseError::MissingKey).ok()?;
                        */
                        let path = url.to_file_path().ok()?;
                        read_to_string(path).await.ok()?
                    } else {
                        let my_client = CLIENT.as_ref().map_err(|e| e.to_string()).ok()?.clone();

                        my_client
                            .get(url)
                            .header(ACCEPT, "application/json")
                            .header(CONTENT_TYPE, "application/json")
                            .timeout(DEFAULT_AUTHENTICATION_TIMEOUT)
                            .send()
                            .await
                            .ok()?
                            .text()
                            .await
                            .ok()?
                    };
                    let jwks: JwkSet = serde_json::from_str(&data).ok()?;
                    Some(jwks)
                };
                Box::pin(fut)
            });
        let deduplicator = Deduplicate::with_capacity(getter, 1);
        // XXX For debugging, generate a valid JWT...
        let key = "c2VjcmV0Cg==";
        let claims = serde_json::json!( {
            "exp": 10_000_000_000usize,
            "another claim": "this is another claim"
        });
        let header = Header {
            kid: Some("gary".to_string()),
            ..Default::default()
        };
        let tok_maybe =
            jsonwebtoken::encode(&header, &claims, &EncodingKey::from_base64_secret(key)?);
        tracing::info!(?tok_maybe, "use this JWT for testing");

        Ok(AuthenticationPlugin {
            configuration: init.config,
            jwks: Arc::new(deduplicator),
            jwks_url: url,
        })
    }

    fn router_service(&self, service: router::BoxService) -> router::BoxService {
        let request_full_config = self.configuration.clone();
        let request_jwks = self.jwks.clone();
        let request_jwks_url = self.jwks_url.clone();

        fn external_service_span() -> impl Fn(&router::Request) -> tracing::Span + Clone {
            move |_request: &router::Request| {
                tracing::info_span!(
                    AUTHENTICATION_SPAN_NAME,
                    "authentication service" = stringify!(router::Request),
                    "otel.kind" = "INTERNAL"
                )
            }
        }

        ServiceBuilder::new()
            .instrument(external_service_span())
            .checkpoint_async(move |request: router::Request| {
                let my_config = request_full_config.clone();
                let my_jwks = request_jwks.clone();
                let my_jwks_url = request_jwks_url.clone();

                async move {
                    // We are going to do a lot of similar checking so let's define a local function
                    // to help reduce repetition
                    fn failure_message(
                        context: Context,
                        msg: String,
                        status: StatusCode,
                    ) -> Result<ControlFlow<router::Response, router::Request>, BoxError>
                    {
                        let response = router::Response::error_builder()
                            .error(
                                graphql::Error::builder()
                                    .message(msg)
                                    .extension_code("AUTH_ERROR")
                                    .build(),
                            )
                            .status_code(status)
                            .context(context)
                            .build()?;
                        Ok(ControlFlow::Break(response))
                    }

                    // The http_request is stored in a `Router::Request` context.
                    // We are going to check the headers for the presence of the configured header
                    let jwt_value_result =
                        match request.router_request.headers().get(&my_config.header_name) {
                            Some(value) => value.to_str(),
                            None =>
                            // Prepare an HTTP 401 response with a GraphQL error message
                            {
                                return failure_message(
                                    request.context,
                                    format!("Missing '{}' header", &my_config.header_name),
                                    StatusCode::UNAUTHORIZED,
                                );
                            }
                        };

                    // If we find the header, but can't convert it to a string, let the client know
                    let jwt_value_untrimmed = match jwt_value_result {
                        Ok(value) => value,
                        Err(_not_a_string_error) => {
                            // Prepare an HTTP 400 response with a GraphQL error message
                            return failure_message(
                                request.context,
                                "configured header is not convertible to a string".to_string(),
                                StatusCode::BAD_REQUEST,
                            );
                        }
                    };

                    // Let's trim out leading and trailing whitespace to be accommodating
                    let jwt_value = jwt_value_untrimmed.trim();

                    // Make sure the format of our message matches our expectations
                    // Technically, the spec is case sensitive, but let's accept
                    // case variations
                    if !jwt_value
                        .to_uppercase()
                        .as_str()
                        .starts_with(&my_config.header_prefix.to_uppercase())
                    {
                        // Prepare an HTTP 400 response with a GraphQL error message
                        return failure_message(
                            request.context,
                            format!("'{jwt_value_untrimmed}' is not correctly formatted"),
                            StatusCode::BAD_REQUEST,
                        );
                    }

                    // Split our string in (at most 2) sections.
                    let jwt_parts: Vec<&str> = jwt_value.splitn(2, ' ').collect();
                    if jwt_parts.len() != 2 {
                        // Prepare an HTTP 400 response with a GraphQL error message
                        return failure_message(
                            request.context,
                            format!("'{jwt_value}' is not correctly formatted"),
                            StatusCode::BAD_REQUEST,
                        );
                    }

                    // Trim off any trailing white space (not valid in BASE64 encoding)
                    let jwt = jwt_parts[1].trim_end();

                    // Try to create a valid header to work with
                    let jwt_header = match decode_header(jwt) {
                        Ok(h) => h,
                        Err(e) => {
                            return failure_message(
                                request.context,
                                format!("'{jwt}' is not a valid JWT header: {e}"),
                                StatusCode::BAD_REQUEST,
                            );
                        }
                    };

                    // Try to find the kid of the header
                    let kid = match jwt_header.kid {
                        Some(k) => k,
                        None => {
                            return failure_message(
                                request.context,
                                "Missing kid value from JWT header".to_string(),
                                StatusCode::BAD_REQUEST,
                            );
                        }
                    };

                    // Get the JWKS here
                    // If the user has instructed us to clear the cache on fail, then we should.
                    let jwks_opt = match my_jwks.get(my_jwks_url).await {
                        Ok(k) => k,
                        Err(e) => {
                            if !my_config.retain_keys {
                                tracing::info!("Clearing cached JWKS");
                                my_jwks.clear();
                            }
                            return failure_message(
                                request.context,
                                format!("Could not retrieve JWKS set: {e}"),
                                StatusCode::INTERNAL_SERVER_ERROR, // XXX: Best error?
                            );
                        }
                    };

                    let jwks = match jwks_opt {
                        Some(k) => k,
                        None => {
                            if !my_config.retain_keys {
                                tracing::info!("Clearing cached JWKS");
                                my_jwks.clear();
                            }
                            return failure_message(
                                request.context,
                                "Could not find JWKS set at the configured location".to_string(),
                                StatusCode::INTERNAL_SERVER_ERROR,
                            );
                        }
                    };

                    // Now let's try to validate our token
                    match jwks.find(&kid) {
                        Some(jwk) => {
                            let decoding_key = match DecodingKey::from_jwk(jwk) {
                                Ok(k) => k,
                                Err(e) => {
                                    return failure_message(
                                        request.context,
                                        format!("Could not create decoding key: {}", e),
                                        StatusCode::INTERNAL_SERVER_ERROR,
                                    );
                                }
                            };

                            let algorithm = match jwk.common.algorithm {
                                Some(a) => a,
                                None => {
                                    return failure_message(
                                        request.context,
                                        "Jwk does not contain an algorithm".to_string(),
                                        StatusCode::INTERNAL_SERVER_ERROR,
                                    );
                                }
                            };

                            let validation = Validation::new(algorithm);

                            let token_data = match decode::<serde_json::Value>(
                                jwt,
                                &decoding_key,
                                &validation,
                            ) {
                                Ok(v) => v,
                                Err(e) => {
                                    return failure_message(
                                        request.context,
                                        format!("Could not create decode JWT: {}", e),
                                        StatusCode::UNAUTHORIZED,
                                    );
                                }
                            };

                            if let Err(e) = request
                                .context
                                .insert("apollo_authentication::JWT::claims", token_data.claims)
                            {
                                return failure_message(
                                    request.context,
                                    format!("Could not insert claims into context: {}", e),
                                    StatusCode::INTERNAL_SERVER_ERROR,
                                );
                            }
                            tracing::info!(?request.context, "request context");
                            Ok(ControlFlow::Continue(request))
                        }
                        None => failure_message(
                            request.context,
                            format!("Could not find kid: {kid} in JWKS set"),
                            StatusCode::UNAUTHORIZED,
                        ),
                    }
                }
            })
            .buffer(20_000)
            .service(service)
            .boxed()
    }
}

// This macro allows us to use it in our plugin registry!
// register_plugin takes a group name, and a plugin name.
//
// In order to keep the plugin names consistent,
// we use using the `Reverse domain name notation`
register_plugin!("apollo", "authentication", AuthenticationPlugin);

#[cfg(test)]
mod tests {

    #[tokio::test]
    async fn load_plugin() {
        let config = serde_json::json!({
            "plugins": {
                "apollo.authentication": {
                    "jwks_url": "http://127.0.0.1:8081"
                }
            }
        });
        // Build a test harness. Usually we'd use this and send requests to
        // it, but in this case it's enough to build the harness to see our
        // output when our service registers.
        let _test_harness = crate::TestHarness::builder()
            .configuration_json(config)
            .unwrap()
            .build_router()
            .await
            .unwrap();
    }
}
