use std::any::TypeId;
use std::future::Future;
use std::ops::Deref;
use std::str::FromStr;
use std::sync::Arc;
use std::task::Poll;

use apollo_compiler::validation::Valid;
use pin_project_lite::pin_project;
use serde_json::Value;
use tower::BoxError;
use tower::ServiceBuilder;
use tower::ServiceExt;
use tower_service::Service;

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
use crate::Configuration;
use crate::Notify;

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
#[buildstructor::buildstructor]
impl<T: Into<Box<dyn DynPlugin + 'static>> + 'static> PluginTestHarness<T> {
    #[builder]
    pub(crate) async fn new<'a, 'b>(config: Option<&'a str>, schema: Option<&'b str>) -> Self {
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
            .build();

        let plugin = factory
            .create_instance(plugin_init)
            .await
            .expect("failed to create plugin");

        Self {
            plugin,
            phantom: Default::default(),
        }
    }

    pub(crate) fn router_service<F>(
        &self,
        response_fn: fn(router::Request) -> F,
    ) -> ServiceHandle<router::Request, router::BoxService>
    where
        F: Future<Output = Result<router::Response, BoxError>> + Send + 'static,
    {
        let service: router::BoxService = router::BoxService::new(
            ServiceBuilder::new()
                .service_fn(move |req: router::Request| async move { (response_fn)(req).await }),
        );

        ServiceHandle::new(self.plugin.router_service(service))
    }

    pub(crate) fn supergraph_service<F>(
        &self,
        response_fn: fn(supergraph::Request) -> F,
    ) -> ServiceHandle<supergraph::Request, supergraph::BoxService>
    where
        F: Future<Output = Result<supergraph::Response, BoxError>> + Send + 'static,
    {
        let service: supergraph::BoxService =
            supergraph::BoxService::new(ServiceBuilder::new().service_fn(
                move |req: supergraph::Request| async move { (response_fn)(req).await },
            ));

        ServiceHandle::new(self.plugin.supergraph_service(service))
    }

    #[allow(dead_code)]
    pub(crate) fn execution_service<F>(
        &self,
        response_fn: fn(execution::Request) -> F,
    ) -> ServiceHandle<execution::Request, execution::BoxService>
    where
        F: Future<Output = Result<execution::Response, BoxError>> + Send + 'static,
    {
        let service: execution::BoxService = execution::BoxService::new(
            ServiceBuilder::new()
                .service_fn(move |req: execution::Request| async move { (response_fn)(req).await }),
        );

        ServiceHandle::new(self.plugin.execution_service(service))
    }

    #[allow(dead_code)]
    pub(crate) fn subgraph_service<F>(
        &self,
        subgraph: &str,
        response_fn: fn(subgraph::Request) -> F,
    ) -> ServiceHandle<subgraph::Request, subgraph::BoxService>
    where
        F: Future<Output = Result<subgraph::Response, BoxError>> + Send + 'static,
    {
        let service: subgraph::BoxService = subgraph::BoxService::new(
            ServiceBuilder::new()
                .service_fn(move |req: subgraph::Request| async move { (response_fn)(req).await }),
        );
        ServiceHandle::new(self.plugin.subgraph_service(subgraph, service))
    }

    #[allow(dead_code)]
    pub(crate) fn http_client_service<F>(
        &self,
        subgraph: &str,
        response_fn: fn(http::HttpRequest) -> F,
    ) -> ServiceHandle<http::HttpRequest, http::BoxService>
    where
        F: Future<Output = Result<http::HttpResponse, BoxError>> + Send + 'static,
    {
        let service: http::BoxService = http::BoxService::new(
            ServiceBuilder::new()
                .service_fn(move |req: http::HttpRequest| async move { (response_fn)(req).await }),
        );

        ServiceHandle::new(self.plugin.http_client_service(subgraph, service))
    }

    #[allow(dead_code)]
    pub(crate) async fn call_connector_request_service(
        &self,
        request: connector::request_service::Request,
        response_fn: fn(
            connector::request_service::Request,
        ) -> connector::request_service::Response,
    ) -> Result<connector::request_service::Response, BoxError> {
        let service: connector::request_service::BoxService =
            connector::request_service::BoxService::new(
                ServiceBuilder::new().service_fn(
                    move |req: connector::request_service::Request| async move {
                        Ok((response_fn)(req))
                    },
                ),
            );

        self.plugin
            .connector_request_service(service)
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
        router::Request::fake_builder().build().unwrap()
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

#[cfg(test)]
mod test_for_harness {
    use ::http::HeaderMap;
    use ::http::HeaderValue;
    use async_trait::async_trait;
    use schemars::JsonSchema;
    use serde::Deserialize;
    use tokio::join;

    use super::*;
    use crate::metrics::FutureMetricsExt;
    use crate::plugin::Plugin;
    use crate::services::router;
    use crate::services::router::body;
    use crate::services::router::BoxService;

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
        let test_harness: PluginTestHarness<MyTestPlugin> =
            PluginTestHarness::builder().build().await;

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
        let test_harness: PluginTestHarness<MyTestPlugin> =
            PluginTestHarness::builder().build().await;

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
        let results = vec![r1, r2];
        // One of the calls should succeed, the other should fail due to concurrency limit
        assert!(results.iter().any(|r| r.is_ok()));
        assert!(results.iter().any(|r| r.is_err()));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_router_service_metrics() {
        async {
            let test_harness: PluginTestHarness<MyTestPlugin> =
                PluginTestHarness::builder().build().await;

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

    #[tokio::test(flavor = "multi_thread")]
    async fn test_is_ready() {
        let test_harness: PluginTestHarness<MyTestPlugin> =
            PluginTestHarness::builder().build().await;

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
        let test_harness: PluginTestHarness<MyTestPlugin> =
            PluginTestHarness::builder().build().await;

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
        let test_harness: PluginTestHarness<MyTestPlugin> =
            PluginTestHarness::builder().build().await;

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
        let test_harness: PluginTestHarness<MyTestPlugin> =
            PluginTestHarness::builder().build().await;

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
        let test_harness: PluginTestHarness<MyTestPlugin> =
            PluginTestHarness::builder().build().await;

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
}
