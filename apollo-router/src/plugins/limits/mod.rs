mod layer;
mod limited;

use std::error::Error;

use async_trait::async_trait;
use bytesize::ByteSize;
use http::StatusCode;
use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;
use tower::BoxError;
use tower::ServiceBuilder;
use tower::ServiceExt;

use crate::Context;
use crate::graphql;
use crate::layers::ServiceBuilderExt;
use crate::plugin::Plugin;
use crate::plugin::PluginInit;
use crate::plugins::limits::layer::RequestBodyLimitLayer;
use crate::plugins::limits::layer::RequestSizeLimitError;
use crate::services::router;
use crate::services::router::BoxService;

/// Configuration for operation limits, parser limits, HTTP limits, etc.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields, default)]
#[schemars(rename = "LimitsConfig")]
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

    /// Limit the maximum number of headers of incoming HTTP1 requests. Default is 100.
    ///
    /// If router receives more headers than the buffer size, it responds to the client with
    /// "431 Request Header Fields Too Large".
    pub(crate) http1_max_request_headers: Option<usize>,

    /// Limit the maximum buffer size for the HTTP1 connection.
    ///
    /// Default is ~400kib.
    #[schemars(with = "Option<String>", default)]
    pub(crate) http1_max_request_buf_size: Option<ByteSize>,

    /// For HTTP2, limit the header list to a threshold of bytes. Default is 16kb.
    ///
    /// If router receives more headers than allowed size of the header list, it responds to the client with
    /// "431 Request Header Fields Too Large".
    #[schemars(with = "Option<String>", default)]
    pub(crate) http2_max_headers_list_bytes: Option<ByteSize>,

    /// Limit the depth of nested list fields in introspection queries
    /// to protect avoid generating huge responses. Returns a GraphQL
    /// error with `{ message: "Maximum introspection depth exceeded" }`
    /// when nested fields exceed the limit.
    /// Default: true
    pub(crate) introspection_max_depth: bool,
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
            http1_max_request_headers: None,
            http1_max_request_buf_size: None,
            http2_max_headers_list_bytes: None,
            parser_max_tokens: 15_000,

            // This is `apollo-parser`’s default, which protects against stack overflow
            // but is still very high for "reasonable" queries.
            // https://github.com/apollographql/apollo-rs/blob/apollo-parser%400.7.3/crates/apollo-parser/src/parser/mod.rs#L93-L104
            parser_max_recursion: 500,

            introspection_max_depth: true,
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
        ServiceBuilder::new()
            .map_future_with_request_data(
                |r: &router::Request| r.context.clone(),
                |ctx, f| async { Self::map_error_to_graphql(f.await, ctx) },
            )
            // Here we need to convert to and from the underlying http request types so that we can use existing middleware.
            .map_request(Into::into)
            .map_response(Into::into)
            .layer(RequestBodyLimitLayer::new(
                self.config.http_max_request_bytes,
            ))
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
        // A request can be rejected for exceeding `http_max_request_bytes` in a few ways:
        // 1. The body's Content-Length header exceeds the limit (eager rejection, `BodyTooLarge`).
        // 2. A GET request's query string exceeds the limit (eager rejection, `QueryTooLarge`).
        // 3. The body exceeds the limit while streaming; this surfaces elsewhere in the pipeline
        //    as a raw 413 response rather than one of our own `RequestSizeLimitError`s, and is
        //    always body-related (`BodyTooLarge`) since GET requests have no body to stream.
        // Cases 1 and 2 carry the actual variant that triggered them (downcast below) — always
        // propagate it rather than hardcoding one, or a `QueryTooLarge` rejection gets silently
        // mislabeled as `BodyTooLarge` and reported with the wrong status code.
        match resp {
            Ok(r) => {
                if r.response.status() == StatusCode::PAYLOAD_TOO_LARGE {
                    Ok(RequestSizeLimitError::BodyTooLarge.into_response(ctx))
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

                match root_cause.downcast_ref::<RequestSizeLimitError>() {
                    None => Err(e),
                    Some(err) => Ok((*err).into_response(ctx)),
                }
            }
        }
    }
}

impl RequestSizeLimitError {
    /// 413 for an oversized body; 414 for an oversized URI, per RFC 9110 (the query string is
    /// part of the request-target, not the content, so `Payload Too Large` doesn't apply to it).
    fn status_code(&self) -> StatusCode {
        match self {
            RequestSizeLimitError::BodyTooLarge => StatusCode::PAYLOAD_TOO_LARGE,
            RequestSizeLimitError::QueryTooLarge => StatusCode::URI_TOO_LONG,
        }
    }

