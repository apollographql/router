//! Plugin system for the router.
//!
//! Provides a customization mechanism for the router.
//!
//! Requests received by the router make their way through a processing pipeline. Each request is
//! processed at:
//!  - router
//!  - execution
//!  - subgraph (multiple in parallel if multiple subgraphs are accessed)
//!  stages.
//!
//! A plugin can choose to interact with the flow of requests at any or all of these stages of
//! processing. At each stage a [`Service`] is provided which provides an appropriate
//! mechanism for interacting with the request and response.

pub mod serde;
#[macro_use]
pub mod test;

use std::any::TypeId;
use std::collections::HashMap;
use std::fmt;
use std::path::PathBuf;
use std::sync::Arc;
use std::task::Context;
use std::task::Poll;

use ::serde::de::DeserializeOwned;
use ::serde::Deserialize;
use apollo_compiler::validation::Valid;
use apollo_compiler::Schema;
use async_trait::async_trait;
use futures::future::BoxFuture;
use multimap::MultiMap;
use once_cell::sync::Lazy;
use schemars::gen::SchemaGenerator;
use schemars::JsonSchema;
use tower::buffer::future::ResponseFuture;
use tower::buffer::Buffer;
use tower::BoxError;
use tower::Service;
use tower::ServiceBuilder;

use crate::graphql;
use crate::layers::ServiceBuilderExt;
use crate::notification::Notify;
use crate::query_planner::fetch::SubgraphSchemas;
use crate::router_factory::Endpoint;
use crate::services::execution;
use crate::services::router;
use crate::services::subgraph;
use crate::services::supergraph;
use crate::ListenAddr;

type InstanceFactory =
    fn(PluginInit<serde_json::Value>) -> BoxFuture<'static, Result<Box<dyn DynPlugin>, BoxError>>;

type SchemaFactory = fn(&mut SchemaGenerator) -> schemars::schema::Schema;

/// Global list of plugins.
#[linkme::distributed_slice]
pub static PLUGINS: [Lazy<PluginFactory>] = [..];

/// Initialise details for a plugin
#[non_exhaustive]
pub struct PluginInit<T> {
    /// Configuration
    pub config: T,
    /// Router Supergraph Schema (schema definition language)
    pub supergraph_sdl: Arc<String>,
    /// The supergraph schema (parsed)
    pub(crate) supergraph_schema: Arc<Valid<Schema>>,

    /// The parsed subgraph schemas from the query planner, keyed by subgraph name
    pub(crate) subgraph_schemas: Arc<SubgraphSchemas>,

    pub(crate) notify: Notify<String, graphql::Response>,
}

