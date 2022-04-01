use crate::services::ServiceBuilderExt;
use crate::{
    http_compat, ExecutionRequest, ExecutionResponse, QueryPlannerRequest, QueryPlannerResponse,
    ResponseBody, RouterRequest, RouterResponse, SubgraphRequest, SubgraphResponse,
};
use async_trait::async_trait;
use bytes::Bytes;
use futures::future::BoxFuture;
use once_cell::sync::Lazy;
use schemars::gen::SchemaGenerator;
use schemars::JsonSchema;
use serde::{de::DeserializeOwned, Deserialize};
use std::collections::HashMap;
use std::sync::Mutex;
use std::task::{Context, Poll};
use tower::buffer::future::ResponseFuture;
use tower::buffer::Buffer;
use tower::util::BoxService;
use tower::{BoxError, Service, ServiceBuilder};

type InstanceFactory = fn(&serde_json::Value) -> Result<Box<dyn DynPlugin>, BoxError>;

type SchemaFactory = fn(&mut SchemaGenerator) -> schemars::schema::Schema;

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

    pub fn create_instance(
        &self,
        configuration: &serde_json::Value,
    ) -> Result<Box<dyn DynPlugin>, BoxError> {
        (self.instance_factory)(configuration)
    }

    pub fn create_schema(&self, gen: &mut SchemaGenerator) -> schemars::schema::Schema {
        (self.schema_factory)(gen)
    }
}

static PLUGIN_REGISTRY: Lazy<Mutex<HashMap<String, PluginFactory>>> = Lazy::new(|| {
    let m = HashMap::new();
    Mutex::new(m)
});

pub fn register_plugin(name: String, plugin_factory: PluginFactory) {
    PLUGIN_REGISTRY
        .lock()
        .expect("Lock poisoned")
        .insert(name, plugin_factory);
}

pub fn plugins() -> HashMap<String, PluginFactory> {
    PLUGIN_REGISTRY.lock().expect("Lock poisoned").clone()
}

#[async_trait]
pub trait Plugin: Send + Sync + 'static + Sized {
    type Config: JsonSchema + DeserializeOwned;

    fn new(config: Self::Config) -> Result<Self, BoxError>;

    // Plugins will receive a notification that they should start up and shut down.
    async fn startup(&mut self) -> Result<(), BoxError> {
        Ok(())
    }
    async fn shutdown(&mut self) -> Result<(), BoxError> {
        Ok(())
    }

    fn router_service(
        &mut self,
        service: BoxService<RouterRequest, RouterResponse, BoxError>,
    ) -> BoxService<RouterRequest, RouterResponse, BoxError> {
        service
    }

    fn query_planning_service(
        &mut self,
        service: BoxService<QueryPlannerRequest, QueryPlannerResponse, BoxError>,
    ) -> BoxService<QueryPlannerRequest, QueryPlannerResponse, BoxError> {
        service
    }

    fn execution_service(
        &mut self,
        service: BoxService<ExecutionRequest, ExecutionResponse, BoxError>,
    ) -> BoxService<ExecutionRequest, ExecutionResponse, BoxError> {
        service
    }

    fn subgraph_service(
        &mut self,
        _name: &str,
        service: BoxService<SubgraphRequest, SubgraphResponse, BoxError>,
    ) -> BoxService<SubgraphRequest, SubgraphResponse, BoxError> {
        service
    }

    fn custom_endpoint(&self) -> Option<Handler> {
        None
    }

    fn name(&self) -> &'static str {
        get_type_of(self)
    }
}

fn get_type_of<T>(_: &T) -> &'static str {
    std::any::type_name::<T>()
}

