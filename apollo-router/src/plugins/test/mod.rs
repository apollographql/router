mod router_ext;
mod subgraph_ext;
mod supergraph_ext;

use std::any::TypeId;
use std::any::type_name;
use std::fmt::Debug;
use std::fmt::Formatter;
use std::future::Future;
use std::ops::Deref;
use std::str::FromStr;
use std::sync::Arc;
use std::task::Poll;

use apollo_compiler::validation::Valid;
use pin_project_lite::pin_project;
use serde::Deserialize;
use serde::Serialize;
use serde_json::Value;
use tower::BoxError;
use tower::ServiceBuilder;
use tower::ServiceExt;
use tower_service::Service;

use crate::Configuration;
use crate::Notify;
use crate::plugin;
use crate::plugin::DynPlugin;
use crate::plugin::PluginInit;
use crate::plugin::PluginPrivate;
use crate::query_planner::QueryPlannerService;
use crate::services::connector;
use crate::services::execution;
use crate::services::http;
use crate::services::router;
use crate::services::subgraph;
use crate::services::supergraph;
use crate::spec::Schema;
use crate::uplink::license_enforcement::LicenseState;

/// Test harness for plugins
/// The difference between this and the regular TestHarness is that this is more suited for unit testing.
/// It doesn't create the entire router stack, and is mostly just a convenient way to call a plugin service given an optional config and a schema.
///
/// Here is a basic example that calls a router service and checks that validates logs are generated for the telemetry plugin.
///
/// ```
///  #[tokio::test(flavor = "multi_thread")]
///     async fn test_router_service() {
///         let test_harness: PluginTestHarness<Telemetry> = PluginTestHarness::builder().build().await;
///
///         async {
///            let test_harness: PluginTestHarness<MyTestPlugin> =
///             PluginTestHarness::builder().build().await;
///
///             let mut service = test_harness.router_service(|_req| async {
///                 Ok(router::Response::fake_builder()
///                     .data(serde_json::json!({"data": {"field": "value"}}))
///                     .header("x-custom-header", "test-value")
///                     .build()
///                     .unwrap())
///                 });
///
///             let response = service.call_default().await.unwrap();
///             assert_eq!(
///                 response.response.headers().get("x-custom-header"),
///                 Some(&HeaderValue::from_static("test-value"))
///             );
///
///             response.next_response().await;
///         }
///         .with_subscriber(assert_snapshot_subscriber!())
///         .await
///     }
/// ```
///
/// You can pass in a configuration and a schema to the test harness. If you pass in a schema, the test harness will create a query planner and use the schema to extract subgraph schemas.
///
///
pub(crate) struct PluginTestHarness<T: Into<Box<dyn DynPlugin>>> {
    plugin: Box<dyn DynPlugin>,
    phantom: std::marker::PhantomData<T>,
}

impl<T: plugin::Plugin> Debug for PluginTestHarness<T> {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "PluginTestHarness<{}>", type_name::<T>())
    }
}

#[buildstructor::buildstructor]
impl<T: Into<Box<dyn DynPlugin + 'static>> + 'static> PluginTestHarness<T> {
    #[builder]
    #[allow(clippy::needless_lifetimes)] // needless in `new` but not in generated builder methods
    pub(crate) async fn new<'a, 'b>(
        config: Option<&'b str>,
        schema: Option<&'a str>,
        license: Option<LicenseState>,
    ) -> Result<Self, BoxError> {
        let factory = crate::plugin::plugins()
            .find(|factory| factory.type_id == TypeId::of::<T>())
            .expect("plugin not registered");

        let config = Configuration::from_str(config.unwrap_or_default())
            .expect("valid config required for test");

        let name = &factory.name.replace("apollo.", "");
        let config_for_plugin = config
            .validated_yaml
            .clone()
            .expect("invalid yaml")
            .as_object()
            .expect("invalid yaml")
            .get(name)
            .cloned()
            .unwrap_or(Value::Object(Default::default()));

        // Only the telemetry plugin should have access to the full router config (even in tests)
        let full_config = config
            .validated_yaml
            .clone()
            .filter(|_| name == "telemetry");

        let (supergraph_sdl, parsed_schema, subgraph_schemas) = if let Some(schema) = schema {
            let schema = Schema::parse(schema, &config).unwrap();
            let sdl = schema.raw_sdl.clone();
            let supergraph = schema.supergraph_schema().clone();
            let planner = QueryPlannerService::new(schema.into(), Arc::new(config))
                .await
                .unwrap();
            (sdl, supergraph, planner.subgraph_schemas())
        } else {
            (
                "".to_string().into(),
                Valid::assume_valid(apollo_compiler::Schema::new()),
                Default::default(),
            )
        };

        let plugin_init = PluginInit::builder()
            .config(config_for_plugin.clone())
            .supergraph_schema_id(crate::spec::Schema::schema_id(&supergraph_sdl).into_inner())
            .supergraph_sdl(supergraph_sdl)
            .supergraph_schema(Arc::new(parsed_schema))
            .subgraph_schemas(Arc::new(
                subgraph_schemas
                    .iter()
                    .map(|(k, v)| (k.clone(), v.schema.clone()))
                    .collect(),
            ))
            .notify(Notify::default())
            .license(Arc::new(license.unwrap_or_default()))
            .full_config(full_config)
            .build();

        let plugin = factory.create_instance(plugin_init).await?;

        Ok(Self {
            plugin,
            phantom: Default::default(),
        })
    }