impl<T> PluginInit<T>
where
    T: for<'de> Deserialize<'de>,
{
    #[deprecated = "use PluginInit::builder() instead"]
    /// Create a new PluginInit for the supplied config and SDL.
    pub fn new(config: T, supergraph_sdl: Arc<String>) -> Self {
        Self::builder()
            .config(config)
            .supergraph_schema(Arc::new(
                Schema::parse_and_validate(supergraph_sdl.to_string(), PathBuf::from("synthetic"))
                    .expect("failed to parse supergraph schema"),
            ))
            .supergraph_sdl(supergraph_sdl)
            .notify(Notify::builder().build())
            .build()
    }

    /// Try to create a new PluginInit for the supplied JSON and SDL.
    ///
    /// This will fail if the supplied JSON cannot be deserialized into the configuration
    /// struct.
    #[deprecated = "use PluginInit::try_builder() instead"]
    pub fn try_new(
        config: serde_json::Value,
        supergraph_sdl: Arc<String>,
    ) -> Result<Self, BoxError> {
        Self::try_builder()
            .config(config)
            .supergraph_schema(Arc::new(
                Schema::parse_and_validate(supergraph_sdl.to_string(), PathBuf::from("synthetic"))
                    .map_err(|e| {
                        // This method is deprecated so we're not going to do anything fancy with the error
                        BoxError::from(e.errors.to_string())
                    })?,
            ))
            .supergraph_sdl(supergraph_sdl)
            .notify(Notify::builder().build())
            .build()
    }

    #[cfg(test)]
    pub(crate) fn fake_new(config: T, supergraph_sdl: Arc<String>) -> Self {
        let supergraph_schema = Arc::new(if !supergraph_sdl.is_empty() {
            Schema::parse_and_validate(supergraph_sdl.to_string(), PathBuf::from("synthetic"))
                .expect("failed to parse supergraph schema")
        } else {
            Valid::assume_valid(Schema::new())
        });

        PluginInit::fake_builder()
            .config(config)
            .supergraph_sdl(supergraph_sdl)
            .supergraph_schema(supergraph_schema)
            .notify(Notify::for_tests())
            .build()
    }

    /// Returns the parsed Schema. This is unstable and may be changed or removed in future router releases.
    /// In addition, Schema is not stable, and may be changed or removed in future apollo-rs releases.
    #[doc(hidden)]
    pub fn unsupported_supergraph_schema(&self) -> Arc<Valid<Schema>> {
        self.supergraph_schema.clone()
    }

    /// Returns a mapping of subgraph to parsed schema. This is unstable and may be changed or removed in
    /// future router releases. In addition, Schema is not stable, and may be changed or removed in future
    /// apollo-rs releases.
    #[doc(hidden)]
    pub fn unsupported_subgraph_schemas(&self) -> Arc<HashMap<String, Arc<Valid<Schema>>>> {
        self.subgraph_schemas.clone()
    }
}

#[buildstructor::buildstructor]
impl<T> PluginInit<T>
where
    T: for<'de> Deserialize<'de>,
{
    /// Create a new PluginInit builder
    #[builder(entry = "builder", exit = "build", visibility = "pub")]
    /// Build a new PluginInit for the supplied configuration and SDL.
    ///
    /// You can reuse a notify instance, or Build your own.
    pub(crate) fn new_builder(
        config: T,
        supergraph_sdl: Arc<String>,
        supergraph_schema: Arc<Valid<Schema>>,
        subgraph_schemas: Option<Arc<SubgraphSchemas>>,
        notify: Notify<String, graphql::Response>,
    ) -> Self {
        PluginInit {
            config,
            supergraph_sdl,
            supergraph_schema,
            subgraph_schemas: subgraph_schemas.unwrap_or_default(),
            notify,
        }
    }

    #[builder(entry = "try_builder", exit = "build", visibility = "pub")]
    /// Try to build a new PluginInit for the supplied json configuration and SDL.
    ///
    /// You can reuse a notify instance, or Build your own.
    /// invoking build() will fail if the JSON doesn't comply with the configuration format.
    pub(crate) fn try_new_builder(
        config: serde_json::Value,
        supergraph_sdl: Arc<String>,
        supergraph_schema: Arc<Valid<Schema>>,
        subgraph_schemas: Option<Arc<SubgraphSchemas>>,
        notify: Notify<String, graphql::Response>,
    ) -> Result<Self, BoxError> {
        let config: T = serde_json::from_value(config)?;
        Ok(PluginInit {
            config,
            supergraph_sdl,
            supergraph_schema,
            subgraph_schemas: subgraph_schemas.unwrap_or_default(),
            notify,
        })
    }

    /// Create a new PluginInit builder
    #[builder(entry = "fake_builder", exit = "build", visibility = "pub")]
    fn fake_new_builder(
        config: T,
        supergraph_sdl: Option<Arc<String>>,
        supergraph_schema: Option<Arc<Valid<Schema>>>,
        subgraph_schemas: Option<Arc<SubgraphSchemas>>,
        notify: Option<Notify<String, graphql::Response>>,
    ) -> Self {
        PluginInit {
            config,
            supergraph_sdl: supergraph_sdl.unwrap_or_default(),
            supergraph_schema: supergraph_schema
                .unwrap_or_else(|| Arc::new(Valid::assume_valid(Schema::new()))),
            subgraph_schemas: subgraph_schemas.unwrap_or_default(),
            notify: notify.unwrap_or_else(Notify::for_tests),
        }
    }
}

