mod layer;
mod limited;

use std::error::Error;

use async_trait::async_trait;
use http::StatusCode;
use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;
use tower::BoxError;
use tower::ServiceBuilder;
use tower::ServiceExt;

use crate::graphql;
use crate::layers::ServiceBuilderExt;
use crate::plugin::Plugin;
use crate::plugin::PluginInit;
use crate::plugins::limits::layer::BodyLimitControl;
use crate::plugins::limits::layer::BodyLimitError;
use crate::plugins::limits::layer::RequestBodyLimitLayer;
use crate::services::router;
use crate::services::router::BoxService;
use crate::Context;

/// Configuration for operation limits, parser limits, HTTP limits, etc.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields, default)]
pub(crate) struct Config {
    /// If set, requests with operations deeper than this maximum
    /// are rejected with a HTTP 400 Bad Request response and GraphQL error with
    /// `"extensions": {"code": "MAX_DEPTH_LIMIT"}`
    ///
    /// Counts depth of an operation, looking at its selection sets,˛
    /// including fields in fragments and inline fragments. The following
    /// example has a depth of 3.
    ///
    /// ```graphql
    /// query getProduct {
    ///   book { # 1
    ///     ...bookDetails
    ///   }
    /// }
    ///
    /// fragment bookDetails on Book {
    ///   details { # 2
    ///     ... on ProductDetailsBook {
    ///       country # 3
    ///     }
    ///   }
    /// }
    /// ```
    pub(crate) max_depth: Option<u32>,

    /// If set, requests with operations higher than this maximum
    /// are rejected with a HTTP 400 Bad Request response and GraphQL error with
    /// `"extensions": {"code": "MAX_DEPTH_LIMIT"}`
    ///
    /// Height is based on simple merging of fields using the same name or alias,
    /// but only within the same selection set.
    /// For example `name` here is only counted once and the query has height 3, not 4:
    ///
    /// ```graphql
    /// query {
    ///     name { first }
    ///     name { last }
    /// }
    /// ```
    ///
    /// This may change in a future version of Apollo Router to do
    /// [full field merging across fragments][merging] instead.
    ///
    /// [merging]: https://spec.graphql.org/October2021/#sec-Field-Selection-Merging]
    pub(crate) max_height: Option<u32>,

    /// If set, requests with operations with more root fields than this maximum
    /// are rejected with a HTTP 400 Bad Request response and GraphQL error with
    /// `"extensions": {"code": "MAX_ROOT_FIELDS_LIMIT"}`
    ///
    /// This limit counts only the top level fields in a selection set,
    /// including fragments and inline fragments.
    pub(crate) max_root_fields: Option<u32>,

    /// If set, requests with operations with more aliases than this maximum
    /// are rejected with a HTTP 400 Bad Request response and GraphQL error with
    /// `"extensions": {"code": "MAX_ALIASES_LIMIT"}`
    pub(crate) max_aliases: Option<u32>,

    /// If set to true (which is the default is dev mode),
    /// requests that exceed a `max_*` limit are *not* rejected.
    /// Instead they are executed normally, and a warning is logged.
    pub(crate) warn_only: bool,

    /// Limit recursion in the GraphQL parser to protect against stack overflow.
    /// default: 500
    pub(crate) parser_max_recursion: usize,

    /// Limit the number of tokens the GraphQL parser processes before aborting.
    pub(crate) parser_max_tokens: usize,

    /// Limit the size of incoming HTTP requests read from the network,
    /// to protect against running out of memory. Default: 2000000 (2 MB)
    pub(crate) http_max_request_bytes: usize,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            // These limits are opt-in
            max_depth: None,
            max_height: None,
            max_root_fields: None,
            max_aliases: None,
            warn_only: false,
            http_max_request_bytes: 2_000_000,
            parser_max_tokens: 15_000,

            // This is `apollo-parser`’s default, which protects against stack overflow
            // but is still very high for "reasonable" queries.
            // https://github.com/apollographql/apollo-rs/blob/apollo-parser%400.7.3/crates/apollo-parser/src/parser/mod.rs#L93-L104
            parser_max_recursion: 500,
        }
    }
}

struct LimitsPlugin {
    config: Config,
}

#[async_trait]
impl Plugin for LimitsPlugin {
    type Config = Config;

    async fn new(init: PluginInit<Self::Config>) -> Result<Self, BoxError>
    where
        Self: Sized,
    {
        Ok(LimitsPlugin {
            config: init.config,
        })
    }

