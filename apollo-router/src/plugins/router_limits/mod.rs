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
    pub(crate) tps: TpsLimitConf,
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

    // This will return an error only in cases where the router_limits plugin has been registered
    // but there are no claims in the license for TPS. We _must_ check that there are claims in the
    // router factory when regsitering this plugin
    async fn new(init: PluginInit<Self::Config>) -> Result<Self, BoxError> {
        let limits = init
            .license
            .get_limits()
            .ok_or("License limits found during plugin initialization but failed to get limits in constructor phase of router_limits")
            ?;

        let tps = limits
            .tps
            .ok_or("License limits defined but no TPS claim defined")?;

        let capacity = NonZeroU64::new(tps.capacity as u64)
            .ok_or("Failed to convert TPS capacity into a usable value")?;

        Ok(Self {
            tps: TpsLimitConf {
                capacity,
                interval: tps.interval,
            },
        })
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
            .layer(RateLimitLayer::new(
                self.tps.capacity.into(),
                self.tps.interval,
            ))
            .service(service)
            .boxed()
    }
}

register_private_plugin!("apollo", "router_limits", RouterLimits);

#[cfg(test)]
mod test {
    use serde_json::json;
    use tower::Service;

    use super::*;
    use crate::plugin::test::MockRouterService;
    use crate::plugin::DynPlugin;
    use crate::plugins::test::PluginTestHarness;
    use crate::services::RouterRequest;
    use crate::uplink::license_enforcement::LicenseLimits;
    use crate::uplink::license_enforcement::LicenseState;
    use crate::uplink::license_enforcement::TpsLimit;

    const APOLLO_ROUTER_LIMITS: &str = "apollo.router_limits";

    async fn get_router_limits_plugin(
        license: LicenseState,
    ) -> Result<Box<dyn DynPlugin>, BoxError> {
        let empty_config = serde_json::to_value(RouterLimitsConfig {}).unwrap();
        let plugin_init = PluginInit::fake_builder()
            .license(license)
            .config(empty_config)
            .build();

        crate::plugin::plugins()
            .find(|factory| factory.name == APOLLO_ROUTER_LIMITS)
            .expect("Plugin not found")
            .create_instance(plugin_init)
            .await
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn it_enforces_tps_limit_when_license() {
        // GIVEN
        // * router limits plugin
        // * license with limits

        let license = LicenseState::Licensed {
            limits: Some(LicenseLimits {
                tps: Some(TpsLimit {
                    capacity: 1,
                    interval: Duration::from_millis(500),
                }),
            }),
        };
        let router_limits_plugin = get_router_limits_plugin(license).await.unwrap();

        let mut mock_service = MockRouterService::new();
        mock_service.expect_call().times(0..3).returning(|_| {
            Ok(RouterResponse::fake_builder()
                .data(json!({ "test": 5678_u32 }))
                .build()
                .unwrap())
        });
        mock_service
            .expect_clone()
            .returning(MockRouterService::new);

        // WHEN
        // * the router is called three times with a capacity of 1 and a interval of 500ms with a
        // delay between the second and third calls

        // THEN
        // * the first call succeeds
        // * the second call violates the tps limit
        // * the third call, being out of the rate limiting interval, succeeds
        let mut svc = router_limits_plugin.router_service(mock_service.boxed());
        let response: RouterResponse = svc
            .ready()
            .await
            .expect("it is ready")
            .call(RouterRequest::fake_builder().build().unwrap())
            .await
            .unwrap();
        assert_eq!(StatusCode::OK, response.response.status());

        let response: RouterResponse = svc
            .ready()
            .await
            .expect("it is ready")
            .call(RouterRequest::fake_builder().build().unwrap())
            .await
            .unwrap();
        assert_eq!(StatusCode::TOO_MANY_REQUESTS, response.response.status());

        let j: serde_json::Value = serde_json::from_slice(
            &crate::services::router::body::into_bytes(response.response)
                .await
                .expect("we have a body"),
        )
        .expect("our body is valid json");

        // THEN
        // * there's an appropriate rate limiting message
        assert_eq!(
            "Your request has been rate limited",
            j["errors"][0]["message"]
        );

        // THEN
        // * there's an appropriate graphql extension code (it's important this stay stable for
        // collecting metrics)
        assert_eq!(
            "ROUTER_TPS_LIMIT_REACHED",
            j["errors"][0]["extensions"]["code"]
        );

        tokio::time::sleep(Duration::from_millis(500)).await;

        let response: RouterResponse = svc
            .ready()
            .await
            .expect("it is ready")
            .call(RouterRequest::fake_builder().build().unwrap())
            .await
            .unwrap();

        assert_eq!(StatusCode::OK, response.response.status());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_tps_limits_multi_threaded() {
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
}