impl PluginInit<serde_json::Value> {
    /// Attempts to convert the plugin configuration from `serde_json::Value` to the desired type `T`
    pub fn with_deserialized_config<T>(self) -> Result<PluginInit<T>, BoxError>
    where
        T: for<'de> Deserialize<'de>,
    {
        PluginInit::try_builder()
            .config(self.config)
            .supergraph_schema(self.supergraph_schema)
            .supergraph_sdl(self.supergraph_sdl)
            .subgraph_schemas(self.subgraph_schemas)
            .notify(self.notify.clone())
            .build()
    }
}

/// Factories for plugin schema and configuration.
#[derive(Clone)]
pub struct PluginFactory {
    pub(crate) name: String,
    instance_factory: InstanceFactory,
    schema_factory: SchemaFactory,
    pub(crate) type_id: TypeId,
}

impl fmt::Debug for PluginFactory {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PluginFactory")
            .field("name", &self.name)
            .field("type_id", &self.type_id)
            .finish()
    }
}

impl PluginFactory {
    pub(crate) fn is_apollo(&self) -> bool {
        self.name.starts_with("apollo.") || self.name.starts_with("experimental.")
    }

    /// Create a plugin factory.
    pub fn new<P: PluginUnstable>(group: &str, name: &str) -> PluginFactory {
        let plugin_factory_name = if group.is_empty() {
            name.to_string()
        } else {
            format!("{group}.{name}")
        };
        tracing::debug!(%plugin_factory_name, "creating plugin factory");
        PluginFactory {
            name: plugin_factory_name,
            instance_factory: |init| {
                Box::pin(async move {
                    let init = init.with_deserialized_config()?;
                    let plugin = P::new(init).await?;
                    Ok(Box::new(plugin) as Box<dyn DynPlugin>)
                })
            },
            schema_factory: |gen| gen.subschema_for::<<P as PluginUnstable>::Config>(),
            type_id: TypeId::of::<P>(),
        }
    }

    /// Create a plugin factory.
    #[allow(dead_code)]
    pub(crate) fn new_private<P: PluginPrivate>(group: &str, name: &str) -> PluginFactory {
        let plugin_factory_name = if group.is_empty() {
            name.to_string()
        } else {
            format!("{group}.{name}")
        };
        tracing::debug!(%plugin_factory_name, "creating plugin factory");
        PluginFactory {
            name: plugin_factory_name,
            instance_factory: |init| {
                Box::pin(async move {
                    let init = init.with_deserialized_config()?;
                    let plugin = P::new(init).await?;
                    Ok(Box::new(plugin) as Box<dyn DynPlugin>)
                })
            },
            schema_factory: |gen| gen.subschema_for::<<P as PluginPrivate>::Config>(),
            type_id: TypeId::of::<P>(),
        }
    }

    pub(crate) async fn create_instance(
        &self,
        init: PluginInit<serde_json::Value>,
    ) -> Result<Box<dyn DynPlugin>, BoxError> {
        (self.instance_factory)(init).await
    }

    #[cfg(test)]
    pub(crate) async fn create_instance_without_schema(
        &self,
        configuration: &serde_json::Value,
    ) -> Result<Box<dyn DynPlugin>, BoxError> {
        (self.instance_factory)(
            PluginInit::fake_builder()
                .config(configuration.clone())
                .build(),
        )
        .await
    }

    pub(crate) fn create_schema(&self, gen: &mut SchemaGenerator) -> schemars::schema::Schema {
        (self.schema_factory)(gen)
    }
}

// If we wanted to create a custom subset of plugins, this is where we would do it
/// Get a copy of the registered plugin factories.
pub(crate) fn plugins() -> impl Iterator<Item = &'static Lazy<PluginFactory>> {
    PLUGINS.iter()
}

