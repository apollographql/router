//! Plugin system for the router.
//!
//! Provides a customization mechanism for the router.
//!
//! Requests received by the router make their way through a processing pipeline. Each request is
//! processed at:
//!  - router
//!  - query planning
//!  - execution
//!  - subgraph (multiple in parallel if multiple subgraphs are accessed)
//!  stages.
//!
//! A plugin can choose to interact with the flow of requests at any or all of these stages of
//! processing. At each stage a [`Service`] is provided which provides an appropriate
//! mechanism for interacting with the request and response.

pub mod serde;
pub mod test;

use std::collections::HashMap;
use std::sync::Mutex;
use std::task::Context;
use std::task::Poll;

use ::serde::de::DeserializeOwned;
use ::serde::Deserialize;
use async_trait::async_trait;
use bytes::Bytes;
use futures::future::BoxFuture;
use once_cell::sync::Lazy;
use schemars::gen::SchemaGenerator;
use schemars::JsonSchema;
use tower::buffer::future::ResponseFuture;
use tower::buffer::Buffer;
use tower::util::BoxService;
use tower::BoxError;
use tower::Service;
use tower::ServiceBuilder;

use crate::http_ext;
use crate::layers::ServiceBuilderExt;
use crate::ExecutionRequest;
use crate::ExecutionResponse;
use crate::QueryPlannerRequest;
use crate::QueryPlannerResponse;
use crate::RouterRequest;
use crate::RouterResponse;
use crate::SubgraphRequest;
use crate::SubgraphResponse;

type InstanceFactory = fn(&serde_json::Value) -> BoxFuture<Result<Box<dyn DynPlugin>, BoxError>>;

type SchemaFactory = fn(&mut SchemaGenerator) -> schemars::schema::Schema;

/// Factories for plugin schema and configuration.
#[derive(Clone)]
pub struct PluginFactory {
    instance_factory: InstanceFactory,
    schema_factory: SchemaFactory,
}

impl PluginFactory {
    pub fn new(instance_factory: InstanceFactory, schema_factory: SchemaFactory) -> Self {
        Self {
            instance_factory,
            schema_factory,
        }
    }

    pub async fn create_instance(
        &self,
        configuration: &serde_json::Value,
    ) -> Result<Box<dyn DynPlugin>, BoxError> {
        (self.instance_factory)(configuration).await
    }

    pub fn create_schema(&self, gen: &mut SchemaGenerator) -> schemars::schema::Schema {
        (self.schema_factory)(gen)
    }
}

static PLUGIN_REGISTRY: Lazy<Mutex<HashMap<String, PluginFactory>>> = Lazy::new(|| {
    let m = HashMap::new();
    Mutex::new(m)
});

/// Register a plugin factory.
pub fn register_plugin(name: String, plugin_factory: PluginFactory) {
    PLUGIN_REGISTRY
        .lock()
        .expect("Lock poisoned")
        .insert(name, plugin_factory);
}

/// Get a copy of the registered plugin factories.
pub fn plugins() -> HashMap<String, PluginFactory> {
    PLUGIN_REGISTRY.lock().expect("Lock poisoned").clone()
}

