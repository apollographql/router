use crate::configuration::{Configuration, ConfigurationError};
use crate::reqwest_subgraph_service::ReqwestSubgraphService;
use apollo_router_core::{
    http_compat::{Request, Response},
    PluggableRouterServiceBuilder, ResponseBody, RouterRequest, Schema,
};
use apollo_router_core::{prelude::*, Context};
use std::sync::Arc;
use tower::buffer::Buffer;
use tower::util::{BoxCloneService, BoxService};
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
                match layer.as_object().and_then(|o| o.iter().next()) {
                    Some((kind, config)) => {
                        match apollo_router_core::layers().get(kind) {
                            None => {
                                errors.push(ConfigurationError::LayerUnknown(kind.to_owned()));
                            }
                            Some(factory) => match factory.create_instance(config) {
                                Ok(layer) => subgraph_service = layer.layer(subgraph_service),
                                Err(err) => errors.push(ConfigurationError::LayerConfiguration {
                                    layer: kind.to_string(),
                                    error: err.to_string(),
                                }),
                            },
                        }
                    }
                    None => errors.push(ConfigurationError::LayerConfiguration {
                        layer: "unknown".into(),
                        error: "layer must be an object".into(),
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
                    .map_request(|http_request: Request<apollo_router_core::Request>| {
                        RouterRequest {
                            context: Context::new().with_request(http_request),
                        }
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

#[cfg(test)]
mod test {
    use crate::router_factory::RouterServiceFactory;
    use crate::{Configuration, YamlRouterServiceFactory};
    use apollo_router_core::Schema;
    use std::sync::Arc;
    use tower_http::BoxError;

    #[tokio::test]
    async fn test_yaml_no_extras() {
        let config = Configuration::builder().build();
        let service = create_service(config).await;
        assert!(service.is_ok())
    }

    #[tokio::test]
    #[test_log::test]
    async fn test_yaml_layers() {
        let config: Configuration = serde_yaml::from_str(
            r#"
            subgraphs:
                foo:
                    routing_url: https://foo
                    layers:
                        - headers_insert:
                              name: "foo"
                              value: "foo"
                            
        "#,
        )
        .unwrap();
        let service = create_service(config).await;
        assert!(service.is_ok())
    }

    async fn create_service(config: Configuration) -> Result<(), BoxError> {
        let schema: Schema = r#"schema
        @core(feature: "https://specs.apollo.dev/core/v0.1"),
        @core(feature: "https://specs.apollo.dev/join/v0.1")
        {
        query: Query
        mutation: Mutation
        }"#
        .parse()
        .unwrap();

        let service = YamlRouterServiceFactory::default()
            .create(Arc::new(config), Arc::new(schema), None)
            .await;
        service.map(|_| ())
    }
}