    pub(crate) fn router_service<F>(
        &self,
        response_fn: impl Fn(router::Request) -> F + Send + Sync + Clone + 'static,
    ) -> ServiceHandle<router::Request, router::BoxService>
    where
        F: Future<Output = Result<router::Response, BoxError>> + Send + 'static,
    {
        let service: router::BoxService = router::BoxService::new(
            ServiceBuilder::new().service_fn(move |req: router::Request| {
                let response_fn = response_fn.clone();
                async move { (response_fn)(req).await }
            }),
        );

        ServiceHandle::new(self.plugin.router_service(service))
    }

    pub(crate) fn supergraph_service<F>(
        &self,
        response_fn: impl Fn(supergraph::Request) -> F + Send + Sync + Clone + 'static,
    ) -> ServiceHandle<supergraph::Request, supergraph::BoxService>
    where
        F: Future<Output = Result<supergraph::Response, BoxError>> + Send + 'static,
    {
        let service: supergraph::BoxService = supergraph::BoxService::new(
            ServiceBuilder::new().service_fn(move |req: supergraph::Request| {
                let response_fn = response_fn.clone();
                async move { (response_fn)(req).await }
            }),
        );

        ServiceHandle::new(self.plugin.supergraph_service(service))
    }

    #[allow(dead_code)]
    pub(crate) fn execution_service<F>(
        &self,
        response_fn: impl Fn(execution::Request) -> F + Send + Sync + Clone + 'static,
    ) -> ServiceHandle<execution::Request, execution::BoxService>
    where
        F: Future<Output = Result<execution::Response, BoxError>> + Send + 'static,
    {
        let service: execution::BoxService = execution::BoxService::new(
            ServiceBuilder::new().service_fn(move |req: execution::Request| {
                let response_fn = response_fn.clone();
                async move { (response_fn)(req).await }
            }),
        );

        ServiceHandle::new(self.plugin.execution_service(service))
    }

    #[allow(dead_code)]
    pub(crate) fn subgraph_service<F>(
        &self,
        subgraph: &str,
        response_fn: impl Fn(subgraph::Request) -> F + Send + Sync + Clone + 'static,
    ) -> ServiceHandle<subgraph::Request, subgraph::BoxService>
    where
        F: Future<Output = Result<subgraph::Response, BoxError>> + Send + 'static,
    {
        let service: subgraph::BoxService = subgraph::BoxService::new(
            ServiceBuilder::new().service_fn(move |req: subgraph::Request| {
                let response_fn = response_fn.clone();
                async move { (response_fn)(req).await }
            }),
        );
        ServiceHandle::new(self.plugin.subgraph_service(subgraph, service))
    }

    #[allow(dead_code)]
    pub(crate) fn http_client_service<F>(
        &self,
        subgraph: &str,
        response_fn: impl Fn(http::HttpRequest) -> F + Send + Sync + Clone + 'static,
    ) -> ServiceHandle<http::HttpRequest, http::BoxService>
    where
        F: Future<Output = Result<http::HttpResponse, BoxError>> + Send + 'static,
    {
        let service: http::BoxService = http::BoxService::new(ServiceBuilder::new().service_fn(
            move |req: http::HttpRequest| {
                let response_fn = response_fn.clone();
                async move { (response_fn)(req).await }
            },
        ));

        ServiceHandle::new(self.plugin.http_client_service(subgraph, service))
    }