/// All router plugins must implement the Plugin trait.
///
/// This trait defines lifecycle hooks that enable hooking into Apollo Router services.
/// The trait also provides a default implementations for each hook, which returns the associated service unmodified.
/// For more information about the plugin lifecycle please check this documentation <https://www.apollographql.com/docs/router/customizations/native/#plugin-lifecycle>
#[async_trait]
pub trait Plugin: Send + Sync + 'static + Sized {
    type Config: JsonSchema + DeserializeOwned;

    /// This is invoked once after the router starts and compiled-in
    /// plugins are registered.
    async fn new(config: Self::Config) -> Result<Self, BoxError>;

    /// This is invoked after all plugins have been created and we're ready to go live.
    /// This method MUST not panic.
    fn activate(&mut self) {}

    /// This service runs at the very beginning and very end of the request lifecycle.
    /// Define router_service if your customization needs to interact at the earliest or latest point possible.
    /// For example, this is a good opportunity to perform JWT verification before allowing a request to proceed further.
    fn router_service(
        &self,
        service: BoxService<RouterRequest, RouterResponse, BoxError>,
    ) -> BoxService<RouterRequest, RouterResponse, BoxError> {
        service
    }

    /// This service handles generating the query plan for each incoming request.
    /// Define `query_planning_service` if your customization needs to interact with query planning functionality (for example, to log query plan details).
    fn query_planning_service(
        &self,
        service: BoxService<QueryPlannerRequest, QueryPlannerResponse, BoxError>,
    ) -> BoxService<QueryPlannerRequest, QueryPlannerResponse, BoxError> {
        service
    }

    /// This service handles initiating the execution of a query plan after it's been generated.
    /// Define `execution_service` if your customization includes logic to govern execution (for example, if you want to block a particular query based on a policy decision).
    fn execution_service(
        &self,
        service: BoxService<ExecutionRequest, ExecutionResponse, BoxError>,
    ) -> BoxService<ExecutionRequest, ExecutionResponse, BoxError> {
        service
    }

    /// This service handles communication between the Apollo Router and your subgraphs.
    /// Define `subgraph_service` to configure this communication (for example, to dynamically add headers to pass to a subgraph).
    /// The `_subgraph_name` parameter is useful if you need to apply a customization only specific subgraphs.
    fn subgraph_service(
        &self,
        _subgraph_name: &str,
        service: BoxService<SubgraphRequest, SubgraphResponse, BoxError>,
    ) -> BoxService<SubgraphRequest, SubgraphResponse, BoxError> {
        service
    }

    /// The `custom_endpoint` method lets you declare a new endpoint exposed for your plugin.
    /// For now it's only accessible for official `apollo.` plugins and for `experimental.`. This endpoint will be accessible via `/plugins/group.plugin_name`
    fn custom_endpoint(&self) -> Option<Handler> {
        None
    }

    /// Return the name of the plugin.
    fn name(&self) -> &'static str {
        get_type_of(self)
    }
}

fn get_type_of<T>(_: &T) -> &'static str {
    std::any::type_name::<T>()
}

/// All router plugins must implement the DynPlugin trait.
///
/// This trait defines lifecycle hooks that enable hooking into Apollo Router services.
/// The trait also provides a default implementations for each hook, which returns the associated service unmodified.
/// For more information about the plugin lifecycle please check this documentation <https://www.apollographql.com/docs/router/customizations/native/#plugin-lifecycle>
#[async_trait]
pub trait DynPlugin: Send + Sync + 'static {
    /// This is invoked after all plugins have been created and we're ready to go live.
    /// This method MUST not panic.
    fn activate(&mut self);

    /// This service runs at the very beginning and very end of the request lifecycle.
    /// It's the entrypoint of every requests and also the last hook before sending the response.
    /// Define router_service if your customization needs to interact at the earliest or latest point possible.
    /// For example, this is a good opportunity to perform JWT verification before allowing a request to proceed further.
    fn router_service(
        &self,
        service: BoxService<RouterRequest, RouterResponse, BoxError>,
    ) -> BoxService<RouterRequest, RouterResponse, BoxError>;

    /// This service handles generating the query plan for each incoming request.
    /// Define `query_planning_service` if your customization needs to interact with query planning functionality (for example, to log query plan details).
    fn query_planning_service(
        &self,
        service: BoxService<QueryPlannerRequest, QueryPlannerResponse, BoxError>,
    ) -> BoxService<QueryPlannerRequest, QueryPlannerResponse, BoxError>;

    /// This service handles initiating the execution of a query plan after it's been generated.
    /// Define `execution_service` if your customization includes logic to govern execution (for example, if you want to block a particular query based on a policy decision).
    fn execution_service(
        &self,
        service: BoxService<ExecutionRequest, ExecutionResponse, BoxError>,
    ) -> BoxService<ExecutionRequest, ExecutionResponse, BoxError>;

    /// This service handles communication between the Apollo Router and your subgraphs.
    /// Define `subgraph_service` to configure this communication (for example, to dynamically add headers to pass to a subgraph).
    /// The `_subgraph_name` parameter is useful if you need to apply a customization only on specific subgraphs.
    fn subgraph_service(
        &self,
        _subgraph_name: &str,
        service: BoxService<SubgraphRequest, SubgraphResponse, BoxError>,
    ) -> BoxService<SubgraphRequest, SubgraphResponse, BoxError>;

    /// The `custom_endpoint` method lets you declare a new endpoint exposed for your plugin.
    /// For now it's only accessible for official `apollo.` plugins and for `experimental.`. This endpoint will be accessible via `/plugins/group.plugin_name`
    fn custom_endpoint(&self) -> Option<Handler>;

    /// Return the name of the plugin.
    fn name(&self) -> &'static str;
}

