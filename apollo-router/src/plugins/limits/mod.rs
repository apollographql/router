pub(crate) mod http2_header_list;
mod layer;
mod limited;
pub(crate) mod measure_operation_limits;
pub(crate) mod operation_limits;
pub(crate) mod operation_limits_layer;
pub(crate) mod response_size_limit;

use std::error::Error;

use async_trait::async_trait;
use bytesize::ByteSize;
use http::StatusCode;
use http::header::CONTENT_TYPE;
pub(crate) use layer::BodyLimitControl;
use mime::APPLICATION_JSON;
use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;
use tower::BoxError;
use tower::ServiceBuilder;
use tower::ServiceExt;
use tower::util::option_layer;

use crate::Context;
use crate::configuration::connector::ConnectorConfiguration;
use crate::configuration::subgraph::SubgraphConfiguration;
use crate::graphql;
use crate::layers::OptionLayer;
use crate::layers::ServiceBuilderExt;
use crate::pipeline::HttpClientTarget;
use crate::plugin::PluginInit;
use crate::plugin::PluginPrivate;
use crate::plugins::limits::layer::RequestBodyLimitLayer;
use crate::plugins::limits::layer::RequestSizeLimitError;
use crate::plugins::limits::measure_operation_limits::MeasureOperationLimitsLayer;
use crate::plugins::limits::operation_limits_layer::EnforceOperationLimitsLayer;
use crate::plugins::limits::response_size_limit::ResponseSizeLimitLayer;
use crate::services::query_parsing::recursive_selections_limit::LimitRecursiveSelectionLayer;
use crate::services::router;
use crate::uplink::license_enforcement::AllowedFeature;
use crate::uplink::license_enforcement::LicensedSetting;

/// Configuration for operation limits, parser limits, HTTP limits, etc.
#[apollo_configuration::configuration]
#[derive(Serialize)]
#[schemars(rename = "LimitsConfig")]
pub(crate) struct Config {
    /// Limits that apply to inbound requests to the router.
    pub(crate) router: RouterLimitsConfig,

    /// Limits that apply to outbound subgraph responses.
    // Generic with a hand-written merging Deserialize, which the macro can't express.
    #[config(skip_validate)]
    pub(crate) subgraph: SubgraphConfiguration<SubgraphLimits>,

    /// Limits that apply to outbound connector responses.
    // Generic with a hand-written merging Deserialize, which the macro can't express.
    #[config(skip_validate)]
    pub(crate) connector: ConnectorConfiguration<ConnectorLimits>,
}

/// Limits that apply to inbound requests to the router.
#[apollo_configuration::configuration]
#[derive(Serialize)]
#[schemars(rename = "RouterLimitsConfig")]
pub(crate) struct RouterLimitsConfig {
    /// If set, requests with operations deeper than this maximum
    /// are rejected with a HTTP 400 Bad Request response and GraphQL error with
    /// `"extensions": {"code": "MAX_DEPTH_LIMIT"}`
    ///
    /// Counts depth of an operation, looking at its selection sets,
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
    /// `"extensions": {"code": "MAX_HEIGHT_LIMIT"}`
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

    /// Limit the total number of selections encountered when recursively expanding
    /// fragment spreads in an operation. This protects against extremely large or
    /// deeply nested operations that could consume excessive resources during
    /// query planning. Default: 10000000 (10 million).
    #[config(default = 10_000_000)]
    pub(crate) max_recursive_selections: u32,

    /// If set to true (which is the default is dev mode),
    /// requests that exceed a `max_*` limit are *not* rejected.
    /// Instead they are executed normally, and a warning is logged.
    pub(crate) warn_only: bool,

    /// Limit recursion in the GraphQL parser to protect against stack overflow.
    /// default: 500
    // This is `apollo-parser`'s default, which protects against stack overflow
    // but is still very high for "reasonable" queries.
    // https://github.com/apollographql/apollo-rs/blob/apollo-parser%400.7.3/crates/apollo-parser/src/parser/mod.rs#L93-L104
    #[config(default = 500)]
    pub(crate) parser_max_recursion: usize,

    /// Limit the number of tokens the GraphQL parser processes before aborting.
    #[config(default = 15_000)]
    pub(crate) parser_max_tokens: usize,