/// All router plugins must implement the Plugin trait.
///
/// This trait defines lifecycle hooks that enable hooking into Apollo Router services.
/// The trait also provides a default implementations for each hook, which returns the associated service unmodified.
/// For more information about the plugin lifecycle please check this documentation <https://www.apollographql.com/docs/router/customizations/native/#plugin-lifecycle>
#[async_trait]
pub trait Plugin: Send + Sync + 'static {
    /// The configuration for this plugin.
    /// Typically a `struct` with `#[derive(serde::Deserialize)]`.
    ///
    /// If a plugin is [registered][register_plugin()!],
    /// it can be enabled through the `plugins` section of Router YAML configuration
    /// by having a sub-section named after the plugin.
    /// The contents of this section are deserialized into this `Config` type
    /// and passed to [`Plugin::new`] as part of [`PluginInit`].
    type Config: JsonSchema + DeserializeOwned + Send;

    /// This is invoked once after the router starts and compiled-in
    /// plugins are registered.
    async fn new(init: PluginInit<Self::Config>) -> Result<Self, BoxError>
    where
        Self: Sized;

    /// This function is EXPERIMENTAL and its signature is subject to change.
    ///
    /// This service runs at the very beginning and very end of the request lifecycle.
    /// It's the entrypoint of every requests and also the last hook before sending the response.
    /// Define supergraph_service if your customization needs to interact at the earliest or latest point possible.
    /// For example, this is a good opportunity to perform JWT verification before allowing a request to proceed further.
    fn router_service(&self, service: router::BoxService) -> router::BoxService {
        service
    }

    /// This service runs after the HTTP request payload has been deserialized into a GraphQL request,
    /// and before the GraphQL response payload is serialized into a raw HTTP response.
    /// Define supergraph_service if your customization needs to interact at the earliest or latest point possible, yet operates on GraphQL payloads.
    fn supergraph_service(&self, service: supergraph::BoxService) -> supergraph::BoxService {
        service
    }

    /// This service handles initiating the execution of a query plan after it's been generated.
    /// Define `execution_service` if your customization includes logic to govern execution (for example, if you want to block a particular query based on a policy decision).
    fn execution_service(&self, service: execution::BoxService) -> execution::BoxService {
        service
    }

    /// This service handles communication between the Apollo Router and your subgraphs.
    /// Define `subgraph_service` to configure this communication (for example, to dynamically add headers to pass to a subgraph).
    /// The `_subgraph_name` parameter is useful if you need to apply a customization only specific subgraphs.
    fn subgraph_service(
        &self,
        _subgraph_name: &str,
        service: subgraph::BoxService,
    ) -> subgraph::BoxService {
        service
    }

    /// Return the name of the plugin.
    fn name(&self) -> &'static str
    where
        Self: Sized,
    {
        get_type_of(self)
    }

    /// Return one or several `Endpoint`s and `ListenAddr` and the router will serve your custom web Endpoint(s).
    ///
    /// This method is experimental and subject to change post 1.0
    fn web_endpoints(&self) -> MultiMap<ListenAddr, Endpoint> {
        MultiMap::new()
    }
}