    fn router_service(&self, service: BoxService) -> BoxService {
        let control = BodyLimitControl::new(self.config.http_max_request_bytes);
        let control_for_context = control.clone();
        ServiceBuilder::new()
            .map_request(move |r: router::Request| {
                let control_for_context = control_for_context.clone();
                r.context
                    .extensions()
                    .with_lock(|mut lock| lock.insert(control_for_context));
                r
            })
            .map_future_with_request_data(
                |r: &router::Request| r.context.clone(),
                |ctx, f| async { Self::map_error_to_graphql(f.await, ctx) },
            )
            // Here we need to convert to and from the underlying http request types so that we can use existing middleware.
            .map_request(Into::into)
            .map_response(Into::into)
            .layer(RequestBodyLimitLayer::new(control))
            .map_request(Into::into)
            .map_response(Into::into)
            .service(service)
            .boxed()
    }
}

impl LimitsPlugin {
    fn map_error_to_graphql(
        resp: Result<router::Response, BoxError>,
        ctx: Context,
    ) -> Result<router::Response, BoxError> {
        // There are two ways we can get a payload too large error:
        // 1. The request body is too large and detected via content length header
        // 2. The request body is and it failed at some other point in the pipeline.
        // We expect that other pipeline errors will have wrapped the source error rather than throwing it away.
        match resp {
            Ok(r) => {
                if r.response.status() == StatusCode::PAYLOAD_TOO_LARGE {
                    Self::increment_legacy_metric();
                    Ok(BodyLimitError::PayloadTooLarge.into_response(ctx))
                } else {
                    Ok(r)
                }
            }
            Err(e) => {
                // Getting the root cause is a bit fiddly
                let mut root_cause: &dyn Error = e.as_ref();
                while let Some(cause) = root_cause.source() {
                    root_cause = cause;
                }

                match root_cause.downcast_ref::<BodyLimitError>() {
                    None => Err(e),
                    Some(_) => {
                        Self::increment_legacy_metric();
                        Ok(BodyLimitError::PayloadTooLarge.into_response(ctx))
                    }
                }
            }
        }
    }

    fn increment_legacy_metric() {
        // Remove this eventually
        // This is already handled by the telemetry plugin via the http.server.request metric.
        u64_counter!(
            "apollo_router_http_requests_total",
            "Total number of HTTP requests made.",
            1,
            status = StatusCode::PAYLOAD_TOO_LARGE.as_u16() as i64,
            error = BodyLimitError::PayloadTooLarge.to_string()
        );
    }
}

impl BodyLimitError {
    fn into_response(self, ctx: Context) -> router::Response {
        match self {
            BodyLimitError::PayloadTooLarge => router::Response::error_builder()
                .error(
                    graphql::Error::builder()
                        .message(self.to_string())
                        .extension_code("INVALID_GRAPHQL_REQUEST")
                        .extension("details", self.to_string())
                        .build(),
                )
                .status_code(StatusCode::PAYLOAD_TOO_LARGE)
                .context(ctx)
                .build()
                .unwrap(),
        }
    }
}

register_plugin!("apollo", "limits", LimitsPlugin);

#[cfg(test)]
mod test {
    use http::StatusCode;
    use tower::BoxError;

    use crate::plugins::limits::layer::BodyLimitControl;
    use crate::plugins::limits::LimitsPlugin;
    use crate::plugins::test::PluginTestHarness;
    use crate::services::router;
    use crate::services::router::body::get_body_bytes;

    #[tokio::test]
    async fn test_body_content_length_limit_exceeded() {
        let plugin = plugin().await;
        let resp = plugin
            .call_router(
                router::Request::fake_builder()
                    .body("This is a test")
                    .build()
                    .unwrap(),
                |r| async {
                    let body = r.router_request.into_body();
                    let _ = get_body_bytes(body).await?;
                    panic!("should have failed to read stream")
                },
            )
            .await;
        assert!(resp.is_ok());
        let resp = resp.unwrap();
        assert_eq!(resp.response.status(), StatusCode::PAYLOAD_TOO_LARGE);
        assert_eq!(
            String::from_utf8(
                get_body_bytes(resp.response.into_body())
                    .await
                    .unwrap()
                    .to_vec()
            )
            .unwrap(),
            "{\"errors\":[{\"message\":\"Request body payload too large\",\"extensions\":{\"details\":\"Request body payload too large\",\"code\":\"INVALID_GRAPHQL_REQUEST\"}}]}"
        );
    }