    fn into_response(self, ctx: Context) -> router::Response {
        router::Response::error_builder()
            .error(
                graphql::Error::builder()
                    .message(self.to_string())
                    .extension_code("INVALID_GRAPHQL_REQUEST")
                    .extension("details", self.to_string())
                    .build(),
            )
            .status_code(self.status_code())
            .context(ctx)
            .build()
            .unwrap()
    }
}

register_plugin!("apollo", "limits", LimitsPlugin);

#[cfg(test)]
mod test {
    use http::StatusCode;
    use tower::BoxError;

    use crate::plugins::limits::LimitsPlugin;
    use crate::plugins::limits::layer::BodyLimitControl;
    use crate::plugins::test::PluginTestHarness;
    use crate::services::router;

    async fn body_to_string(resp: router::Response) -> String {
        String::from_utf8(
            router::body::into_bytes(resp.response.into_body())
                .await
                .unwrap()
                .to_vec(),
        )
        .unwrap()
    }

    /// Asserts that `resp` has `expected_status` and a GraphQL error message/details matching
    /// `expected_message`.
    async fn assert_rejected(
        resp: Result<router::Response, BoxError>,
        expected_status: StatusCode,
        expected_message: &str,
    ) {
        assert!(resp.is_ok());
        let resp = resp.unwrap();
        assert_eq!(resp.response.status(), expected_status);
        let expected_body = format!(
            r#"{{"errors":[{{"message":"{expected_message}","extensions":{{"details":"{expected_message}","code":"INVALID_GRAPHQL_REQUEST"}}}}]}}"#
        );
        assert_eq!(body_to_string(resp).await, expected_body);
    }

    /// Asserts that `resp` is a 200 with the default empty-response body.
    async fn assert_ok(resp: Result<router::Response, BoxError>) {
        assert!(resp.is_ok());
        let resp = resp.unwrap();
        assert_eq!(resp.response.status(), StatusCode::OK);
        assert_eq!(body_to_string(resp).await, "{}");
    }

    #[tokio::test]
    async fn test_body_content_length_limit_exceeded() {
        let plugin = plugin().await;
        let resp = plugin
            .router_service(|r| async {
                let body = r.router_request.into_body();
                let _ = router::body::into_bytes(body).await?;
                panic!("should have failed to read stream")
            })
            .call(
                router::Request::fake_builder()
                    .body(router::body::from_bytes("This is a test"))
                    .build()
                    .unwrap(),
            )
            .await;
        assert_rejected(
            resp,
            StatusCode::PAYLOAD_TOO_LARGE,
            "Request body payload too large",
        )
        .await;
    }

    #[tokio::test]
    async fn test_body_content_length_limit_ok() {
        let plugin = plugin().await;
        let resp = plugin
            .router_service(|r| async {
                let body = r.router_request.into_body();
                let body = router::body::into_bytes(body).await;
                assert!(body.is_ok());
                Ok(router::Response::fake_builder().build().unwrap())
            })
            .call(
                router::Request::fake_builder()
                    .body(router::body::empty())
                    .build()
                    .unwrap(),
            )
            .await;
        assert_ok(resp).await;
    }

    #[tokio::test]
    async fn test_header_content_length_limit_exceeded() {
        let plugin = plugin().await;
        let resp = plugin
            .router_service(|_| async { panic!("should have rejected request") })
            .call(
                router::Request::fake_builder()
                    .header("Content-Length", "100")
                    .body(router::body::empty())
                    .build()
                    .unwrap(),
            )
            .await;
        assert_rejected(
            resp,
            StatusCode::PAYLOAD_TOO_LARGE,
            "Request body payload too large",
        )
        .await;
    }

    #[tokio::test]
    async fn test_header_content_length_limit_ok() {
        let plugin = plugin().await;
        let resp = plugin
            .router_service(|_| async { Ok(router::Response::fake_builder().build().unwrap()) })
            .call(
                router::Request::fake_builder()
                    .header("Content-Length", "5")
                    .body(router::body::empty())
                    .build()
                    .unwrap(),
            )
            .await;
        assert_ok(resp).await;
    }

    #[tokio::test]
    async fn test_get_query_length_limit_exceeded() {
        let plugin = plugin().await;
        let resp = plugin
            .router_service(|_| async { panic!("should have rejected request") })
            .call(
                router::Request::fake_builder()
                    .method(http::Method::GET)
                    .uri(http::Uri::from_static(
                        "http://example.com/?query=this-query-is-way-too-long-for-the-limit",
                    ))
                    .build()
                    .unwrap(),
            )
            .await;
        assert_rejected(
            resp,
            StatusCode::URI_TOO_LONG,
            "Request query payload too large",
        )
        .await;
    }