    /// Limit the size of incoming HTTP requests read from the network,
    /// to protect against running out of memory. Default: 2000000 (2 MB)
    #[config(default = 2_000_000)]
    pub(crate) http_max_request_bytes: usize,

    /// Limit the maximum number of headers of incoming HTTP1 requests. Default is 100.
    ///
    /// If router receives more headers than the buffer size, it responds to the client with
    /// "431 Request Header Fields Too Large".
    pub(crate) http1_max_request_headers: Option<usize>,

    /// Limit the maximum buffer size for the HTTP1 connection.
    ///
    /// Default is ~400kib.
    // ByteSize has no Validate impl.
    #[config(skip_validate)]
    #[schemars(with = "Option<String>", default)]
    pub(crate) http1_max_request_buf_size: Option<ByteSize>,

    /// For HTTP2, limit the header list to a threshold of bytes. Default is 16kib.
    ///
    /// If router receives more headers than allowed size of the header list, it responds to the client with
    /// "431 Request Header Fields Too Large".
    // ByteSize has no Validate impl.
    #[config(skip_validate)]
    #[schemars(with = "Option<String>", default)]
    pub(crate) http2_max_headers_list_bytes: Option<ByteSize>,

    /// Limit the depth of nested list fields in introspection queries
    /// to protect avoid generating huge responses. Returns a GraphQL
    /// error with `{ message: "Maximum introspection depth exceeded" }`
    /// when nested fields exceed the limit.
    /// Default: true
    #[config(default = true)]
    pub(crate) introspection_max_depth: bool,
}

/// The HTTP/2 header-list limit when `http2_max_headers_list_bytes` is unset.
///
/// This is hyper's own default. The Router names it so it doesn't depend on a private hyper
/// constant to know which limit applies.
pub(crate) const DEFAULT_HTTP2_MAX_HEADERS_LIST_BYTES: ByteSize = ByteSize::kib(16);

impl RouterLimitsConfig {
    /// The HTTP/2 header-list limit in effect: the configured value or the default.
    pub(crate) fn http2_header_list_limit(&self) -> ByteSize {
        self.http2_max_headers_list_bytes
            .unwrap_or(DEFAULT_HTTP2_MAX_HEADERS_LIST_BYTES)
    }
}

/// Per-subgraph response size limits.
#[derive(Debug, Default, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields, default)]
#[schemars(rename = "SubgraphLimits")]
pub(crate) struct SubgraphLimits {
    /// Limit the size of incoming subgraph response bodies read from the network,
    /// to protect against running out of memory. Default: no limit.
    #[schemars(with = "Option<String>", default)]
    pub(crate) http_max_response_size: Option<ByteSize>,
}

/// Per-connector-source response size limits.
#[derive(Debug, Clone, Default, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields, default)]
#[schemars(rename = "ConnectorLimits")]
pub(crate) struct ConnectorLimits {
    /// Limit the size of incoming connector response bodies read from the network,
    /// to protect against running out of memory. Default: no limit.
    #[schemars(with = "Option<String>", default)]
    pub(crate) http_max_response_size: Option<ByteSize>,
}

impl Config {
    /// `subgraph_name`'s response size limit in bytes: its own, or the one for all subgraphs.
    fn subgraph_response_size_limit(&self, subgraph_name: &str) -> Option<usize> {
        let subgraph_limit = self
            .subgraph
            .subgraphs
            .get(subgraph_name)
            .and_then(|s| s.http_max_response_size);
        let limit = subgraph_limit.or(self.subgraph.all.http_max_response_size)?;
        limit.as_u64().try_into().ok()
    }

    /// `source_name`'s response size limit in bytes: its own, or the one for all connector sources.
    fn connector_response_size_limit(&self, source_name: &str) -> Option<usize> {
        let source_limit = self
            .connector
            .sources
            .get(source_name)
            .and_then(|s| s.http_max_response_size);
        let limit = source_limit.or(self.connector.all.http_max_response_size)?;
        limit.as_u64().try_into().ok()
    }
}

pub(crate) struct LimitsPlugin {
    config: Config,
}

