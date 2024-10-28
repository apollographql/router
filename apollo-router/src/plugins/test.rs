use std::any::TypeId;
use std::future::Future;
use std::ops::Deref;
use std::str::FromStr;
use std::sync::Arc;

use apollo_compiler::validation::Valid;
use serde_json::Value;
use tower::BoxError;
use tower::ServiceBuilder;
use tower_service::Service;

use crate::introspection::IntrospectionCache;
use crate::plugin::DynPlugin;
use crate::plugin::Plugin;
use crate::plugin::PluginInit;
use crate::query_planner::BridgeQueryPlanner;
use crate::query_planner::PlannerMode;
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
///             let mut response = test_harness
///                 .call_router(
///                     router::Request::fake_builder()
///                         .body("query { foo }")
///                         .build()
///                         .expect("expecting valid request"),
///                     |_r| {
///                         tracing::info!("response");
///                         router::Response::fake_builder()
///                             .header("custom-header", "val1")
///                             .data(serde_json::json!({"data": "res"}))
///                             .build()
///                             .expect("expecting valid response")
///                     },
///                 )
///                 .await
///                 .expect("expecting successful response");
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
pub(crate) struct PluginTestHarness<T> {
    plugin: Box<dyn DynPlugin>,
    phantom: std::marker::PhantomData<T>,
}
#[buildstructor::buildstructor]
impl<T: DynPlugin> PluginTestHarness<T> {
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
            let rust_planner = PlannerMode::maybe_rust(&schema, &config).unwrap();
            let introspection = Arc::new(IntrospectionCache::new(&config));
            let planner = BridgeQueryPlanner::new(
                schema.into(),
                Arc::new(config),
                None,
                rust_planner,
                introspection,
            )
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
            .supergraph_schema_id(crate::spec::Schema::schema_id(&supergraph_sdl).into())
            .supergraph_sdl(supergraph_sdl)
            .supergraph_schema(Arc::new(parsed_schema))
            .subgraph_schemas(subgraph_schemas)
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

    #[allow(dead_code)]
    pub(crate) async fn call_router<F>(
        &self,
        request: router::Request,
        response_fn: fn(router::Request) -> F,
    ) -> Result<router::Response, BoxError>
    where
        F: Future<Output = Result<router::Response, BoxError>> + Send + 'static,
    {
        let service: router::BoxService = router::BoxService::new(
            ServiceBuilder::new()
                .service_fn(move |req: router::Request| async move { (response_fn)(req).await }),
        );

        self.plugin.router_service(service).call(request).await
    }

    pub(crate) async fn call_supergraph(
        &self,
        request: supergraph::Request,
        response_fn: fn(supergraph::Request) -> supergraph::Response,
    ) -> Result<supergraph::Response, BoxError> {
        let service: supergraph::BoxService = supergraph::BoxService::new(
            ServiceBuilder::new()
                .service_fn(move |req: supergraph::Request| async move { Ok((response_fn)(req)) }),
        );

        self.plugin.supergraph_service(service).call(request).await
    }

    #[allow(dead_code)]
    pub(crate) async fn call_execution(
        &self,
        request: execution::Request,
        response_fn: fn(execution::Request) -> execution::Response,
    ) -> Result<execution::Response, BoxError> {
        let service: execution::BoxService = execution::BoxService::new(
            ServiceBuilder::new()
                .service_fn(move |req: execution::Request| async move { Ok((response_fn)(req)) }),
        );

        self.plugin.execution_service(service).call(request).await
    }

    #[allow(dead_code)]
    pub(crate) async fn call_subgraph(
        &self,
        request: subgraph::Request,
        response_fn: fn(subgraph::Request) -> subgraph::Response,
    ) -> Result<subgraph::Response, BoxError> {
        let name = request.subgraph_name.clone();
        let service: subgraph::BoxService = subgraph::BoxService::new(
            ServiceBuilder::new()
                .service_fn(move |req: subgraph::Request| async move { Ok((response_fn)(req)) }),
        );

        self.plugin
            .subgraph_service(&name.expect("subgraph name must be populated"), service)
            .call(request)
            .await
    }
    #[allow(dead_code)]
    pub(crate) async fn call_http_client(
        &self,
        subgraph_name: &str,
        request: http::HttpRequest,
        response_fn: fn(http::HttpRequest) -> http::HttpResponse,
    ) -> Result<http::HttpResponse, BoxError> {
        let service: http::BoxService = http::BoxService::new(
            ServiceBuilder::new()
                .service_fn(move |req: http::HttpRequest| async move { Ok((response_fn)(req)) }),
        );

        self.plugin
            .http_client_service(subgraph_name, service)
            .call(request)
            .await
    }
}

impl<T> Deref for PluginTestHarness<T>
where
    T: Plugin,
{
    type Target = T;

    fn deref(&self) -> &Self::Target {
        self.plugin
            .as_any()
            .downcast_ref()
            .expect("plugin should be of type T")
    }
}