    #[allow(dead_code)]
    pub(crate) async fn call_connector_request_service(
        &self,
        request: connector::request_service::Request,
        response_fn: impl Fn(
            connector::request_service::Request,
        ) -> connector::request_service::Response
        + Send
        + Sync
        + Clone
        + 'static,
    ) -> Result<connector::request_service::Response, BoxError> {
        let service: connector::request_service::BoxService =
            connector::request_service::BoxService::new(ServiceBuilder::new().service_fn(
                move |req: connector::request_service::Request| {
                    let response_fn = response_fn.clone();
                    async move { Ok((response_fn)(req)) }
                },
            ));

        self.plugin
            .connector_request_service(service, "my_connector".to_string())
            .call(request)
            .await
    }
}

impl<T> Deref for PluginTestHarness<T>
where
    T: PluginPrivate,
{
    type Target = T;

    fn deref(&self) -> &Self::Target {
        self.plugin
            .as_any()
            .downcast_ref()
            .expect("plugin should be of type T")
    }
}

pub(crate) struct ServiceHandle<Req, S>
where
    S: Service<Req, Error = BoxError>,
{
    _phantom: std::marker::PhantomData<Req>,
    service: Arc<tokio::sync::Mutex<S>>,
}

impl Clone for ServiceHandle<router::Request, router::BoxService> {
    fn clone(&self) -> Self {
        Self {
            _phantom: Default::default(),
            service: self.service.clone(),
        }
    }
}

impl<Req, S> ServiceHandle<Req, S>
where
    S: Service<Req, Error = BoxError>,
{
    pub(crate) fn new(service: S) -> Self {
        Self {
            _phantom: Default::default(),
            service: Arc::new(tokio::sync::Mutex::new(service)),
        }
    }

    /// Await the service to be ready and make a call to the service.
    pub(crate) async fn call(&self, request: Req) -> Result<S::Response, BoxError> {
        // This is a bit of a dance to ensure that we wait until the service is readu to call, make
        // the call and then drop the mutex guard before the call is executed.
        // This means that other calls to the service can take place.
        let mut service = self.service.lock().await;
        let fut = service.ready().await?.call(request);
        drop(service);
        fut.await
    }

    /// Call using the default request for the service.
    pub(crate) async fn call_default(&self) -> Result<S::Response, BoxError>
    where
        Req: FakeDefault,
    {
        self.call(FakeDefault::default()).await
    }

    /// Returns the result of calling `poll_ready` on the service.
    /// This is useful for testing things where a service may exert backpressure, but load shedding is not
    /// is expected elsewhere in the pipeline.
    pub(crate) async fn poll_ready(&self) -> Poll<Result<(), S::Error>> {
        PollReadyFuture {
            _phantom: Default::default(),
            service: self.service.clone().lock_owned().await,
        }
        .await
    }
}

pin_project! {
    struct PollReadyFuture<Req, S>
    where
        S: Service<Req>,
    {
        _phantom: std::marker::PhantomData<Req>,
        #[pin]
        service: tokio::sync::OwnedMutexGuard<S>,
    }
}

