use crate::configuration::{Configuration, ConfigurationError};
use crate::reqwest_subgraph_service::ReqwestSubgraphService;
use apollo_router_core::header_manipulation::HeaderManipulationLayer;
use apollo_router_core::prelude::*;
use apollo_router_core::{
    http_compat::{Request, Response},
    Context, PluggableRouterServiceBuilder, ResponseBody, RouterRequest, Schema,
};
use http::header::HeaderName;
use serde_json::Value;
use std::str::FromStr;
use std::sync::Arc;
use tower::buffer::Buffer;
use tower::util::{BoxCloneService, BoxLayer, BoxService};
use tower::{BoxError, Layer, ServiceBuilder, ServiceExt};
use tower_service::Service;
use tracing::instrument::WithSubscriber;

/// Factory for creating a RouterService
///
/// Instances of this traits are used by the StateMachine to generate a new
/// RouterService from configuration when it changes
#[async_trait::async_trait]
pub trait RouterServiceFactory: Send + Sync + 'static {
    type RouterService: Service<
            Request<graphql::Request>,
            Response = Response<ResponseBody>,
            Error = BoxError,
            Future = Self::Future,
        > + Send
        + Sync
        + Clone
        + 'static;
    type Future: Send;

    async fn create<'a>(
        &'a self,
        configuration: Arc<Configuration>,
        schema: Arc<graphql::Schema>,
        previous_router: Option<&'a Self::RouterService>,
    ) -> Result<Self::RouterService, BoxError>;
}

/// Main implementation of the RouterService factory, supporting the extensions system
#[derive(Default)]
pub struct YamlRouterServiceFactory {}

#[async_trait::async_trait]
impl RouterServiceFactory for YamlRouterServiceFactory {
    type RouterService = Buffer<
        BoxCloneService<Request<graphql::Request>, Response<ResponseBody>, BoxError>,
        Request<graphql::Request>,
    >;
    type Future = <Self::RouterService as Service<Request<graphql::Request>>>::Future;

    async fn create<'a>(
        &'a self,
        configuration: Arc<Configuration>,
        schema: Arc<Schema>,
        _previous_router: Option<&'a Self::RouterService>,
    ) -> Result<Self::RouterService, BoxError> {
        let mut errors: Vec<ConfigurationError> = Vec::default();
        let mut configuration = (*configuration).clone();
        if let Err(mut e) = configuration.load_subgraphs(&schema) {
            errors.append(&mut e);
        }

        let dispatcher = configuration
            .subscriber
            .clone()
            .map(tracing::Dispatch::new)
            .unwrap_or_default();
        let buffer = 20000;
        let mut builder = PluggableRouterServiceBuilder::new(schema, buffer, dispatcher.clone());

        for (name, subgraph) in &configuration.subgraphs {
            let mut subgraph_service = BoxService::new(ReqwestSubgraphService::new(
                name.to_string(),
                subgraph.routing_url.clone(),
            ));

            for layer in &subgraph.layers {
                match layer.get("kind") {
                    Some(Value::String(kind)) if kind == "header" => {
                        if let Some(header_name) =
                            layer.get("propagate").as_ref().and_then(|v| v.as_str())
                        {
                            subgraph_service = BoxLayer::new(HeaderManipulationLayer::propagate(
                                HeaderName::from_str(header_name).unwrap(),
                            ))
                            .layer(subgraph_service);
                        }
                    }
                    Some(_) => errors.push(ConfigurationError::LayerConfiguration {
                        layer: "unknown".into(),
                        error: "'kind' must be a string.".into(),
                    }),
                    _ => errors.push(ConfigurationError::LayerConfiguration {
                        layer: "unknown".into(),
                        error: "'kind' missing".into(),
                    }),
                }
            }

            builder = builder.with_subgraph_service(name, subgraph_service);
        }
        {
            let plugin_registry = apollo_router_core::plugins();
            for (name, configuration) in &configuration.plugins {
                let name = name.as_str().to_string();
                match plugin_registry.get(name.as_str()) {
                    Some(factory) => {
                        let mut plugin = (*factory)();
                        match plugin.configure(configuration) {
                            Ok(_) => {
                                builder = builder.with_dyn_plugin(plugin);
                            }
                            Err(err) => {
                                errors.push(ConfigurationError::PluginConfiguration {
                                    plugin: name,
                                    error: err.to_string(),
                                });
                            }
                        }
                    }
                    None => {
                        errors.push(ConfigurationError::PluginUnknown(name));
                    }
                }
            }
        }

        if !errors.is_empty() {
            for error in errors {
                tracing::error!("{:#}", error);
            }
            return Err(Box::new(ConfigurationError::InvalidConfiguration));
        }

        let (service, worker) = Buffer::pair(
            ServiceBuilder::new().service(
                builder
                    .build()
                    .await
                    .map_request(|http_request| RouterRequest {
                        http_request,
                        context: Context::new(),
                    })
                    .map_response(|response| response.response)
                    .boxed_clone(),
            ),
            buffer,
        );
        tokio::spawn(worker.with_subscriber(dispatcher));
        Ok(service)
    }
}