impl LimitsPlugin {
    /// The HTTP/2 header-list check for graph requests, applied in the router service stack ahead
    /// of every plugin's [`PluginPrivate::router_service`].
    pub(crate) fn router_header_list_limit_layer(
        &self,
    ) -> http2_header_list::RouterHeaderListLimitLayer {
        http2_header_list::RouterHeaderListLimitLayer::new(
            self.config.router.http2_header_list_limit(),
        )
    }

    /// The HTTP/2 header-list check for plugin endpoints, which don't go through the router
    /// service stack.
    pub(crate) fn endpoint_header_list_limit_layer(
        &self,
    ) -> http2_header_list::EndpointHeaderListLimitLayer {
        http2_header_list::EndpointHeaderListLimitLayer::new(
            self.config.router.http2_header_list_limit(),
        )
    }

    pub(crate) fn recursive_selections_limit_layer(&self) -> LimitRecursiveSelectionLayer {
        LimitRecursiveSelectionLayer::new(
            self.config.router.max_recursive_selections,
            self.config.router.warn_only,
        )
    }

    pub(crate) fn measure_operation_limits_layer(&self) -> MeasureOperationLimitsLayer {
        MeasureOperationLimitsLayer
    }

    pub(crate) fn enforce_operation_limits_layer(&self) -> EnforceOperationLimitsLayer {
        EnforceOperationLimitsLayer::new(&self.config.router)
    }

    /// The response size check for the HTTP client calling `target`, present only when the target
    /// has a limit of its own or one for all subgraphs or all connector sources.
    pub(crate) fn response_size_limit_layer(
        &self,
        target: HttpClientTarget<'_>,
    ) -> OptionLayer<ResponseSizeLimitLayer> {
        let limit = match target {
            HttpClientTarget::Subgraph(name) => self.config.subgraph_response_size_limit(name),
            HttpClientTarget::ConnectorSource(source) => {
                self.config.connector_response_size_limit(source)
            }
        };
        option_layer(limit.map(|limit| ResponseSizeLimitLayer::new(target, limit)))
    }
}

#[async_trait]
impl PluginPrivate for LimitsPlugin {
    type Config = Config;

    fn licensed_features(
        config: &Self::Config,
        _supergraph: &apollo_compiler::Schema,
    ) -> Vec<LicensedSetting> {
        // Parser limits such as `parser_max_recursion` only configure the GraphQL parser and are
        // not licensed.
        let router = &config.router;
        [
            (router.max_depth, "limits.router.max_depth"),
            (router.max_height, "limits.router.max_height"),
            (router.max_root_fields, "limits.router.max_root_fields"),
            (router.max_aliases, "limits.router.max_aliases"),
        ]
        .into_iter()
        .filter(|(limit, _)| limit.is_some())
        .map(|(_, path)| LicensedSetting {
            feature: AllowedFeature::RequestLimits,
            path,
        })
        .collect()
    }

    async fn new(init: PluginInit<Self::Config>) -> Result<Self, BoxError>
    where
        Self: Sized,
    {
        Ok(LimitsPlugin {
            config: init.config,
        })
    }

