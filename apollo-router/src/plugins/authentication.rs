//! Authentication plugin
// With regards to ELv2 licensing, this entire file is license key functionality

use std::ops::ControlFlow;
use std::str::FromStr;
use std::sync::Arc;

use deduplicate::Deduplicate;
use deduplicate::DeduplicateFuture;
use http::StatusCode;
use jsonwebtoken::decode_header;
use jsonwebtoken::jwk::JwkSet;
use schemars::JsonSchema;
use serde::Deserialize;
use tower::BoxError;
use tower::ServiceBuilder;
use tower::ServiceExt;
use url::Url;

use crate::graphql;
use crate::layers::ServiceBuilderExt;
use crate::plugin::Plugin;
use crate::plugin::PluginInit;
use crate::register_plugin;
use crate::services::router;
use crate::Context;

pub(crate) const AUTHENTICATION_SPAN_NAME: &str = "authentication plugin";

type SharedDeduplicate = Arc<
    Deduplicate<
        Box<dyn Fn(String) -> DeduplicateFuture<JwkSet> + Send + Sync + 'static>,
        String,
        JwkSet,
    >,
>;

struct AuthenticationPlugin {
    configuration: Conf,
    jwks: SharedDeduplicate,
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
        let getter: Box<dyn Fn(String) -> DeduplicateFuture<JwkSet> + Send + Sync + 'static> =
            Box::new(|s_url: String| -> DeduplicateFuture<JwkSet> {
                let url: Url = Url::from_str(&s_url).expect("fix later");
                let fut = if url.scheme() == "file" {
                    // TODO: Write code to load JwkSet from disk
                    todo!()
                } else {
                    async {
                        let jwks: JwkSet = serde_json::from_value(
                            reqwest::get(url).await.ok()?.json().await.ok()?,
                        )
                        .ok()?;
                        Some(jwks)
                    }
                };
                Box::pin(fut)
            });
        let deduplicator = Deduplicate::with_capacity(getter, 1);
        Ok(AuthenticationPlugin {
            configuration: init.config,
            jwks: Arc::new(deduplicator),
        })
    }

    fn router_service(&self, service: router::BoxService) -> router::BoxService {
        let request_full_config = self.configuration.clone();
        let request_jwks = self.jwks.clone();

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

                    // GET THE JWKS here
                    let jwks_opt = match my_jwks.get(my_config.jwks_url).await {
                        Ok(k) => k,
                        Err(e) => {
                            if !my_config.retain_keys {
                                tracing::info!("Could not retrieve JWKS, clearing cached JWKS");
                                // TODO: Doesn't work until I fix deduplicate::clear()
                                // my_jwks.clear();
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
                            return failure_message(
                                request.context,
                                "Did not receive valid JWKS set".to_string(),
                                StatusCode::INTERNAL_SERVER_ERROR,
                            );
                        }
                    };

                    // Now let's try to validate our token
                    match jwks.find(&kid) {
                        Some(_jwk) => {
                            //todo!()
                            // XXX: NEED TO RESUME HERE WITH VALIDATION AND CLAIM POPULATION
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