/// Plugin trait for unstable features
///
/// This trait defines lifecycle hooks that enable hooking into Apollo Router services. The hooks that are not already defined
/// in the [Plugin] trait are not considered stable and may change at any moment.
/// The trait also provides a default implementations for each hook, which returns the associated service unmodified.
/// For more information about the plugin lifecycle please check this documentation <https://www.apollographql.com/docs/router/customizations/native/#plugin-lifecycle>
#[async_trait]
pub trait PluginUnstable: Send + Sync + 'static {
    /// The configuration for this plugin.
    /// Typically a `struct` with `#[derive(serde::Deserialize)]`.
    ///
    /// If a plugin is [registered][register_plugin()!],
    /// it can be enabled through the `plugins` section of Router YAML configuration
    /// by having a sub-section named after the plugin.
    /// The contents of this section are deserialized into this `Config` type
    /// and passed to [`Plugin::new`] as part of [`PluginInit`].
    type Config: JsonSchema + DeserializeOwned + Send;

    /// This is invoked once after the router starts and compiled-in
    /// plugins are registered.
    async fn new(init: PluginInit<Self::Config>) -> Result<Self, BoxError>
    where
        Self: Sized;

    /// This function is EXPERIMENTAL and its signature is subject to change.
    ///
    /// This service runs at the very beginning and very end of the request lifecycle.
    /// It's the entrypoint of every requests and also the last hook before sending the response.
    /// Define supergraph_service if your customization needs to interact at the earliest or latest point possible.
    /// For example, this is a good opportunity to perform JWT verification before allowing a request to proceed further.
    fn router_service(&self, service: router::BoxService) -> router::BoxService {
        service
    }

    /// This service runs after the HTTP request payload has been deserialized into a GraphQL request,
    /// and before the GraphQL response payload is serialized into a raw HTTP response.
    /// Define supergraph_service if your customization needs to interact at the earliest or latest point possible, yet operates on GraphQL payloads.
    fn supergraph_service(&self, service: supergraph::BoxService) -> supergraph::BoxService {
        service
    }

    /// This service handles initiating the execution of a query plan after it's been generated.
    /// Define `execution_service` if your customization includes logic to govern execution (for example, if you want to block a particular query based on a policy decision).
    fn execution_service(&self, service: execution::BoxService) -> execution::BoxService {
        service
    }

    /// This service handles communication between the Apollo Router and your subgraphs.
    /// Define `subgraph_service` to configure this communication (for example, to dynamically add headers to pass to a subgraph).
    /// The `_subgraph_name` parameter is useful if you need to apply a customization only specific subgraphs.
    fn subgraph_service(
        &self,
        _subgraph_name: &str,
        service: subgraph::BoxService,
    ) -> subgraph::BoxService {
        service
    }

    /// Return the name of the plugin.
    fn name(&self) -> &'static str
    where
        Self: Sized,
    {
        get_type_of(self)
    }

    /// Return one or several `Endpoint`s and `ListenAddr` and the router will serve your custom web Endpoint(s).
    ///
    /// This method is experimental and subject to change post 1.0
    fn web_endpoints(&self) -> MultiMap<ListenAddr, Endpoint> {
        MultiMap::new()
    }

    /// test
    fn unstable_method(&self);
}

#[async_trait]
impl<P> PluginUnstable for P
where
    P: Plugin,
{
    type Config = <P as Plugin>::Config;

    async fn new(init: PluginInit<Self::Config>) -> Result<Self, BoxError>
    where
        Self: Sized,
    {
        Plugin::new(init).await
    }

    fn router_service(&self, service: router::BoxService) -> router::BoxService {
        Plugin::router_service(self, service)
    }

    fn supergraph_service(&self, service: supergraph::BoxService) -> supergraph::BoxService {
        Plugin::supergraph_service(self, service)
    }

    fn execution_service(&self, service: execution::BoxService) -> execution::BoxService {
        Plugin::execution_service(self, service)
    }

    fn subgraph_service(
        &self,
        subgraph_name: &str,
        service: subgraph::BoxService,
    ) -> subgraph::BoxService {
        Plugin::subgraph_service(self, subgraph_name, service)
    }

    /// Return the name of the plugin.
    fn name(&self) -> &'static str
    where
        Self: Sized,
    {
        Plugin::name(self)
    }

    fn web_endpoints(&self) -> MultiMap<ListenAddr, Endpoint> {
        Plugin::web_endpoints(self)
    }

    fn unstable_method(&self) {
        todo!()
    }
}

