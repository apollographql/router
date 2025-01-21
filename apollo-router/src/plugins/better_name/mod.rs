// TODO: mod docs
use std::num::NonZeroU64;
use std::time::Duration;

use http::StatusCode;
use schemars::JsonSchema;
use serde::Deserialize;
use tower::limit::RateLimitLayer;
use tower::load_shed::error::Overloaded;
use tower::BoxError;
use tower::ServiceBuilder;
use tower::ServiceExt;

use crate::graphql;
use crate::layers::ServiceBuilderExt;
use crate::plugin::PluginInit;
use crate::plugin::PluginPrivate;
use crate::services::router;
use crate::services::RouterResponse;

pub(crate) const APOLLO_ROUTER_LIMITS: &str = "apollo.traffic_shaping";

#[derive(PartialEq, Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct RouterLimits {
    pub(crate) tps: TpsLimitConf,
}

#[derive(PartialEq, Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct TpsLimitConf {
    pub(crate) capacity: NonZeroU64,
    #[serde(deserialize_with = "humantime_serde::deserialize")]
    #[schemars(with = "String")]
    pub(crate) interval: Duration,
}

#[async_trait::async_trait]
impl PluginPrivate for RouterLimits {
    type Config = ();

    async fn new(init: PluginInit<Self::Config>) -> Result<Self, BoxError> {
        let limits = if let Some(limits) = init.license.get_limits() {
            limits
        } else {
            tracing::warn!("License limits found during plugin initialization but failed to get_limits() during plugin initialization");
            // FIXME: panic, figure out whether to default here or to where
            panic!()
        };

        Ok(Self {
            tps: TpsLimitConf {
                // FIXME: unwrap
                capacity: NonZeroU64::new(limits.tps.unwrap().capacity as u64).unwrap(),
                // FIXME: unwrap
                interval: limits.tps.unwrap().interval,
            },
        })
    }

    fn router_service(&self, service: router::BoxService) -> router::BoxService {
        ServiceBuilder::new()
            .map_future_with_request_data(
                |req: &router::Request| req.context.clone(),
                move |ctx, future| {
                    async {
                        let response: Result<RouterResponse, BoxError> = future.await;
                        match response {
                            Ok(ok) => Ok(ok),
                            Err(err) if err.is::<Overloaded>() => {
                                // TODO: add metrics
                                let error = graphql::Error::builder()
                                    // TODO: better product messaging
                                    .message("Your request has been rate limited")
                                    // TODO: better extension to distinguish between user- and
                                    // apollo-set limits
                                    .extension_code("REQUEST_RATE_LIMITED")
                                    .build();
                                Ok(RouterResponse::error_builder()
                                    .status_code(StatusCode::SERVICE_UNAVAILABLE)
                                    .error(error)
                                    .context(ctx)
                                    .build()
                                    .expect("should build overloaded response"))
                            }
                            Err(err) => Err(err),
                        }
                    }
                },
            )
            .load_shed()
            .layer(RateLimitLayer::new(
                self.tps.capacity.into(),
                self.tps.interval,
            ))
            .service(service)
            .boxed()
    }
}

register_private_plugin!("apollo", "router_limits", RouterLimits);