    #[tokio::test]
    async fn test_body_content_length_limit_ok() {
        let plugin = plugin().await;
        let resp = plugin
            .call_router(
                router::Request::fake_builder().body("").build().unwrap(),
                |r| async {
                    let body = r.router_request.into_body();
                    let body = get_body_bytes(body).await;
                    assert!(body.is_ok());
                    Ok(router::Response::fake_builder().build().unwrap())
                },
            )
            .await;

        assert!(resp.is_ok());
        let resp = resp.unwrap();
        assert_eq!(resp.response.status(), StatusCode::OK);
        assert_eq!(
            String::from_utf8(
                get_body_bytes(resp.response.into_body())
                    .await
                    .unwrap()
                    .to_vec()
            )
            .unwrap(),
            "{}"
        );
    }

    #[tokio::test]
    async fn test_header_content_length_limit_exceeded() {
        let plugin = plugin().await;
        let resp = plugin
            .call_router(
                router::Request::fake_builder()
                    .header("Content-Length", "100")
                    .body("")
                    .build()
                    .unwrap(),
                |_| async { panic!("should have rejected request") },
            )
            .await;
        assert!(resp.is_ok());
        let resp = resp.unwrap();
        assert_eq!(resp.response.status(), StatusCode::PAYLOAD_TOO_LARGE);
        assert_eq!(
            String::from_utf8(
                get_body_bytes(resp.response.into_body())
                    .await
                    .unwrap()
                    .to_vec()
            )
            .unwrap(),
            "{\"errors\":[{\"message\":\"Request body payload too large\",\"extensions\":{\"details\":\"Request body payload too large\",\"code\":\"INVALID_GRAPHQL_REQUEST\"}}]}"
        );
    }

    #[tokio::test]
    async fn test_header_content_length_limit_ok() {
        let plugin = plugin().await;
        let resp = plugin
            .call_router(
                router::Request::fake_builder()
                    .header("Content-Length", "5")
                    .body("")
                    .build()
                    .unwrap(),
                |_| async { Ok(router::Response::fake_builder().build().unwrap()) },
            )
            .await;
        assert!(resp.is_ok());
        let resp = resp.unwrap();
        assert_eq!(resp.response.status(), StatusCode::OK);
        assert_eq!(
            String::from_utf8(
                get_body_bytes(resp.response.into_body())
                    .await
                    .unwrap()
                    .to_vec()
            )
            .unwrap(),
            "{}"
        );
    }

    #[tokio::test]
    async fn test_non_limit_error_passthrough() {
        // We should not be translating errors that are not limit errors into graphql errors
        let plugin = plugin().await;
        let resp = plugin
            .call_router(
                router::Request::fake_builder().body("").build().unwrap(),
                |_| async { Err(BoxError::from("error")) },
            )
            .await;
        assert!(resp.is_err());
    }

    #[tokio::test]
    async fn test_limits_dynamic_update() {
        let plugin = plugin().await;
        let resp = plugin
            .call_router(
                router::Request::fake_builder()
                    .body("This is a test")
                    .build()
                    .unwrap(),
                |r| async move {
                    // Before we go for the body, we'll update the limit
                    r.context.extensions().with_lock(|lock| {
                        let control: &BodyLimitControl =
                            lock.get().expect("mut have body limit control");
                        assert_eq!(control.remaining(), 10);
                        assert_eq!(control.limit(), 10);
                        control.update_limit(100);
                    });
                    let body = r.router_request.into_body();
                    let _ = get_body_bytes(body).await?;

                    // Now let's check progress
                    r.context.extensions().with_lock(|lock| {
                        let control: &BodyLimitControl =
                            lock.get().expect("mut have body limit control");
                        assert_eq!(control.remaining(), 86);
                    });
                    Ok(router::Response::fake_builder().build().unwrap())
                },
            )
            .await;
        assert!(resp.is_ok());
        let resp = resp.unwrap();
        assert_eq!(resp.response.status(), StatusCode::OK);
        assert_eq!(
            String::from_utf8(
                get_body_bytes(resp.response.into_body())
                    .await
                    .unwrap()
                    .to_vec()
            )
            .unwrap(),
            "{}"
        );
    }

    async fn plugin() -> PluginTestHarness<LimitsPlugin> {
        let plugin: PluginTestHarness<LimitsPlugin> = PluginTestHarness::new(
            Some(include_str!("fixtures/content_length_limit.router.yaml")),
            None,
        )
        .await;
        plugin
    }
}