#[async_trait]
impl<T> DynPlugin for T
where
    T: Plugin,
    for<'de> <T as Plugin>::Config: Deserialize<'de>,
{
    #[allow(deprecated)]
    fn activate(&mut self) {
        self.activate()
    }

    fn router_service(
        &self,
        service: BoxService<RouterRequest, RouterResponse, BoxError>,
    ) -> BoxService<RouterRequest, RouterResponse, BoxError> {
        self.router_service(service)
    }

    fn query_planning_service(
        &self,
        service: BoxService<QueryPlannerRequest, QueryPlannerResponse, BoxError>,
    ) -> BoxService<QueryPlannerRequest, QueryPlannerResponse, BoxError> {
        self.query_planning_service(service)
    }

    fn execution_service(
        &self,
        service: BoxService<ExecutionRequest, ExecutionResponse, BoxError>,
    ) -> BoxService<ExecutionRequest, ExecutionResponse, BoxError> {
        self.execution_service(service)
    }

    fn subgraph_service(
        &self,
        name: &str,
        service: BoxService<SubgraphRequest, SubgraphResponse, BoxError>,
    ) -> BoxService<SubgraphRequest, SubgraphResponse, BoxError> {
        self.subgraph_service(name, service)
    }

    fn custom_endpoint(&self) -> Option<Handler> {
        self.custom_endpoint()
    }

    fn name(&self) -> &'static str {
        self.name()
    }
}

/// Register a plugin with a group and a name
/// Grouping prevent name clashes for plugins, so choose something unique, like your domain name.
/// Plugins will appear in the configuration as a layer property called: {group}.{name}
#[macro_export]
macro_rules! register_plugin {
    ($group: literal, $name: literal, $value: ident) => {
        $crate::_private::startup::on_startup! {
            let qualified_name = if $group == "" {
                $name.to_string()
            }
            else {
                format!("{}.{}", $group, $name)
            };

            $crate::plugin::register_plugin(
                qualified_name,
                $crate::plugin::PluginFactory::new(
                    |configuration| Box::pin(async move {
                        let configuration = $crate::_private::serde_json::from_value(configuration.clone())?;
                        let plugin = $value::new(configuration).await?;
                        Ok(Box::new(plugin) as Box<dyn $crate::plugin::DynPlugin>)
                    }),
                    |gen| gen.subschema_for::<<$value as $crate::plugin::Plugin>::Config>()
                )
            );
        }
    };
}

/// Handler represents a [`Plugin`] endpoint.
#[derive(Clone)]
pub struct Handler {
    service: Buffer<
        BoxService<http_ext::Request<Bytes>, http_ext::Response<Bytes>, BoxError>,
        http_ext::Request<Bytes>,
    >,
}

impl Handler {
    pub fn new(
        service: BoxService<http_ext::Request<Bytes>, http_ext::Response<Bytes>, BoxError>,
    ) -> Self {
        Self {
            service: ServiceBuilder::new().buffered().service(service),
        }
    }
}

impl Service<http_ext::Request<Bytes>> for Handler {
    type Response = http_ext::Response<Bytes>;
    type Error = BoxError;
    type Future = ResponseFuture<BoxFuture<'static, Result<Self::Response, Self::Error>>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.service.poll_ready(cx)
    }

    fn call(&mut self, req: http_ext::Request<Bytes>) -> Self::Future {
        self.service.call(req)
    }
}

impl From<BoxService<http_ext::Request<Bytes>, http_ext::Response<Bytes>, BoxError>> for Handler {
    fn from(
        original: BoxService<http_ext::Request<Bytes>, http_ext::Response<Bytes>, BoxError>,
    ) -> Self {
        Self::new(original)
    }
}
