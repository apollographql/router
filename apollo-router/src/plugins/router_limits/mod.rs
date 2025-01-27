//! A plugin for enforcing product limitations in the router based on License claims
//!
//! Currently includes:
//! * TPS Rate Limiting: a certain threshold, set via License claim, for how many operations over a certain interval can be serviced

use std::num::NonZeroU64;
use std::time::Duration;

use http::StatusCode;
use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;
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

#[derive(PartialEq, Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
/// The limits placed on a router in virtue of what's in a user's license
pub(crate) struct RouterLimits {
    /// Transactions per second allowed based on license tier
    pub(crate) tps: Option<TpsLimitConf>,
}

#[derive(PartialEq, Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
/// Configuration for transactions per second
pub(crate) struct TpsLimitConf {
    /// The number of operations allowed during a certain interval
    pub(crate) capacity: NonZeroU64,
    /// The interval as specied in the user's license; this is in milliseconds
    #[serde(deserialize_with = "humantime_serde::deserialize")]
    #[schemars(with = "String")]
    pub(crate) interval: Duration,
}

#[derive(Debug, Default, Deserialize, JsonSchema, Serialize)]
pub(crate) struct RouterLimitsConfig {}

#[async_trait::async_trait]
impl PluginPrivate for RouterLimits {
    type Config = RouterLimitsConfig;

    async fn new(init: PluginInit<Self::Config>) -> Result<Self, BoxError> {
        let tps = init.license.get_limits().and_then(|limits| {
            limits.tps.and_then(|tps| {
                NonZeroU64::new(tps.capacity as u64).map(|capacity| TpsLimitConf {
                    capacity,
                    interval: tps.interval,
                })
            })
        });

        Ok(Self { tps })
    }

    fn router_service(&self, service: router::BoxService) -> router::BoxService {
        ServiceBuilder::new()
            .map_future_with_request_data(
                |req: &router::Request| req.context.clone(),
                move |ctx, future| async {
                    let response: Result<RouterResponse, BoxError> = future.await;
                    match response {
                        Ok(ok) => Ok(ok),
                        Err(err) if err.is::<Overloaded>() => {
                            u64_counter!(
                                "apollo.router.product_limits.tps.enforced",
                                "TPS Rate Limiting triggered",
                                1u64
                            );
                            let error = graphql::Error::builder()
                                .message("Your request has been rate limited")
                                .extension_code("ROUTER_TPS_LIMIT_REACHED")
                                .build();
                            Ok(RouterResponse::error_builder()
                                .status_code(StatusCode::TOO_MANY_REQUESTS)
                                .error(error)
                                .context(ctx)
                                .build()
                                .expect("should build overloaded response"))
                        }
                        Err(err) => Err(err),
                    }
                },
            )
            .load_shed()
            .option_layer(
                self.tps
                    .as_ref()
                    .map(|config| RateLimitLayer::new(config.capacity.into(), config.interval)),
            )
            .service(service)
            .boxed()
    }
}

register_private_plugin!("apollo", "router_limits", RouterLimits);

#[cfg(test)]
mod test {
    use super::*;
    use crate::metrics::FutureMetricsExt;
    use crate::plugins::test::PluginTestHarness;
    use crate::uplink::license_enforcement::LicenseLimits;
    use crate::uplink::license_enforcement::LicenseState;
    use crate::uplink::license_enforcement::TpsLimit;

    #[tokio::test(flavor = "multi_thread")]
    async fn it_enforces_tps_limit_when_license() {
        // GIVEN
        // * a license with tps limits set to 1 req per 200ms
        // * the router limits plugin
        let license = LicenseState::Licensed {
            limits: Some(LicenseLimits {
                tps: Some(TpsLimit {
                    capacity: 1,
                    interval: Duration::from_millis(150),
                }),
            }),
        };

        let test_harness: PluginTestHarness<RouterLimits> =
            PluginTestHarness::builder().license(license).build().await;

        let service = test_harness.router_service(|_req| async {
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            Ok(router::Response::fake_builder()
                .data(serde_json::json!({"data": {"field": "value"}}))
                .header("x-custom-header", "test-value")
                .build()
                .unwrap())
        });

        // WHEN
        // * three reqs happen concurrently
        // * one delayed enough to be outside of rate limiting interval
        let f1 = service.call_default();
        let f2 = service.call_default();
        #[allow(clippy::async_yields_async)]
        let f3 = async {
            tokio::time::sleep(Duration::from_millis(500)).await;
            service.call_default()
        };

        let (r1, r2, r3) = tokio::join!(f1, f2, f3);

        // THEN
        // * the first succeeds
        // * the second gets rate limited
        // * the third, delayed req succeeds

        assert!(r1.is_ok_and(|resp| resp.response.status().is_success()));
        assert!(r2.is_ok_and(|resp| resp.response.status() == StatusCode::TOO_MANY_REQUESTS));
        assert!(r3
            .await
            .is_ok_and(|resp| resp.response.status().is_success()));
    }

    #[tokio::test]
    async fn it_emits_metrics_when_tps_enforced() {
        async {
            // GIVEN
            // * a license with tps limits set to 1 req per 200ms
            // * the router limits plugin
            let license = LicenseState::Licensed {
                limits: Some(LicenseLimits {
                    tps: Some(TpsLimit {
                        capacity: 1,
                        interval: Duration::from_millis(150),
                    }),
                }),
            };

            let test_harness: PluginTestHarness<RouterLimits> =
                PluginTestHarness::builder().license(license).build().await;

            let service = test_harness.router_service(|_req| async {
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                Ok(router::Response::fake_builder()
                    .data(serde_json::json!({"data": {"field": "value"}}))
                    .header("x-custom-header", "test-value")
                    .build()
                    .unwrap())
            });

            // WHEN
            // * two reqs happen
            service.call_default().await;
            service.call_default().await;

            // THEN
            // * we get a metric saying the tps limit was enforced
            assert_counter!("apollo.router_limits.tps.limit_enforced", 1);
        }
        .with_metrics()
        .await;
    }
}