/// Internal Plugin trait
///
/// This trait defines lifecycle hooks that enable hooking into Apollo Router services. The hooks that are not already defined
/// in the [Plugin] or [PluginUnstable] traits are internal hooks not yet open to public usage. This allows testing of new plugin
/// hooks without committing to their API right away.
/// The trait also provides a default implementations for each hook, which returns the associated service unmodified.
/// For more information about the plugin lifecycle please check this documentation <https://www.apollographql.com/docs/router/customizations/native/#plugin-lifecycle>
#[async_trait]
pub(crate) trait PluginPrivate: Send + Sync + 'static {
    /// The configuration for this plugin.
    /// Typically a `struct` with `#[derive(serde::Deserialize)]`.
    ///
    /// If a plugin is [registered][register_plugin()!],
    /// it can be enabled through the `plugins` section of Router YAML configuration
    /// by having a sub-section named after the plugin.
    /// The contents of this section are deserialized into this `Config` type
    /// and passed to [`Plugin::new`] as part of [`PluginInit`].
    type Config: JsonSchema + DeserializeOwned + Send;

    /// This is invoked once after the router starts and compiled-in
    /// plugins are registered.
    async fn new(init: PluginInit<Self::Config>) -> Result<Self, BoxError>
    where
        Self: Sized;

    /// This function is EXPERIMENTAL and its signature is subject to change.
    ///
    /// This service runs at the very beginning and very end of the request lifecycle.
    /// It's the entrypoint of every requests and also the last hook before sending the response.
    /// Define supergraph_service if your customization needs to interact at the earliest or latest point possible.
    /// For example, this is a good opportunity to perform JWT verification before allowing a request to proceed further.
    fn router_service(&self, service: router::BoxService) -> router::BoxService {
        service
    }

    /// This service runs after the HTTP request payload has been deserialized into a GraphQL request,
    /// and before the GraphQL response payload is serialized into a raw HTTP response.
    /// Define supergraph_service if your customization needs to interact at the earliest or latest point possible, yet operates on GraphQL payloads.
    fn supergraph_service(&self, service: supergraph::BoxService) -> supergraph::BoxService {
        service
    }

    /// This service handles initiating the execution of a query plan after it's been generated.
    /// Define `execution_service` if your customization includes logic to govern execution (for example, if you want to block a particular query based on a policy decision).
    fn execution_service(&self, service: execution::BoxService) -> execution::BoxService {
        service
    }

    /// This service handles communication between the Apollo Router and your subgraphs.
    /// Define `subgraph_service` to configure this communication (for example, to dynamically add headers to pass to a subgraph).
    /// The `_subgraph_name` parameter is useful if you need to apply a customization only specific subgraphs.
    fn subgraph_service(
        &self,
        _subgraph_name: &str,
        service: subgraph::BoxService,
    ) -> subgraph::BoxService {
        service
    }

    /// This service handles HTTP communication
    fn http_client_service(
        &self,
        _subgraph_name: &str,
        service: crate::services::http::BoxService,
    ) -> crate::services::http::BoxService {
        service
    }

    /// Return the name of the plugin.
    fn name(&self) -> &'static str
    where
        Self: Sized,
    {
        get_type_of(self)
    }

    /// Return one or several `Endpoint`s and `ListenAddr` and the router will serve your custom web Endpoint(s).
    ///
    /// This method is experimental and subject to change post 1.0
    fn web_endpoints(&self) -> MultiMap<ListenAddr, Endpoint> {
        MultiMap::new()
    }
}