    fn router_service(&self, service: router::BoxCloneService) -> router::BoxCloneService {
        ServiceBuilder::new()
            .map_future_with_request_data(
                |r: &router::Request| r.context.clone(),
                |ctx, f| async { Self::map_error_to_graphql(f.await, ctx) },
            )
            // Here we need to convert to and from the underlying http request types so that we can use existing middleware.
            .map_request(Into::into)
            .map_response(Into::into)
            .layer(RequestBodyLimitLayer::new(
                self.config.router.http_max_request_bytes,
            ))
            .map_request(Into::into)
            .map_response(Into::into)
            .service(service)
            .boxed_clone()
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
    /// part of the request-target, not the content, so `Payload Too Large` doesn't apply to it);
    /// 431 for an oversized header list, per RFC 6585.
    fn status_code(&self) -> StatusCode {
        match self {
            RequestSizeLimitError::BodyTooLarge => StatusCode::PAYLOAD_TOO_LARGE,
            RequestSizeLimitError::QueryTooLarge => StatusCode::URI_TOO_LONG,
            RequestSizeLimitError::OversizedHeaderList => {
                StatusCode::REQUEST_HEADER_FIELDS_TOO_LARGE
            }
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
            .header(CONTENT_TYPE, APPLICATION_JSON.essence_str())
            .context(ctx)
            .build()
            .unwrap()
    }
}

register_private_plugin!("apollo", "limits", LimitsPlugin, mandatory);

#[cfg(test)]
impl From<SubgraphConfiguration<SubgraphLimits>> for Config {
    fn from(subgraph: SubgraphConfiguration<SubgraphLimits>) -> Self {
        Self {
            subgraph,
            ..Self::default()
        }
    }
}

#[cfg(test)]
impl From<ConnectorConfiguration<ConnectorLimits>> for Config {
    fn from(connector: ConnectorConfiguration<ConnectorLimits>) -> Self {
        Self {
            connector,
            ..Self::default()
        }
    }
}

#[cfg(test)]
mod test {
    use http::StatusCode;
    use tower::BoxError;
    use tower::ServiceBuilder;
    use tower::ServiceExt as _;
    use tower::service_fn;

    use crate::Context;
    use crate::metrics::FutureMetricsExt;
    use crate::pipeline::HttpClientTarget;
    use crate::plugins::limits::LimitsPlugin;
    use crate::plugins::limits::layer::BodyLimitControl;
    use crate::plugins::telemetry::Telemetry;
    use crate::plugins::test::PluginTestHarness;
    use crate::services::http::HttpRequest;
    use crate::services::http::HttpResponse;
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
        assert_eq!(
            resp.response.headers().get(http::header::CONTENT_TYPE),
            Some(&http::HeaderValue::from_static("application/json"))
        );
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

    /// A request whose header list is just over the default 16 KiB HTTP/2 limit.
    fn oversized_header_list_request(version: http::Version) -> router::Request {
        http::Request::builder()
            .version(version)
            .uri("http://localhost:4000/")
            .header("cookie", "v".repeat(16 * 1024))
            .body(router::body::empty())
            .unwrap()
            .into()
    }

    /// A router service that fails the test if a request reaches it.
    fn unreachable_router_service() -> router::BoxCloneService {
        service_fn(|_| async { panic!("the request should have been rejected") }).boxed_clone()
    }

    #[tokio::test]
    async fn test_http2_header_list_limit_exceeded() {
        let plugin = plugin().await;
        let resp = ServiceBuilder::new()
            .layer(plugin.router_header_list_limit_layer())
            .service(unreachable_router_service())
            .oneshot(oversized_header_list_request(http::Version::HTTP_2))
            .await;
        assert_rejected(
            resp,
            StatusCode::REQUEST_HEADER_FIELDS_TOO_LARGE,
            "Request header fields too large",
        )
        .await;
    }

    #[tokio::test]
    async fn test_http2_header_list_limit_ignores_http1() {
        let plugin = plugin().await;
        let resp = ServiceBuilder::new()
            .layer(plugin.router_header_list_limit_layer())
            .service_fn(|_: router::Request| async {
                Ok::<_, BoxError>(router::Response::fake_builder().build().unwrap())
            })
            .oneshot(oversized_header_list_request(http::Version::HTTP_11))
            .await;
        assert_ok(resp).await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_http2_header_list_limit_exceeded_is_recorded() {
        async {
            let telemetry: PluginTestHarness<Telemetry> = PluginTestHarness::builder()
                .config(
                    r#"
                    telemetry:
                      instrumentation:
                        instruments:
                          router:
                            http.server.request.duration: true
                    "#,
                )
                .build()
                .await
                .expect("test harness");
            let limits: PluginTestHarness<LimitsPlugin> = PluginTestHarness::builder()
                .build()
                .await
                .expect("test harness");
            // Telemetry wraps the header-list check, as it does in the router pipeline.
            let service = ServiceBuilder::new()
                .layer(telemetry.instrument_router_layer())
                .layer(limits.router_header_list_limit_layer())
                .service(unreachable_router_service());

            let resp = service
                .oneshot(oversized_header_list_request(http::Version::HTTP_2))
                .await
                .unwrap();
            assert_eq!(
                resp.response.status(),
                StatusCode::REQUEST_HEADER_FIELDS_TOO_LARGE
            );
            assert_histogram_count!(
                "http.server.request.duration",
                1,
                "error.type" = "Request Header Fields Too Large",
                "http.request.method" = "GET",
                "http.response.status_code" = 431
            );
        }
        .with_metrics()
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

    /// Check configuration for subgraph_response_limit
    mod subgraph_response_limit {
        use bytesize::ByteSize;

        use crate::configuration::subgraph::SubgraphConfiguration;
        use crate::plugins::limits::Config;
        use crate::plugins::limits::SubgraphLimits;

        #[test]
        fn get_response_limit_no_config() {
            let subgraph_config = SubgraphConfiguration::<SubgraphLimits>::default();
            let config: Config = subgraph_config.into();
            assert_eq!(config.subgraph_response_size_limit("products"), None);
        }

        #[test]
        fn get_response_limit_all() {
            let mut subgraph_config = SubgraphConfiguration::<SubgraphLimits>::default();
            subgraph_config.all.http_max_response_size = Some(ByteSize::kb(1));

            let config: Config = subgraph_config.into();
            assert_eq!(config.subgraph_response_size_limit("products"), Some(1000));
            assert_eq!(config.subgraph_response_size_limit("reviews"), Some(1000));
        }

        #[test]
        fn get_response_limit_subgraph_specific() {
            let mut subgraph_config = SubgraphConfiguration::<SubgraphLimits>::default();
            subgraph_config.subgraphs.insert(
                "products".to_string(),
                SubgraphLimits {
                    http_max_response_size: Some(ByteSize::b(512)),
                },
            );

            let config: Config = subgraph_config.into();
            assert_eq!(config.subgraph_response_size_limit("products"), Some(512));
            assert_eq!(config.subgraph_response_size_limit("reviews"), None);
        }

        #[test]
        fn get_response_limit_subgraph_overrides_all() {
            let mut subgraph_config = SubgraphConfiguration::<SubgraphLimits>::default();
            subgraph_config.all.http_max_response_size = Some(ByteSize::kib(1));
            subgraph_config.subgraphs.insert(
                "products".to_string(),
                SubgraphLimits {
                    http_max_response_size: Some(ByteSize::b(500)),
                },
            );
            subgraph_config.subgraphs.insert(
                "reviews".to_string(),
                SubgraphLimits {
                    http_max_response_size: None,
                },
            );

            let config: Config = subgraph_config.into();
            // per-subgraph override wins
            assert_eq!(config.subgraph_response_size_limit("products"), Some(500));
            // fallback to all despite having an entry in the map
            assert_eq!(config.subgraph_response_size_limit("reviews"), Some(1024));
        }
    }

    /// Check configuration for connector_response_limit
    mod connector_response_limit {
        use bytesize::ByteSize;

        use crate::configuration::connector::ConnectorConfiguration;
        use crate::plugins::limits::Config;
        use crate::plugins::limits::ConnectorLimits;

        #[test]
        fn get_response_limit_no_config() {
            let connector_config = ConnectorConfiguration::<ConnectorLimits>::default();
            let config: Config = connector_config.into();
            assert_eq!(config.connector_response_size_limit("products.rest"), None);
        }

        #[test]
        fn get_response_limit_all() {
            let mut connector_config = ConnectorConfiguration::<ConnectorLimits>::default();
            connector_config.all.http_max_response_size = Some(ByteSize::kb(1));

            let config: Config = connector_config.into();
            assert_eq!(
                config.connector_response_size_limit("products.rest"),
                Some(1000)
            );
            assert_eq!(
                config.connector_response_size_limit("reviews.api"),
                Some(1000)
            );
        }

        #[test]
        fn get_response_limit_subgraph_specific() {
            let mut connector_config = ConnectorConfiguration::<ConnectorLimits>::default();
            connector_config.sources.insert(
                "products.rest".to_string(),
                ConnectorLimits {
                    http_max_response_size: Some(ByteSize::b(512)),
                },
            );

            let config: Config = connector_config.into();
            assert_eq!(
                config.connector_response_size_limit("products.rest"),
                Some(512)
            );
            assert_eq!(config.connector_response_size_limit("reviews.api"), None);
        }

        #[test]
        fn get_response_limit_subgraph_overrides_all() {
            let mut connector_config = ConnectorConfiguration::<ConnectorLimits>::default();
            connector_config.all.http_max_response_size = Some(ByteSize::kib(1));
            connector_config.sources.insert(
                "products.rest".to_string(),
                ConnectorLimits {
                    http_max_response_size: Some(ByteSize::b(500)),
                },
            );
            connector_config.sources.insert(
                "reviews.api".to_string(),
                ConnectorLimits {
                    http_max_response_size: None,
                },
            );

            let config: Config = connector_config.into();
            // per-subgraph override wins
            assert_eq!(
                config.connector_response_size_limit("products.rest"),
                Some(500)
            );
            // fallback to all despite having an entry in the map
            assert_eq!(
                config.connector_response_size_limit("reviews.api"),
                Some(1024)
            );
        }
    }

    // --- LimitsPlugin::response_size_limit_layer ---

    async fn read_oversized_response(
        config: &str,
        target: HttpClientTarget<'_>,
    ) -> Result<bytes::Bytes, BoxError> {
        let plugin: PluginTestHarness<LimitsPlugin> = PluginTestHarness::builder()
            .config(config)
            .build()
            .await
            .expect("test harness");

        let (mock, mut handle) = tower_test::mock::pair::<HttpRequest, HttpResponse>();
        let driver = tokio::spawn(async move {
            let (_req, responder) = handle.next_request().await.unwrap();
            responder.send_response(HttpResponse {
                http_response: http::Response::builder()
                    .body(router::body::from_bytes(vec![0; 10_000]))
                    .unwrap(),
                context: Context::new(),
            });
        });

        let service = ServiceBuilder::new()
            .layer(plugin.response_size_limit_layer(target))
            .service(mock);

        let request = HttpRequest {
            http_request: http::Request::builder()
                .body(router::body::empty())
                .unwrap(),
            context: Context::new(),
        };

        let response = service.oneshot(request).await.unwrap();
        let body = router::body::into_bytes(response.http_response.into_body()).await;
        crate::plugin::test::await_mock_driver(driver).await;
        body.map_err(BoxError::from)
    }

    async fn read_oversized_products_response(config: &str) -> Result<bytes::Bytes, BoxError> {
        read_oversized_response(config, HttpClientTarget::Subgraph("products")).await
    }

    #[tokio::test]
    async fn subgraph_response_size_layer_left_out_without_limits() {
        let body = read_oversized_products_response("limits: {}").await;
        assert_eq!(body.expect("no limit is checked").len(), 10_000);
    }

    #[tokio::test]
    async fn subgraph_response_size_layer_applied_with_a_limit_for_all() {
        let body = read_oversized_products_response(indoc::indoc! {"
            limits:
              subgraph:
                all:
                  http_max_response_size: 1024b
        "})
        .await;
        assert_eq!(
            body.expect_err("the limit for all subgraphs is enforced")
                .to_string(),
            "subgraph response body exceeded limit of 1024 bytes"
        );
    }

    #[tokio::test]
    async fn subgraph_response_size_layer_left_out_for_subgraphs_without_their_own_limit() {
        let body = read_oversized_products_response(indoc::indoc! {"
            limits:
              subgraph:
                subgraphs:
                  reviews:
                    http_max_response_size: 1024b
        "})
        .await;
        assert_eq!(body.expect("products has no limit").len(), 10_000);
    }

    #[tokio::test]
    async fn connector_response_size_layer_applied_with_its_source_limit() {
        let body = read_oversized_response(
            indoc::indoc! {"
                limits:
                  connector:
                    sources:
                      products.api:
                        http_max_response_size: 1024b
            "},
            HttpClientTarget::ConnectorSource("products.api"),
        )
        .await;
        assert_eq!(
            body.expect_err("the source's limit is enforced")
                .to_string(),
            "connector response body exceeded limit of 1024 bytes"
        );
    }

    #[tokio::test]
    async fn connector_response_size_layer_left_out_with_only_subgraph_limits() {
        let body = read_oversized_response(
            indoc::indoc! {"
                limits:
                  subgraph:
                    all:
                      http_max_response_size: 1024b
            "},
            HttpClientTarget::ConnectorSource("products.api"),
        )
        .await;
        assert_eq!(body.expect("the source has no limit").len(), 10_000);
    }
}