    #[tokio::test]
    async fn test_get_query_length_limit_ok() {
        let plugin = plugin().await;
        let resp = plugin
            .router_service(|_| async { Ok(router::Response::fake_builder().build().unwrap()) })
            .call(
                router::Request::fake_builder()
                    .method(http::Method::GET)
                    .uri(http::Uri::from_static("http://example.com/?query=ok"))
                    .build()
                    .unwrap(),
            )
            .await;
        assert_ok(resp).await;
    }

    #[tokio::test]
    async fn test_get_query_length_exactly_at_limit_ok() {
        let plugin = plugin().await;
        // "query=1234" is exactly 10 bytes, matching the fixture's configured limit.
        let resp = plugin
            .router_service(|_| async { Ok(router::Response::fake_builder().build().unwrap()) })
            .call(
                router::Request::fake_builder()
                    .method(http::Method::GET)
                    .uri(http::Uri::from_static("http://example.com/?query=1234"))
                    .build()
                    .unwrap(),
            )
            .await;
        assert_ok(resp).await;
    }

    #[tokio::test]
    async fn test_get_query_length_one_byte_over_limit_exceeded() {
        let plugin = plugin().await;
        // "query=12345" is exactly 11 bytes, one over the fixture's configured limit of 10.
        let resp = plugin
            .router_service(|_| async { panic!("should have rejected request") })
            .call(
                router::Request::fake_builder()
                    .method(http::Method::GET)
                    .uri(http::Uri::from_static("http://example.com/?query=12345"))
                    .build()
                    .unwrap(),
            )
            .await;
        assert_rejected(
            resp,
            StatusCode::URI_TOO_LONG,
            "Request query payload too large",
        )
        .await;
    }

    #[tokio::test]
    async fn test_post_request_query_string_not_checked() {
        // The GET query-length check must not affect POST requests at the full plugin
        // composition level, even if their (unused) query string would exceed the limit; only
        // the body/content-length matters for POST. `layer.rs` has an equivalent test at the raw
        // `RequestBodyLimit` layer; this exercises the same guarantee through `LimitsPlugin`.
        let plugin = plugin().await;
        let resp = plugin
            .router_service(|_| async { Ok(router::Response::fake_builder().build().unwrap()) })
            .call(
                router::Request::fake_builder()
                    .method(http::Method::POST)
                    .uri(http::Uri::from_static(
                        "http://example.com/?query=this-query-is-way-too-long-for-the-limit",
                    ))
                    .body(router::body::empty())
                    .build()
                    .unwrap(),
            )
            .await;
        assert_ok(resp).await;
    }

    #[tokio::test]
    async fn test_non_limit_error_passthrough() {
        // We should not be translating errors that are not limit errors into graphql errors
        let plugin = plugin().await;
        let resp = plugin
            .router_service(|_| async { Err(BoxError::from("error")) })
            .call(
                router::Request::fake_builder()
                    .body(router::body::empty())
                    .build()
                    .unwrap(),
            )
            .await;
        assert!(resp.is_err());
    }

    #[tokio::test]
    async fn test_limits_dynamic_update() {
        let plugin = plugin().await;
        let resp = plugin
            .router_service(|mut r: router::Request| async move {
                // Before we go for the body, we'll update the limit
                let control = r
                    .router_request
                    .extensions_mut()
                    .get::<BodyLimitControl>()
                    .expect("body limit control must have been set")
                    .clone();

                assert_eq!(control.remaining(), 10);
                assert_eq!(control.limit(), 10);
                control.update_limit(100);

                let body = r.router_request.into_body();
                let _ = router::body::into_bytes(body).await?;

                // Now let's check progress
                assert_eq!(control.remaining(), 86);
                Ok(router::Response::fake_builder().build().unwrap())
            })
            .call(
                router::Request::fake_builder()
                    .body(router::body::from_bytes("This is a test"))
                    .build()
                    .unwrap(),
            )
            .await;
        assert!(resp.is_ok());
        let resp = resp.unwrap();
        assert_eq!(resp.response.status(), StatusCode::OK);
        assert_eq!(
            String::from_utf8(
                router::body::into_bytes(resp.response.into_body())
                    .await
                    .unwrap()
                    .to_vec()
            )
            .unwrap(),
            "{}"
        );
    }

    async fn plugin() -> PluginTestHarness<LimitsPlugin> {
        let plugin: PluginTestHarness<LimitsPlugin> = PluginTestHarness::builder()
            .config(include_str!("fixtures/content_length_limit.router.yaml"))
            .build()
            .await
            .expect("test harness");
        plugin
    }
}