#[async_trait]
impl<P> PluginPrivate for P
where
    P: PluginUnstable,
{
    type Config = <P as PluginUnstable>::Config;

    async fn new(init: PluginInit<Self::Config>) -> Result<Self, BoxError>
    where
        Self: Sized,
    {
        PluginUnstable::new(init).await
    }

    fn router_service(&self, service: router::BoxService) -> router::BoxService {
        PluginUnstable::router_service(self, service)
    }

    fn supergraph_service(&self, service: supergraph::BoxService) -> supergraph::BoxService {
        PluginUnstable::supergraph_service(self, service)
    }

    fn execution_service(&self, service: execution::BoxService) -> execution::BoxService {
        PluginUnstable::execution_service(self, service)
    }

    fn subgraph_service(
        &self,
        subgraph_name: &str,
        service: subgraph::BoxService,
    ) -> subgraph::BoxService {
        PluginUnstable::subgraph_service(self, subgraph_name, service)
    }

    /// Return the name of the plugin.
    fn name(&self) -> &'static str
    where
        Self: Sized,
    {
        PluginUnstable::name(self)
    }

    fn web_endpoints(&self) -> MultiMap<ListenAddr, Endpoint> {
        PluginUnstable::web_endpoints(self)
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
pub(crate) trait DynPlugin: Send + Sync + 'static {
    /// This service runs at the very beginning and very end of the request lifecycle.
    /// It's the entrypoint of every requests and also the last hook before sending the response.
    /// Define supergraph_service if your customization needs to interact at the earliest or latest point possible.
    /// For example, this is a good opportunity to perform JWT verification before allowing a request to proceed further.
    fn router_service(&self, service: router::BoxService) -> router::BoxService;

    /// This service runs after the HTTP request payload has been deserialized into a GraphQL request,
    /// and before the GraphQL response payload is serialized into a raw HTTP response.
    /// Define supergraph_service if your customization needs to interact at the earliest or latest point possible, yet operates on GraphQL payloads.
    fn supergraph_service(&self, service: supergraph::BoxService) -> supergraph::BoxService;

    /// This service handles initiating the execution of a query plan after it's been generated.
    /// Define `execution_service` if your customization includes logic to govern execution (for example, if you want to block a particular query based on a policy decision).
    fn execution_service(&self, service: execution::BoxService) -> execution::BoxService;

    /// This service handles communication between the Apollo Router and your subgraphs.
    /// Define `subgraph_service` to configure this communication (for example, to dynamically add headers to pass to a subgraph).
    /// The `_subgraph_name` parameter is useful if you need to apply a customization only on specific subgraphs.
    fn subgraph_service(
        &self,
        _subgraph_name: &str,
        service: subgraph::BoxService,
    ) -> subgraph::BoxService;

    /// This service handles HTTP communication
    fn http_client_service(
        &self,
        _subgraph_name: &str,
        service: crate::services::http::BoxService,
    ) -> crate::services::http::BoxService;

    /// Return the name of the plugin.
    fn name(&self) -> &'static str;

    /// Return one or several `Endpoint`s and `ListenAddr` and the router will serve your custom web Endpoint(s).
    fn web_endpoints(&self) -> MultiMap<ListenAddr, Endpoint>;

    /// Support downcasting
    fn as_any(&self) -> &dyn std::any::Any;

    /// Support downcasting
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any;
}

#[async_trait]
impl<T> DynPlugin for T
where
    T: PluginPrivate,
    for<'de> <T as PluginPrivate>::Config: Deserialize<'de>,
{
    fn router_service(&self, service: router::BoxService) -> router::BoxService {
        self.router_service(service)
    }

    fn supergraph_service(&self, service: supergraph::BoxService) -> supergraph::BoxService {
        self.supergraph_service(service)
    }

    fn execution_service(&self, service: execution::BoxService) -> execution::BoxService {
        self.execution_service(service)
    }

    fn subgraph_service(&self, name: &str, service: subgraph::BoxService) -> subgraph::BoxService {
        self.subgraph_service(name, service)
    }

    /// This service handles HTTP communication
    fn http_client_service(
        &self,
        name: &str,
        service: crate::services::http::BoxService,
    ) -> crate::services::http::BoxService {
        self.http_client_service(name, service)
    }

    fn name(&self) -> &'static str {
        self.name()
    }

    /// Return one or several `Endpoint`s and `ListenAddr` and the router will serve your custom web Endpoint(s).
    fn web_endpoints(&self) -> MultiMap<ListenAddr, Endpoint> {
        self.web_endpoints()
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }
}

