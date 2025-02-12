//! A plugin for enforcing product limitations in the router based on License claims
//!
//! Currently includes:
//! * TPS Rate Limiting: a certain threshold, set via License claim, for how many operations over a certain interval can be serviced

use std::num::NonZeroU64;
use std::ops::ControlFlow;
use std::ops::Sub;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;
use std::time::Instant;

use http::StatusCode;
use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;
use tower::limit::RateLimitLayer;
use tower::load_shed::error::Overloaded;
use tower::BoxError;
use tower::ServiceBuilder;
use tower::ServiceExt;
use tracing::Span;

use crate::graphql;
use crate::layers::ServiceBuilderExt;
use crate::metrics::count_graphql_error;
use crate::plugin::PluginInit;
use crate::plugin::PluginPrivate;
use crate::plugins::telemetry::consts::OTEL_STATUS_CODE;
use crate::plugins::telemetry::consts::OTEL_STATUS_CODE_ERROR;
use crate::services::router;
use crate::services::RouterResponse;
use crate::uplink::license_enforcement::LicenseState;
use crate::uplink::license_enforcement::APOLLO_ROUTER_LICENSE_EXPIRED;
use crate::uplink::license_enforcement::LICENSE_EXPIRED_SHORT_MESSAGE;

#[derive(PartialEq, Debug, Clone)]
/// The limits placed on a router in virtue of what's in a user's license
pub(crate) struct LicenseEnforcement {
    /// Transactions per second allowed based on license tier
    tps: Option<TpsLimitConf>,

    license: Arc<LicenseState>,
}

#[derive(PartialEq, Debug, Clone)]
/// Configuration for transactions per second
pub(crate) struct TpsLimitConf {
    /// The number of operations allowed during a certain interval
    capacity: NonZeroU64,
    /// The interval as specied in the user's license; this is in milliseconds
    interval: Duration,
}

#[derive(Debug, Default, Deserialize, JsonSchema, Serialize)]
pub(crate) struct LicenseEnforcementConfig {}

#[async_trait::async_trait]
impl PluginPrivate for LicenseEnforcement {
    type Config = LicenseEnforcementConfig;

    async fn new(init: PluginInit<Self::Config>) -> Result<Self, BoxError> {
        let tps = init.license.get_limits().and_then(|limits| {
            limits.tps.and_then(|tps| {
                NonZeroU64::new(tps.capacity as u64).map(|capacity| TpsLimitConf {
                    capacity,
                    interval: tps.interval,
                })
            })
        });

        Ok(Self {
            tps,
            license: Arc::new(init.license),
        })
    }