#[async_trait]
pub trait DynPlugin: Send + Sync + 'static {
    // Plugins will receive a notification that they should start up and shut down.
    async fn startup(&mut self) -> Result<(), BoxError>;

    async fn shutdown(&mut self) -> Result<(), BoxError>;

    fn router_service(
        &mut self,
        service: BoxService<RouterRequest, RouterResponse, BoxError>,
    ) -> BoxService<RouterRequest, RouterResponse, BoxError>;

    fn query_planning_service(
        &mut self,
        service: BoxService<QueryPlannerRequest, QueryPlannerResponse, BoxError>,
    ) -> BoxService<QueryPlannerRequest, QueryPlannerResponse, BoxError>;

    fn execution_service(
        &mut self,
        service: BoxService<ExecutionRequest, ExecutionResponse, BoxError>,
    ) -> BoxService<ExecutionRequest, ExecutionResponse, BoxError>;

    fn subgraph_service(
        &mut self,
        _name: &str,
        service: BoxService<SubgraphRequest, SubgraphResponse, BoxError>,
    ) -> BoxService<SubgraphRequest, SubgraphResponse, BoxError>;
    fn custom_endpoint(&self) -> Option<Handler>;
    fn name(&self) -> &'static str;
}

#[async_trait]
impl<T> DynPlugin for T
where
    T: Plugin,
    for<'de> <T as Plugin>::Config: Deserialize<'de>,
{
    // Plugins will receive a notification that they should start up and shut down.
    async fn startup(&mut self) -> Result<(), BoxError> {
        self.startup().await
    }

    async fn shutdown(&mut self) -> Result<(), BoxError> {
        self.shutdown().await
    }

    fn router_service(
        &mut self,
        service: BoxService<RouterRequest, RouterResponse, BoxError>,
    ) -> BoxService<RouterRequest, RouterResponse, BoxError> {
        self.router_service(service)
    }

    fn query_planning_service(
        &mut self,
        service: BoxService<QueryPlannerRequest, QueryPlannerResponse, BoxError>,
    ) -> BoxService<QueryPlannerRequest, QueryPlannerResponse, BoxError> {
        self.query_planning_service(service)
    }

    fn execution_service(
        &mut self,
        service: BoxService<ExecutionRequest, ExecutionResponse, BoxError>,
    ) -> BoxService<ExecutionRequest, ExecutionResponse, BoxError> {
        self.execution_service(service)
    }

    fn subgraph_service(
        &mut self,
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
    ($name: literal, $value: ident) => {
        startup::on_startup! {
            let qualified_name = $name.to_string();

            $crate::register_plugin(qualified_name, $crate::PluginFactory::new(|configuration| {
                let plugin = $value::new(serde_json::from_value(configuration.clone())?)?;
                Ok(Box::new(plugin))
            }, |gen| gen.subschema_for::<<$value as $crate::Plugin>::Config>()));
        }
    };
    ($group: literal, $name: literal, $value: ident) => {
        $crate::reexports::startup::on_startup! {
            let qualified_name = if $group == "" {
                $name.to_string()
            }
            else {
                format!("{}.{}", $group, $name)
            };

            $crate::register_plugin(qualified_name, $crate::PluginFactory::new(|configuration| {
                let plugin = $value::new(serde_json::from_value(configuration.clone())?)?;
                Ok(Box::new(plugin))
            }, |gen| gen.subschema_for::<<$value as $crate::Plugin>::Config>()));
        }
    };
}

#[derive(Clone)]
pub struct Handler {
    service: Buffer<
        BoxService<http_compat::Request<Bytes>, http_compat::Response<ResponseBody>, BoxError>,
        http_compat::Request<Bytes>,
    >,
}

impl Handler {
    pub fn new(
        service: BoxService<
            http_compat::Request<Bytes>,
            http_compat::Response<ResponseBody>,
            BoxError,
        >,
    ) -> Self {
        Self {
            service: ServiceBuilder::new().buffered().service(service),
        }
    }
}

impl Service<http_compat::Request<Bytes>> for Handler {
    type Response = http_compat::Response<ResponseBody>;
    type Error = BoxError;
    type Future = ResponseFuture<BoxFuture<'static, Result<Self::Response, Self::Error>>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.service.poll_ready(cx)
    }

    fn call(&mut self, req: http_compat::Request<Bytes>) -> Self::Future {
        self.service.call(req)
    }
}

impl From<BoxService<http_compat::Request<Bytes>, http_compat::Response<ResponseBody>, BoxError>>
    for Handler
{
    fn from(
        original: BoxService<
            http_compat::Request<Bytes>,
            http_compat::Response<ResponseBody>,
            BoxError,
        >,
    ) -> Self {
        Self::new(original)
    }
}