/// Register a plugin with a group and a name
/// Grouping prevent name clashes for plugins, so choose something unique, like your domain name.
/// Plugins will appear in the configuration as a layer property called: {group}.{name}
#[macro_export]
macro_rules! register_plugin {
    ($group: literal, $name: literal, $plugin_type: ident <  $generic: ident >) => {
        //  Artificial scope to avoid naming collisions
        const _: () = {
            use $crate::_private::once_cell::sync::Lazy;
            use $crate::_private::PluginFactory;
            use $crate::_private::PLUGINS;

            #[$crate::_private::linkme::distributed_slice(PLUGINS)]
            #[linkme(crate = $crate::_private::linkme)]
            static REGISTER_PLUGIN: Lazy<PluginFactory> = Lazy::new(|| {
                $crate::plugin::PluginFactory::new::<$plugin_type<$generic>>($group, $name)
            });
        };
    };

    ($group: literal, $name: literal, $plugin_type: ident) => {
        //  Artificial scope to avoid naming collisions
        const _: () = {
            use $crate::_private::once_cell::sync::Lazy;
            use $crate::_private::PluginFactory;
            use $crate::_private::PLUGINS;

            #[$crate::_private::linkme::distributed_slice(PLUGINS)]
            #[linkme(crate = $crate::_private::linkme)]
            static REGISTER_PLUGIN: Lazy<PluginFactory> =
                Lazy::new(|| $crate::plugin::PluginFactory::new::<$plugin_type>($group, $name));
        };
    };
}

/// Register a private plugin with a group and a name
/// Grouping prevent name clashes for plugins, so choose something unique, like your domain name.
/// Plugins will appear in the configuration as a layer property called: {group}.{name}
#[macro_export]
macro_rules! register_private_plugin {
    ($group: literal, $name: literal, $plugin_type: ident <  $generic: ident >) => {
        //  Artificial scope to avoid naming collisions
        const _: () = {
            use $crate::_private::once_cell::sync::Lazy;
            use $crate::_private::PluginFactory;
            use $crate::_private::PLUGINS;

            #[$crate::_private::linkme::distributed_slice(PLUGINS)]
            #[linkme(crate = $crate::_private::linkme)]
            static REGISTER_PLUGIN: Lazy<PluginFactory> = Lazy::new(|| {
                $crate::plugin::PluginFactory::new_private::<$plugin_type<$generic>>($group, $name)
            });
        };
    };

    ($group: literal, $name: literal, $plugin_type: ident) => {
        //  Artificial scope to avoid naming collisions
        const _: () = {
            use $crate::_private::once_cell::sync::Lazy;
            use $crate::_private::PluginFactory;
            use $crate::_private::PLUGINS;

            #[$crate::_private::linkme::distributed_slice(PLUGINS)]
            #[linkme(crate = $crate::_private::linkme)]
            static REGISTER_PLUGIN: Lazy<PluginFactory> = Lazy::new(|| {
                $crate::plugin::PluginFactory::new_private::<$plugin_type>($group, $name)
            });
        };
    };
}

/// Handler represents a [`Plugin`] endpoint.
#[derive(Clone)]
pub(crate) struct Handler {
    service: Buffer<router::BoxService, router::Request>,
}

impl Handler {
    pub(crate) fn new(service: router::BoxService) -> Self {
        Self {
            service: ServiceBuilder::new().buffered().service(service),
        }
    }
}

impl Service<router::Request> for Handler {
    type Response = router::Response;
    type Error = BoxError;
    type Future = ResponseFuture<BoxFuture<'static, Result<Self::Response, Self::Error>>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.service.poll_ready(cx)
    }

    fn call(&mut self, req: router::Request) -> Self::Future {
        self.service.call(req)
    }
}

impl From<router::BoxService> for Handler {
    fn from(original: router::BoxService) -> Self {
        Self::new(original)
    }
}
