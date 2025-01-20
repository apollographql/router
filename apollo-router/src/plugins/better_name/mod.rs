// TODO: docs
pub(crate) mod tps;

use std::num::NonZeroU64;
use std::time::Duration;

use futures::future::BoxFuture;
use futures::FutureExt;
use http::StatusCode;
use schemars::JsonSchema;
use serde::Deserialize;
use tower::BoxError;
use tower::Service;
use tower::ServiceBuilder;

use crate::graphql;
use crate::layers::ServiceBuilderExt;
use crate::plugin::PluginInit;
use crate::plugin::PluginPrivate;
use crate::plugins::better_name::tps::TpsLimitLayer;
use crate::plugins::better_name::tps::TpsLimited;
use crate::services::supergraph;
use crate::uplink::license_enforcement::LicenseState;

pub(crate) const APOLLO_ROUTER_LIMITS: &str = "apollo.router_limits";
// TODO: better name and actual default; align on defaulting strategy, especially whether to have
// consts in default impl for TpsLimit
const SOME_SENSIBLE_DEFAULT_SET_BY_PRODUCT: u128 = 1000;

#[derive(PartialEq, Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct RouterLimits {
    tps: Option<TpsLimitConfig>,
}

#[derive(PartialEq, Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct TpsLimitConfig {
    capacity: NonZeroU64,
    #[serde(deserialize_with = "humantime_serde::deserialize")]
    #[schemars(with = "String")]
    interval: Duration,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub(crate) struct RouterLimitsConfig {
    tps: TpsLimitConfig,
}

#[async_trait::async_trait]
impl PluginPrivate for RouterLimits {
    type Config = RouterLimitsConfig;

    async fn new(plugin: PluginInit<Self::Config>) -> Result<Self, BoxError> {
        tracing::debug!("initializing the RouterLimits plugin");
        // TODO: decide whether to check anything about capacity (max, etc)
        let capacity = plugin.config.tps.capacity;
        let mut interval = plugin.config.tps.interval.as_millis();
        if interval > u64::MAX as u128 {
            tracing::warn!("invalid TPS interval: {interval}. Using {SOME_SENSIBLE_DEFAULT_SET_BY_PRODUCT} instead.");
            interval = SOME_SENSIBLE_DEFAULT_SET_BY_PRODUCT;
        }
        Ok(Self {
            tps: Some(TpsLimitConfig {
                capacity,
                // FIXME: unwrap
                interval: Duration::from_millis(interval.try_into().unwrap()),
            }),
        })
    }
}

impl RouterLimits {
    pub(crate) fn supergraph_service_internal<S>(
        &self,
        service: S,
        license: LicenseState,
    ) -> impl Service<
        supergraph::Request,
        Response = supergraph::Response,
        Error = BoxError,
        Future = BoxFuture<'static, Result<supergraph::Response, BoxError>>,
    > + Clone
           + Send
           + Sync
           + 'static
    where
        S: Service<supergraph::Request, Response = supergraph::Response, Error = BoxError>
            + Clone
            + Send
            + Sync
            + 'static,
        <S as Service<supergraph::Request>>::Future: std::marker::Send,
    {
        // TODO: doublecheck defaults
        let limits = license.get_limits().unwrap_or_default();

        ServiceBuilder::new()
            .map_future_with_request_data(
                |req: &supergraph::Request| req.context.clone(),
                move |ctx, future| {
                    async {
                        let response: Result<supergraph::Response, BoxError> = future.await;
                        match response {
                            Err(error) if error.is::<TpsLimited>() => {
                                supergraph::Response::error_builder()
                                    // TODO: decide if 429 still right (seems right)
                                    .status_code(StatusCode::TOO_MANY_REQUESTS)
                                    .error::<graphql::Error>(TpsLimited::new().into())
                                    .context(ctx)
                                    .build()
                            }
                            _ => response,
                        }
                    }
                    .boxed()
                },
            )
            .layer(TpsLimitLayer::new(
                // FIXME: unwrap
                NonZeroU64::new(limits.tps.unwrap_or_default().capacity.try_into().unwrap())
                    // FIXME: unwrap
                    .unwrap(),
                limits.tps.unwrap_or_default().interval,
            ))
            .service(service)
    }
}

register_private_plugin!("apollo", "router_limits", RouterLimits);