impl<Req, S> Future for PollReadyFuture<Req, S>
where
    S: Service<Req>,
{
    type Output = Poll<Result<(), S::Error>>;

    fn poll(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Self::Output> {
        let mut this = self.project();
        Poll::Ready(this.service.poll_ready(cx))
    }
}

pub(crate) trait FakeDefault {
    fn default() -> Self;
}

impl FakeDefault for router::Request {
    fn default() -> Self {
        router::Request::canned_request()
    }
}

impl FakeDefault for supergraph::Request {
    fn default() -> Self {
        supergraph::Request::fake_builder().build().unwrap()
    }
}

impl FakeDefault for execution::Request {
    fn default() -> Self {
        execution::Request::fake_builder().build()
    }
}

impl FakeDefault for subgraph::Request {
    fn default() -> Self {
        subgraph::Request::fake_builder().build()
    }
}

impl FakeDefault for http::HttpRequest {
    fn default() -> Self {
        http::HttpRequest {
            http_request: Default::default(),
            context: Default::default(),
        }
    }
}

pub(crate) trait RequestTestExt<Request, Response>
where
    Request: Send + 'static,
    Response: Send + 'static,
{
    fn canned_request() -> Request;
    fn canned_result(self) -> Result<Response, BoxError>;
    fn assert_context_eq<T>(&self, key: &str, value: T)
    where
        T: for<'de> Deserialize<'de> + Eq + PartialEq + Debug;
    fn assert_context_contains(&self, key: &str);
    fn assert_context_not_contains(&self, key: &str);
    fn assert_header_eq(&self, key: &str, value: &str);
    async fn assert_body_eq<T>(&mut self, value: T)
    where
        T: for<'de> Deserialize<'de> + Eq + PartialEq + Debug + Serialize;
    async fn assert_canned_body(&mut self);
}

pub(crate) trait ResponseTestExt {
    fn assert_context_eq<T>(&self, key: &str, value: T)
    where
        T: for<'de> Deserialize<'de> + Eq + PartialEq + Debug;
    fn assert_context_contains(&self, key: &str);
    fn assert_context_not_contains(&self, key: &str);
    fn assert_header_eq(&self, key: &str, value: &str);
    async fn assert_body_eq<T>(&mut self, value: T)
    where
        T: for<'de> Deserialize<'de> + Eq + PartialEq + Debug + Serialize;
    async fn assert_canned_body(&mut self);
    fn assert_status_code(&self, status_code: ::http::StatusCode);
    async fn assert_contains_error(&mut self, error: &Value);
}

#[cfg(test)]
mod test_for_harness {
    use ::http::HeaderMap;
    use ::http::HeaderValue;
    use async_trait::async_trait;
    use schemars::JsonSchema;
    use serde::Deserialize;
    use tokio::join;

    use super::*;
    use crate::Context;
    use crate::graphql;
    use crate::metrics::FutureMetricsExt;
    use crate::plugin::Plugin;
    use crate::services::router;
    use crate::services::router::BoxService;
    use crate::services::router::body;

    /// Config for the test plugin
    #[derive(JsonSchema, Deserialize)]
    struct MyTestPluginConfig {}

    struct MyTestPlugin {}
    #[async_trait]
    impl Plugin for MyTestPlugin {
        type Config = MyTestPluginConfig;

        async fn new(_init: PluginInit<Self::Config>) -> Result<Self, BoxError>
        where
            Self: Sized,
        {
            Ok(Self {})
        }

        fn router_service(&self, service: BoxService) -> BoxService {
            ServiceBuilder::new()
                .load_shed()
                .concurrency_limit(1)
                .service(service)
                .boxed()
        }

        fn supergraph_service(&self, service: supergraph::BoxService) -> supergraph::BoxService {
            // This purposely does not use load_shed to allow us to test readiness.
            ServiceBuilder::new()
                .concurrency_limit(1)
                .service(service)
                .boxed()
        }
    }
    register_plugin!("apollo_testing", "my_test_plugin", MyTestPlugin);

    #[tokio::test]
    async fn test_router_service() {
        let test_harness: PluginTestHarness<MyTestPlugin> = PluginTestHarness::builder()
            .build()
            .await
            .expect("test harness");

        let service = test_harness.router_service(|_req| async {
            Ok(router::Response::fake_builder()
                .data(serde_json::json!({"data": {"field": "value"}}))
                .header("x-custom-header", "test-value")
                .build()
                .unwrap())
        });

        for _ in 0..2 {
            let response = service.call_default().await.unwrap();
            assert!(service.poll_ready().await.is_ready());
            assert_eq!(
                response.response.headers().get("x-custom-header"),
                Some(&HeaderValue::from_static("test-value"))
            );
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_router_service_multi_threaded() {
        let test_harness: PluginTestHarness<MyTestPlugin> = PluginTestHarness::builder()
            .build()
            .await
            .expect("test harness");

        let service = test_harness.router_service(|_req| async {
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            Ok(router::Response::fake_builder()
                .data(serde_json::json!({"data": {"field": "value"}}))
                .header("x-custom-header", "test-value")
                .build()
                .unwrap())
        });

        let f1 = service.call_default();
        let f2 = service.call_default();

        let (r1, r2) = join!(f1, f2);
        let results = [r1, r2];
        // One of the calls should succeed, the other should fail due to concurrency limit
        assert!(results.iter().any(|r| r.is_ok()));
        assert!(results.iter().any(|r| r.is_err()));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_is_ready() {
        let test_harness: PluginTestHarness<MyTestPlugin> = PluginTestHarness::builder()
            .build()
            .await
            .expect("test harness");

        let service = test_harness.supergraph_service(|_req| async {
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            Ok(supergraph::Response::fake_builder()
                .data(serde_json::json!({"data": {"field": "value"}}))
                .header("x-custom-header", "test-value")
                .build()
                .unwrap())
        });

        // Join will progress each future in turn, so we are guaranteed that the service will enter not
        // ready state..
        let request = service.call_default();
        let (resp, poll) = join!(request, service.poll_ready());
        assert!(resp.is_ok());
        assert!(poll.is_pending());
        // Now that the first request has completed, the service should be ready again
        assert!(service.poll_ready().await.is_ready())
    }

    #[tokio::test]
    async fn test_supergraph_service() {
        let test_harness: PluginTestHarness<MyTestPlugin> = PluginTestHarness::builder()
            .build()
            .await
            .expect("test harness");

        let service = test_harness.supergraph_service(|_req| async {
            Ok(supergraph::Response::fake_builder()
                .data(serde_json::json!({"data": {"field": "value"}}))
                .header("x-custom-header", "test-value")
                .build()
                .unwrap())
        });

        let response = service.call_default().await.unwrap();
        assert_eq!(
            response.response.headers().get("x-custom-header"),
            Some(&HeaderValue::from_static("test-value"))
        );
    }

    #[tokio::test]
    async fn test_execution_service() {
        let test_harness: PluginTestHarness<MyTestPlugin> = PluginTestHarness::builder()
            .build()
            .await
            .expect("test harness");

        let service = test_harness.execution_service(|_req| async {
            Ok(execution::Response::fake_builder()
                .data(serde_json::json!({"data": {"field": "value"}}))
                .header("x-custom-header", "test-value")
                .build()
                .unwrap())
        });

        let response = service.call_default().await.unwrap();
        assert_eq!(
            response.response.headers().get("x-custom-header"),
            Some(&HeaderValue::from_static("test-value"))
        );
    }

    #[tokio::test]
    async fn test_subgraph_service() {
        let test_harness: PluginTestHarness<MyTestPlugin> = PluginTestHarness::builder()
            .build()
            .await
            .expect("test harness");

        let service = test_harness.subgraph_service("test_subgraph", |_req| async {
            let mut headers = HeaderMap::new();
            headers.insert("x-custom-header", "test-value".parse().unwrap());
            Ok(subgraph::Response::fake_builder()
                .data(serde_json::json!({"data": {"field": "value"}}))
                .headers(headers)
                .build())
        });

        let response = service.call_default().await.unwrap();
        assert_eq!(
            response.response.headers().get("x-custom-header"),
            Some(&HeaderValue::from_static("test-value"))
        );
    }

    #[tokio::test]
    async fn test_http_client_service() {
        let test_harness: PluginTestHarness<MyTestPlugin> = PluginTestHarness::builder()
            .build()
            .await
            .expect("test harness");

        let service = test_harness.http_client_service("test_client", |req| async {
            Ok(http::HttpResponse {
                http_response: ::http::Response::builder()
                    .status(200)
                    .header("x-custom-header", "test-value")
                    .body(body::empty())
                    .expect("valid response"),
                context: req.context,
            })
        });

        let response = service.call_default().await.unwrap();
        assert_eq!(
            response.http_response.headers().get("x-custom-header"),
            Some(&HeaderValue::from_static("test-value"))
        );
    }

    #[tokio::test]
    async fn test_router_service_metrics() {
        async {
            let test_harness: PluginTestHarness<MyTestPlugin> = PluginTestHarness::builder()
                .build()
                .await
                .expect("test harness");

            let service = test_harness.router_service(|_req| async {
                u64_counter!("test", "test", 1u64);
                Ok(router::Response::fake_builder()
                    .data(serde_json::json!({"data": {"field": "value"}}))
                    .header("x-custom-header", "test-value")
                    .build()
                    .unwrap())
            });

            let _ = service.call_default().await;
            assert_counter!("test", 1u64);
        }
        .with_metrics()
        .await;
    }

    #[tokio::test]
    async fn test_router_service_assertions() {
        let test_harness: PluginTestHarness<MyTestPlugin> = PluginTestHarness::builder()
            .build()
            .await
            .expect("test harness");

        let service = test_harness.router_service(|mut req| async move {
            req.assert_context_contains("request-context-key");
            req.assert_context_not_contains("non-existent-key");
            req.assert_context_eq("request-context-key", "request-context-value".to_string());
            req.assert_header_eq("x-request-header", "request-value");
            req.assert_body_eq(serde_json::json!({"query": "topProducts"}))
                .await;
            let context = req.context.clone();
            context
                .insert("response-context-key", "response-context-value".to_string())
                .expect("context");
            Ok(router::Response::fake_builder()
                .data(serde_json::json!({"field": "value"}))
                .header("x-custom-header", "test-value")
                .context(context)
                .build()
                .unwrap())
        });

        let context = Context::new();
        context
            .insert("request-context-key", "request-context-value".to_string())
            .unwrap();
        let mut response = service
            .call(
                router::Request::fake_builder()
                    .header("x-request-header", "request-value")
                    .context(context)
                    .body(serde_json::json!({"query": "topProducts"}).to_string())
                    .build()
                    .unwrap(),
            )
            .await
            .unwrap();
        response.assert_header_eq("x-custom-header", "test-value");
        response.assert_context_contains("response-context-key");
        response.assert_context_eq("response-context-key", "response-context-value".to_string());
        response.assert_context_not_contains("non-existent-key");
        response.assert_status_code(::http::StatusCode::OK);
        response
            .assert_body_eq(serde_json::json!({"data": {"field": "value"}}))
            .await;
    }

    #[tokio::test]
    async fn test_supergraph_service_assertions() {
        let test_harness: PluginTestHarness<MyTestPlugin> = PluginTestHarness::builder()
            .build()
            .await
            .expect("test harness");

        let service = test_harness.supergraph_service(|mut req| async move {
            req.assert_context_contains("request-context-key");
            req.assert_context_not_contains("non-existent-key");
            req.assert_context_eq("request-context-key", "request-context-value".to_string());
            req.assert_header_eq("x-request-header", "request-value");
            req.assert_body_eq(serde_json::json!({"query": "topProducts"}))
                .await;
            let context = req.context.clone();
            context
                .insert("response-context-key", "response-context-value".to_string())
                .expect("context");
            Ok(supergraph::Response::fake_builder()
                .data(serde_json::json!({"field": "value"}))
                .header("x-custom-header", "test-value")
                .context(context)
                .build()
                .unwrap())
        });

        let context = Context::new();
        context
            .insert("request-context-key", "request-context-value".to_string())
            .unwrap();
        let mut response = service
            .call(
                supergraph::Request::fake_builder()
                    .header("x-request-header", "request-value")
                    .context(context)
                    .query("topProducts".to_string())
                    .build()
                    .unwrap(),
            )
            .await
            .unwrap();
        response.assert_header_eq("x-custom-header", "test-value");
        response.assert_context_contains("response-context-key");
        response.assert_context_eq("response-context-key", "response-context-value".to_string());
        response.assert_context_not_contains("non-existent-key");
        response.assert_status_code(::http::StatusCode::OK);
        response
            .assert_body_eq(serde_json::json!([{"data": {"field": "value"}}]))
            .await;
    }

    #[tokio::test]
    async fn test_subgraph_service_assertions() {
        let test_harness: PluginTestHarness<MyTestPlugin> = PluginTestHarness::builder()
            .build()
            .await
            .expect("test harness");

        let service = test_harness.subgraph_service("test_subgraph", |mut req| async move {
            req.assert_context_contains("request-context-key");
            req.assert_context_not_contains("non-existent-key");
            req.assert_context_eq("request-context-key", "request-context-value".to_string());
            req.assert_header_eq("x-request-header", "request-value");
            req.assert_body_eq(serde_json::json!({"query": "topProducts"}))
                .await;
            let context = req.context.clone();
            context
                .insert("response-context-key", "response-context-value".to_string())
                .expect("context");
            let mut headers = HeaderMap::new();
            headers.insert("x-custom-header", "test-value".parse().unwrap());
            Ok(subgraph::Response::fake_builder()
                .data(serde_json::json!({"field": "value"}))
                .headers(headers)
                .context(context)
                .build())
        });

        let context = Context::new();
        context
            .insert("request-context-key", "request-context-value".to_string())
            .unwrap();
        let mut response = service
            .call(
                subgraph::Request::fake_builder()
                    .subgraph_request(
                        ::http::Request::builder()
                            .header("x-request-header", "request-value")
                            .body(
                                graphql::Request::fake_builder()
                                    .query("topProducts".to_string())
                                    .build(),
                            )
                            .unwrap(),
                    )
                    .context(context)
                    .build(),
            )
            .await
            .unwrap();
        response.assert_header_eq("x-custom-header", "test-value");
        response.assert_context_contains("response-context-key");
        response.assert_context_eq("response-context-key", "response-context-value".to_string());
        response.assert_context_not_contains("non-existent-key");
        response.assert_status_code(::http::StatusCode::OK);
        response
            .assert_body_eq(serde_json::json!({"data": {"field": "value"}}))
            .await;
    }

    #[tokio::test]
    async fn test_canned_router_request_response() {
        let test_harness: PluginTestHarness<MyTestPlugin> = PluginTestHarness::builder()
            .build()
            .await
            .expect("test harness");

        let service = test_harness.router_service(|mut req| async move {
            req.assert_canned_body().await;
            req.canned_result()
        });

        let mut response = service
            .call(router::Request::canned_request())
            .await
            .unwrap();
        response.assert_canned_body().await;
    }

    #[tokio::test]
    async fn test_canned_supergraph_request_response() {
        let test_harness: PluginTestHarness<MyTestPlugin> = PluginTestHarness::builder()
            .build()
            .await
            .expect("test harness");

        let service = test_harness.supergraph_service(|mut req| async move {
            req.assert_canned_body().await;
            req.canned_result()
        });

        let mut response = service
            .call(supergraph::Request::canned_request())
            .await
            .unwrap();
        response.assert_canned_body().await;
    }

    #[tokio::test]
    async fn test_canned_subgraph_request_response() {
        let test_harness: PluginTestHarness<MyTestPlugin> = PluginTestHarness::builder()
            .build()
            .await
            .expect("test harness");

        let service = test_harness.subgraph_service("test_subgraph", |mut req| async move {
            req.assert_canned_body().await;
            req.canned_result()
        });

        let mut response = service
            .call(subgraph::Request::canned_request())
            .await
            .unwrap();
        response.assert_canned_body().await
    }

    #[tokio::test]
    async fn test_router_service_assert_contains_error() {
        let test_harness: PluginTestHarness<MyTestPlugin> = PluginTestHarness::builder()
            .build()
            .await
            .expect("test harness");

        let service = test_harness.router_service(|_req| async {
            Ok(router::Response::fake_builder()
                .error(
                    graphql::Error::builder()
                        .message("Test error")
                        .extension_code("TEST_ERROR")
                        .build(),
                )
                .build()
                .unwrap())
        });

        let mut response = service.call_default().await.unwrap();
        response
            .assert_contains_error(
                &serde_json::json!({"message": "Test error", "extensions":{"code": "TEST_ERROR"}}),
            )
            .await;
    }

    #[tokio::test]
    async fn test_supergraph_service_assert_contains_error() {
        let test_harness: PluginTestHarness<MyTestPlugin> = PluginTestHarness::builder()
            .build()
            .await
            .expect("test harness");

        let service = test_harness.supergraph_service(|_req| async {
            Ok(supergraph::Response::fake_builder()
                .error(
                    graphql::Error::builder()
                        .message("Test error")
                        .extension_code("TEST_ERROR")
                        .build(),
                )
                .build()
                .unwrap())
        });

        let mut response = service.call_default().await.unwrap();
        response
            .assert_contains_error(
                &serde_json::json!({"message": "Test error", "extensions":{"code": "TEST_ERROR"}}),
            )
            .await;
    }

    #[tokio::test]
    async fn test_subgraph_service_assert_error_contains_error() {
        let test_harness: PluginTestHarness<MyTestPlugin> = PluginTestHarness::builder()
            .build()
            .await
            .expect("test harness");

        let service = test_harness.subgraph_service("test_subgraph", |_req| async {
            Ok(subgraph::Response::fake_builder()
                .error(
                    graphql::Error::builder()
                        .message("Test error")
                        .extension_code("TEST_ERROR")
                        .build(),
                )
                .build())
        });

        let mut response = service.call_default().await.unwrap();
        response
            .assert_contains_error(
                &serde_json::json!({"message": "Test error", "extensions":{"code": "TEST_ERROR"}}),
            )
            .await;
    }
}
