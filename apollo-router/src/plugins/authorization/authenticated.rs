//! Authorization plugin
// With regards to ELv2 licensing, this entire file is license key functionality

use schemars::JsonSchema;
use serde::Deserialize;
use tower::{BoxError, ServiceBuilder};

use crate::{
    layers::ServiceBuilderExt,
    plugin::{Plugin, PluginInit},
    services::execution,
};

pub(crate) const AUTHORIZATION_SPAN_NAME: &str = "authorization_plugin";
/*
struct AuthenticatedDirectivePlugin {}

#[derive(Deserialize, JsonSchema)]
struct Config {}

#[async_trait::async_trait]
impl Plugin for AuthenticatedDirectivePlugin {
    type Config = Config;

    async fn new(init: PluginInit<Self::Config>) -> Result<Self, BoxError> {
        Ok(AuthenticatedDirectivePlugin {})
    }

    fn execution_service(&self, service: execution::BoxService) -> execution::BoxService {
        fn authorization_service_span() -> impl Fn(&execution::Request) -> tracing::Span + Clone {
            move |_request: &execution::Request| {
                tracing::info_span!(
                    AUTHORIZATION_SPAN_NAME,
                    "authorization service" = stringify!(router::Request),
                    "otel.kind" = "INTERNAL"
                )
            }
        }

        ServiceBuilder::new()
            .instrument(authorization_service_span())
            .checkpoint(move |request: execution::Request| {})
            //.buffered()
            .service(service)
            .boxed()
    }
}

register_plugin!("apollo", "authentication", AuthenticatedDirectivePlugin);
*/