    fn router_service(&self, service: router::BoxService) -> router::BoxService {
        let license = self.license.clone();
        // Start from 1 second ago so that we get the first log message
        let start_log_tracking = Instant::now().sub(Duration::from_secs(1));
        ServiceBuilder::new()
            .checkpoint(move |req: router::Request| {
                Self::set_span_state(&license);
                Self::maybe_log(&start_log_tracking, &license);

                if matches!(license.as_ref(), LicenseState::LicensedHalt { limits: _ }) {
                    Ok(ControlFlow::Break(router::Response::builder()
                        .context(req.context)
                        .status_code(StatusCode::INTERNAL_SERVER_ERROR)
                        .build()
                        .expect("response must be valid")
                    ))
                } else {
                    Ok(ControlFlow::Continue(req))
                }
            })
            .map_future_with_request_data(
                |req: &router::Request| req.context.clone(),
                move |ctx, future| async {
                    let response: Result<RouterResponse, BoxError> = future.await;
                    match response {
                        Ok(ok) => Ok(ok),
                        Err(err) if err.is::<Overloaded>() => {
                            let extension_code = "ROUTER_FREE_PLAN_RATE_LIMIT_REACHED";
                            count_graphql_error(1u64, Some(extension_code));

                            let error = graphql::Error::builder()
                                .message("Your request has been rate limited. You've reached the limits for the Free plan. Consider upgrading to a higher plan for increased limits.")
                                .extension_code(extension_code)
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

impl LicenseEnforcement {
    fn set_span_state(license: &LicenseState) {
        if matches!(
            license,
            LicenseState::LicensedWarn { limits: _ } | LicenseState::LicensedHalt { limits: _ }
        ) {
            let span = Span::current();
            span.record(OTEL_STATUS_CODE, OTEL_STATUS_CODE_ERROR);
            span.record("apollo_router.license", LICENSE_EXPIRED_SHORT_MESSAGE);
        }
    }

    fn maybe_log(start: &Instant, license: &LicenseState) {
        static DELTA: AtomicU64 = AtomicU64::new(0);
        if matches!(
            license,
            LicenseState::LicensedHalt { limits: _ } | LicenseState::LicensedWarn { limits: _ }
        ) {
            // This will rate limit logs about license to 1 a second.
            // The way it works is storing the delta in seconds from a starting instant.
            // If the delta is over one second from the last time we logged then try and do a compare_exchange and if successfull log.
            // If not successful some other thread will have logged.
            let last_elapsed_seconds = DELTA.load(Ordering::SeqCst);
            let elapsed_seconds = start.elapsed().as_secs();
            if elapsed_seconds > last_elapsed_seconds
                && DELTA
                    .compare_exchange(
                        last_elapsed_seconds,
                        elapsed_seconds,
                        Ordering::SeqCst,
                        Ordering::SeqCst,
                    )
                    .is_ok()
            {
                ::tracing::error!(
                    code = APOLLO_ROUTER_LICENSE_EXPIRED,
                    LICENSE_EXPIRED_SHORT_MESSAGE
                );
            }
        }
    }
}

register_private_plugin!("apollo", "license_enforcement", LicenseEnforcement);

#[cfg(test)]
mod test {
    use http_body_util::BodyExt;
    use tracing::instrument::WithSubscriber;

    use super::*;
    use crate::assert_snapshot_subscriber;
    use crate::metrics::FutureMetricsExt;
    use crate::plugins::test::PluginTestHarness;
    use crate::services::router::Response;
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

        let test_harness: PluginTestHarness<LicenseEnforcement> =
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
        assert!(r2.is_ok_and(|resp| resp.response.status() == StatusCode::SERVICE_UNAVAILABLE));
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

            let test_harness: PluginTestHarness<LicenseEnforcement> =
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
            let _ = service.call_default().await;
            let _ = service.call_default().await;

            // THEN
            // * we get a metric saying the tps limit was enforced
            assert_counter!(
                "apollo.router.graphql_error",
                1,
                code = "ROUTER_FREE_PLAN_RATE_LIMIT_REACHED"
            );
        }
        .with_metrics()
        .await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_license_warn() {
        async {
            let license = LicenseState::LicensedWarn {
                limits: Some(LicenseLimits { tps: None }),
            };

            let test_harness: PluginTestHarness<LicenseEnforcement> =
                PluginTestHarness::builder().license(license).build().await;

            let service = test_harness.router_service(|_req| async {
                Ok(router::Response::fake_builder()
                    .data(serde_json::json!({"data": {"field": "value"}}))
                    .header("x-custom-header", "test-value")
                    .build()
                    .unwrap())
            });
            // There should be one log message only
            let resp = service.call_default().await;
            assert_eq!(
                get_body(resp).await,
                r#"{"data":{"data":{"field":"value"}}}"#
            );
            let resp = service.call_default().await;
            assert_eq!(
                get_body(resp).await,
                r#"{"data":{"data":{"field":"value"}}}"#
            );
        }
        .with_subscriber(assert_snapshot_subscriber!())
        .await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_license_halt() {
        async {
            let license = LicenseState::LicensedHalt {
                limits: Some(LicenseLimits { tps: None }),
            };

            let test_harness: PluginTestHarness<LicenseEnforcement> =
                PluginTestHarness::builder().license(license).build().await;

            let service = test_harness.router_service(|_req| async {
                Ok(router::Response::fake_builder()
                    .data(serde_json::json!({"data": {"field": "value"}}))
                    .header("x-custom-header", "test-value")
                    .build()
                    .unwrap())
            });
            // There should be one log message only
            let resp = service.call_default().await;
            assert_eq!(get_body(resp).await, r#"{}"#);
            let resp = service.call_default().await;
            assert_eq!(get_body(resp).await, r#"{}"#);
        }
        .with_subscriber(assert_snapshot_subscriber!())
        .await;
    }

    async fn get_body(resp: Result<Response, BoxError>) -> String {
        String::from_utf8_lossy(
            resp.expect("ok response")
                .response
                .into_body()
                .collect()
                .await
                .expect("body")
                .to_bytes()
                .as_ref(),
        )
        .to_string()
    }
}
