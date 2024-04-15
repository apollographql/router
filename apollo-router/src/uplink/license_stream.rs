// tonic does not derive `Eq` for the gRPC message types, which causes a warning from Clippy. The
// current suggestion is to explicitly allow the lint in the module that imports the protos.
// Read more: https://github.com/hyperium/tonic/issues/1056
#![allow(clippy::derive_partial_eq_without_eq)]

use std::collections::HashSet;
use std::pin::Pin;
use std::str::FromStr;
use std::sync::Arc;
use std::task::Context;
use std::task::Poll;
use std::time::Instant;
use std::time::SystemTime;

use futures::future::Ready;
use futures::stream::FilterMap;
use futures::stream::Fuse;
use futures::stream::Repeat;
use futures::stream::Zip;
use futures::Stream;
use futures::StreamExt;
use graphql_client::GraphQLQuery;
use pin_project_lite::pin_project;
use tokio_util::time::DelayQueue;

use crate::router::Event;
use crate::uplink::license_enforcement::Audience;
use crate::uplink::license_enforcement::Claims;
use crate::uplink::license_enforcement::License;
use crate::uplink::license_enforcement::LicenseState;
use crate::uplink::license_enforcement::OneOrMany;
use crate::uplink::license_stream::license_query::FetchErrorCode;
use crate::uplink::license_stream::license_query::LicenseQueryRouterEntitlements;
use crate::uplink::UplinkRequest;
use crate::uplink::UplinkResponse;

const APOLLO_ROUTER_LICENSE_OFFLINE_UNSUPPORTED: &str = "APOLLO_ROUTER_LICENSE_OFFLINE_UNSUPPORTED";

#[derive(GraphQLQuery)]
#[graphql(
    query_path = "src/uplink/license_query.graphql",
    schema_path = "src/uplink/uplink.graphql",
    request_derives = "Debug",
    response_derives = "PartialEq, Debug, Deserialize",
    deprecated = "warn"
)]
pub(crate) struct LicenseQuery {}

impl From<UplinkRequest> for license_query::Variables {
    fn from(req: UplinkRequest) -> Self {
        license_query::Variables {
            api_key: req.api_key,
            graph_ref: req.graph_ref,
            if_after_id: req.id,
        }
    }
}

impl From<license_query::ResponseData> for UplinkResponse<License> {
    fn from(response: license_query::ResponseData) -> Self {
        match response.router_entitlements {
            LicenseQueryRouterEntitlements::RouterEntitlementsResult(result) => {
                if let Some(license) = result.entitlement {
                    match License::from_str(&license.jwt) {
                        Ok(jwt) => UplinkResponse::New {
                            response: jwt,
                            id: result.id,
                            // this will truncate the number of seconds to under u64::MAX, which should be
                            // a large enough delay anyway
                            delay: result.min_delay_seconds as u64,
                        },
                        Err(error) => UplinkResponse::Error {
                            retry_later: true,
                            code: "INVALID_LICENSE".to_string(),
                            message: error.to_string(),
                        },
                    }
                } else {
                    UplinkResponse::New {
                        response: License::default(),
                        id: result.id,
                        // this will truncate the number of seconds to under u64::MAX, which should be
                        // a large enough delay anyway
                        delay: result.min_delay_seconds as u64,
                    }
                }
            }
            LicenseQueryRouterEntitlements::Unchanged(response) => UplinkResponse::Unchanged {
                id: Some(response.id),
                delay: Some(response.min_delay_seconds as u64),
            },
            LicenseQueryRouterEntitlements::FetchError(error) => UplinkResponse::Error {
                retry_later: error.code == FetchErrorCode::RETRY_LATER,
                code: match error.code {
                    FetchErrorCode::AUTHENTICATION_FAILED => "AUTHENTICATION_FAILED".to_string(),
                    FetchErrorCode::ACCESS_DENIED => "ACCESS_DENIED".to_string(),
                    FetchErrorCode::UNKNOWN_REF => "UNKNOWN_REF".to_string(),
                    FetchErrorCode::RETRY_LATER => "RETRY_LATER".to_string(),
                    FetchErrorCode::NOT_IMPLEMENTED_ON_THIS_INSTANCE => {
                        "NOT_IMPLEMENTED_ON_THIS_INSTANCE".to_string()
                    }
                    FetchErrorCode::Other(other) => other,
                },
                message: error.message,
            },
        }
    }
}

pin_project! {
    /// This stream wrapper will cause check the current license at the point of warn_at or halt_at.
    /// This means that the state machine can be kept clean, and not have to deal with setting it's own timers and also avoids lots of racy scenarios as license checks are guaranteed to happen after a license update even if they were in the past.
    #[must_use = "streams do nothing unless polled"]
    #[project = LicenseExpanderProj]
    pub(crate) struct LicenseExpander<Upstream>
    where
        Upstream: Stream<Item = License>,
    {
        #[pin]
        checks: DelayQueue<Event>,
        #[pin]
        upstream: Fuse<Upstream>,
    }
}

impl<Upstream> Stream for LicenseExpander<Upstream>
where
    Upstream: Stream<Item = License>,
{
    type Item = Event;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let mut this = self.project();
        let checks = this.checks.poll_expired(cx);
        // Only check downstream if checks was not Some
        let next = if matches!(checks, Poll::Ready(Some(_))) {
            None
        } else {
            // Poll upstream. Note that it is OK for this to be called again after it has finished as the stream is fused and if it is exhausted it will return Poll::Ready(None).
            Some(this.upstream.poll_next(cx))
        };

        match (checks, next) {
            // Checks has an expired claim that needs checking.
            // This is the ONLY arm where upstream.poll_next has not been called, and this is OK because we are not returning pending.
            (Poll::Ready(Some(item)), _) => Poll::Ready(Some(item.into_inner())),
            // Upstream has a new license with a claim
            (_, Some(Poll::Ready(Some(license)))) if license.claims.is_some() => {
                // If we got a new license then we need to reset the stream of events and return the new license event.
                reset_checks_for_licenses(&mut this.checks, license)
            }
            // Upstream has a new license with no claim.
            (_, Some(Poll::Ready(Some(_)))) => {
                // We don't clear the checks if there is a license with no claim.
                Poll::Ready(Some(Event::UpdateLicense(LicenseState::Unlicensed)))
            }
            // If either checks or upstream returned pending then we need to return pending.
            // It is the responsibility of upstream and checks to schedule wakeup.
            // If we have got to this line then checks.poll_expired and upstream.poll_next *will* have been called.
            (Poll::Pending, _) | (_, Some(Poll::Pending)) => Poll::Pending,
            // If both stream are exhausted then return none.
            (Poll::Ready(None), Some(Poll::Ready(None))) => Poll::Ready(None),
            (Poll::Ready(None), None) => {
                unreachable!("upstream will have been called as checks did not have a value")
            }
        }
    }
}

/// This function takes a license and returns the appropriate event for that license.
/// If warn at or halt at are in the future it will register appropriate checks to trigger at such times.
fn reset_checks_for_licenses(
    checks: &mut DelayQueue<Event>,
    license: License,
) -> Poll<Option<Event>> {
    // We got a new claim, so clear the previous checks.
    checks.clear();
    let claims = license.claims.as_ref().expect("claims is gated, qed");
    let halt_at = to_positive_instant(claims.halt_at);
    let warn_at = to_positive_instant(claims.warn_at);
    let now = Instant::now();
    // Insert the new checks. If any of the boundaries are in the past then just return the immediate result
    if halt_at > now {
        // Only add halt if it isn't immediately going to be triggered.
        checks.insert_at(
            Event::UpdateLicense(LicenseState::LicensedHalt),
            (halt_at).into(),
        );
    } else {
        return Poll::Ready(Some(Event::UpdateLicense(LicenseState::LicensedHalt)));
    }
    if warn_at > now {
        // Only add warn if it isn't immediately going to be triggered and halt is not already set.
        // Something that is halted is by definition also warn.
        checks.insert_at(
            Event::UpdateLicense(LicenseState::LicensedWarn),
            (warn_at).into(),
        );
    } else {
        return Poll::Ready(Some(Event::UpdateLicense(LicenseState::LicensedWarn)));
    }

    Poll::Ready(Some(Event::UpdateLicense(LicenseState::Licensed)))
}

/// This function exists to generate an approximate Instant from a `SystemTime`. We have externally generated unix timestamps that need to be scheduled, but anything time related to scheduling must be an `Instant`.
/// The generated instant is only approximate.
/// Subtracting from instants is not supported on all platforms, so if the calculated instant was in the past we just return now as we don't care about how long ago the instant was, just that it happened already.
fn to_positive_instant(system_time: SystemTime) -> Instant {
    // This is approximate as there is no real conversion between SystemTime and Instant
    let now_instant = Instant::now();
    let now_system_time = SystemTime::now();
    // system_time is likely to be a time in the future, but may be in the past.
    match system_time.duration_since(now_system_time) {
        // system_time was in the future.
        Ok(duration) => now_instant + duration,

        // system_time was in the past.
        Err(_) => now_instant,
    }
}

type ValidateAudience<T> = FilterMap<
    Zip<T, Repeat<Arc<HashSet<Audience>>>>,
    Ready<Option<License>>,
    fn((License, Arc<HashSet<Audience>>)) -> Ready<Option<License>>,
>;

pub(crate) trait LicenseStreamExt: Stream<Item = License> {
    fn expand_licenses(self) -> LicenseExpander<Self>
    where
        Self: Sized,
    {
        LicenseExpander {
            checks: Default::default(),
            upstream: self.fuse(),
        }
    }

    fn validate_audience(self, audiences: impl Into<HashSet<Audience>>) -> ValidateAudience<Self>
    where
        Self: Sized,
    {
        // Zip is used to inject the data into the stream, and then filter_map can be used to actually deal with the data.
        // There's no way to do this with a closure without hitting compiler issues.
        // In the past we have implemented our own steps where we have needed to inject state, but this is the recommended way to do it.
        let audiences: Arc<HashSet<Audience>> = Arc::new(audiences.into());
        self.zip(futures::stream::repeat(audiences))
            .filter_map(|(license, audiences)| {
                let matches = match &license {
                    License {
                        claims:
                            Some(Claims {
                                aud: OneOrMany::Many(aud),
                                ..
                            }),
                    } => aud.iter().any(|aud| audiences.contains(aud)),
                    License {
                        claims:
                            Some(Claims {
                                aud: OneOrMany::One(aud),
                                ..
                            }),
                    } => audiences.contains(aud),
                    // A license with no claims is always valid. We will check later if any commercial features are in use.
                    License { claims: None } => true,
                };

                if !matches {
                    tracing::error!(
                        code = APOLLO_ROUTER_LICENSE_OFFLINE_UNSUPPORTED,
                        "the license file was valid, but was not enabled offline use",
                    );
                }
                futures::future::ready(if matches { Some(license) } else { None })
            })
    }
}

impl<T: Stream<Item = License>> LicenseStreamExt for T {}

#[cfg(test)]
mod test {
    use std::future::ready;
    use std::time::Duration;
    use std::time::Instant;
    use std::time::SystemTime;

    use futures::StreamExt;
    use futures_test::stream::StreamTestExt;
    use tracing::instrument::WithSubscriber;

    use crate::assert_snapshot_subscriber;
    use crate::router::Event;
    use crate::uplink::license_enforcement::Audience;
    use crate::uplink::license_enforcement::Claims;
    use crate::uplink::license_enforcement::License;
    use crate::uplink::license_enforcement::LicenseState;
    use crate::uplink::license_enforcement::OneOrMany;
    use crate::uplink::license_stream::to_positive_instant;
    use crate::uplink::license_stream::LicenseQuery;
    use crate::uplink::license_stream::LicenseStreamExt;
    use crate::uplink::stream_from_uplink;
    use crate::uplink::UplinkConfig;

    #[tokio::test]
    async fn integration_test() {
        if let (Ok(apollo_key), Ok(apollo_graph_ref)) = (
            std::env::var("TEST_APOLLO_KEY"),
            std::env::var("TEST_APOLLO_GRAPH_REF"),
        ) {
            let results = stream_from_uplink::<LicenseQuery, License>(UplinkConfig {
                apollo_key,
                apollo_graph_ref,
                endpoints: None,
                poll_interval: Duration::from_secs(1),
                timeout: Duration::from_secs(5),
            })
            .take(1)
            .collect::<Vec<_>>()
            .await;

            assert!(results
                .first()
                .expect("expected one result")
                .as_ref()
                .expect("license should be OK")
                .claims
                .is_some())
        }
    }

    #[test]
    fn test_to_instant() {
        let now_system_time = SystemTime::now();
        let now_instant = Instant::now();
        let future_system_time = now_system_time + Duration::from_secs(1024);
        let future_instant = to_positive_instant(future_system_time);
        assert!(future_instant < now_instant + Duration::from_secs(1025));
        assert!(future_instant > now_instant + Duration::from_secs(1023));

        // An instant in the past will return something greater than the original now_instant, but less than a new instant.
        let past_system_time = now_system_time - Duration::from_secs(1024);
        let past_instant = to_positive_instant(past_system_time);
        assert!(past_instant > now_instant);
        assert!(past_instant < Instant::now());
    }

    #[tokio::test]
    async fn license_expander() {
        let events_stream = futures::stream::iter(vec![license_with_claim(15, 30)])
            .expand_licenses()
            .map(SimpleEvent::from);

        let events = events_stream.collect::<Vec<_>>().await;
        assert_eq!(
            events,
            &[
                SimpleEvent::UpdateLicense,
                SimpleEvent::WarnLicense,
                SimpleEvent::HaltLicense
            ]
        );
    }

    #[tokio::test]
    async fn license_expander_warn_now() {
        let events_stream = futures::stream::iter(vec![license_with_claim(0, 15)])
            .interleave_pending()
            .expand_licenses()
            .map(SimpleEvent::from);

        let events = events_stream.collect::<Vec<_>>().await;
        assert_eq!(
            events,
            &[SimpleEvent::WarnLicense, SimpleEvent::HaltLicense]
        );
    }

    #[tokio::test]
    async fn license_expander_halt_now() {
        let events_stream = futures::stream::iter(vec![license_with_claim(0, 0)])
            .interleave_pending()
            .expand_licenses()
            .map(SimpleEvent::from);

        let events = events_stream.collect::<Vec<_>>().await;
        assert_eq!(events, &[SimpleEvent::HaltLicense]);
    }

    #[tokio::test]
    async fn license_expander_no_claim() {
        let events_stream = futures::stream::iter(vec![license_with_no_claim()])
            .interleave_pending()
            .expand_licenses()
            .map(SimpleEvent::from);

        let events = events_stream.collect::<Vec<_>>().await;
        assert_eq!(events, &[SimpleEvent::UpdateLicense]);
    }

    #[tokio::test]
    async fn license_expander_claim_no_claim() {
        // Licenses with no claim do not clear checks as they are ignored if we move from entitled to unentitled, this is handled at the state machine level.
        let events_stream =
            futures::stream::iter(vec![license_with_claim(10, 10), license_with_no_claim()])
                .interleave_pending()
                .expand_licenses()
                .map(SimpleEvent::from);

        let events = events_stream.collect::<Vec<_>>().await;
        assert_eq!(
            events,
            &[
                SimpleEvent::UpdateLicense,
                SimpleEvent::UpdateLicense,
                SimpleEvent::WarnLicense,
                SimpleEvent::HaltLicense
            ]
        );
    }

    #[tokio::test]
    async fn license_expander_no_claim_claim() {
        let events_stream =
            futures::stream::iter(vec![license_with_no_claim(), license_with_claim(15, 30)])
                .interleave_pending()
                .expand_licenses()
                .map(SimpleEvent::from);

        let events = events_stream.collect::<Vec<_>>().await;
        assert_eq!(
            events,
            &[
                SimpleEvent::UpdateLicense,
                SimpleEvent::UpdateLicense,
                SimpleEvent::WarnLicense,
                SimpleEvent::HaltLicense
            ]
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn license_expander_claim_pause_claim() {
        let (tx, rx) = tokio::sync::mpsc::channel(10);
        let rx_stream = tokio_stream::wrappers::ReceiverStream::new(rx);
        let events_stream = rx_stream.expand_licenses().map(SimpleEvent::from);

        tokio::task::spawn(async move {
            // This simulates a new claim coming in before in between the warning and halt
            let _ = tx.send(license_with_claim(15, 45)).await;
            tokio::time::sleep(Duration::from_millis(20)).await;
            let _ = tx.send(license_with_claim(15, 30)).await;
        });
        let events = events_stream.collect::<Vec<_>>().await;
        assert_eq!(
            events,
            &[
                SimpleEvent::UpdateLicense,
                SimpleEvent::WarnLicense,
                SimpleEvent::UpdateLicense,
                SimpleEvent::WarnLicense,
                SimpleEvent::HaltLicense
            ]
        );
    }

    fn license_with_claim(warn_delta: u64, halt_delta: u64) -> License {
        let now = SystemTime::now();
        License {
            claims: Some(Claims {
                iss: "".to_string(),
                sub: "".to_string(),
                aud: OneOrMany::One(Audience::SelfHosted),
                warn_at: now + Duration::from_millis(warn_delta),
                halt_at: now + Duration::from_millis(halt_delta),
            }),
        }
    }

    fn license_with_no_claim() -> License {
        License { claims: None }
    }

    #[derive(Eq, PartialEq, Debug)]
    enum SimpleEvent {
        UpdateConfiguration,
        NoMoreConfiguration,
        UpdateSchema,
        NoMoreSchema,
        UpdateLicense,
        HaltLicense,
        WarnLicense,
        NoMoreLicense,
        ForcedHotReload,
        Shutdown,
    }

    impl From<Event> for SimpleEvent {
        fn from(value: Event) -> Self {
            match value {
                Event::UpdateConfiguration(_) => SimpleEvent::UpdateConfiguration,
                Event::NoMoreConfiguration => SimpleEvent::NoMoreConfiguration,
                Event::UpdateSchema(_) => SimpleEvent::UpdateSchema,
                Event::NoMoreSchema => SimpleEvent::NoMoreSchema,
                Event::UpdateLicense(LicenseState::LicensedHalt) => SimpleEvent::HaltLicense,
                Event::UpdateLicense(LicenseState::LicensedWarn) => SimpleEvent::WarnLicense,
                Event::UpdateLicense(_) => SimpleEvent::UpdateLicense,
                Event::NoMoreLicense => SimpleEvent::NoMoreLicense,
                Event::Reload => SimpleEvent::ForcedHotReload,
                Event::Shutdown => SimpleEvent::Shutdown,
            }
        }
    }

    #[tokio::test]
    async fn test_validate_audience_single() {
        assert_eq!(
            futures::stream::once(ready(License {
                claims: Some(Claims {
                    iss: "".to_string(),
                    sub: "".to_string(),
                    aud: OneOrMany::One(Audience::Offline),
                    warn_at: SystemTime::now(),
                    halt_at: SystemTime::now(),
                }),
            }))
            .validate_audience([Audience::Offline, Audience::Cloud])
            .count()
            .with_subscriber(assert_snapshot_subscriber!())
            .await,
            1
        );
    }

    #[tokio::test]
    async fn test_validate_audience_single_filtered() {
        assert_eq!(
            futures::stream::once(ready(License {
                claims: Some(Claims {
                    iss: "".to_string(),
                    sub: "".to_string(),
                    aud: OneOrMany::One(Audience::SelfHosted),
                    warn_at: SystemTime::now(),
                    halt_at: SystemTime::now(),
                }),
            }))
            .validate_audience([Audience::Offline, Audience::Cloud])
            .count()
            .with_subscriber(assert_snapshot_subscriber!())
            .await,
            0
        );
    }

    #[tokio::test]
    async fn test_validate_audience_multiple() {
        assert_eq!(
            futures::stream::once(ready(License {
                claims: Some(Claims {
                    iss: "".to_string(),
                    sub: "".to_string(),
                    aud: OneOrMany::Many(vec![Audience::SelfHosted, Audience::Offline]),
                    warn_at: SystemTime::now(),
                    halt_at: SystemTime::now(),
                }),
            }))
            .validate_audience([Audience::Offline, Audience::Cloud])
            .count()
            .with_subscriber(assert_snapshot_subscriber!())
            .await,
            1
        );
    }

    #[tokio::test]
    async fn test_validate_audience_multiple_filtered() {
        assert_eq!(
            futures::stream::once(ready(License {
                claims: Some(Claims {
                    iss: "".to_string(),
                    sub: "".to_string(),
                    aud: OneOrMany::Many(vec![Audience::SelfHosted, Audience::SelfHosted]),
                    warn_at: SystemTime::now(),
                    halt_at: SystemTime::now(),
                }),
            }))
            .validate_audience([Audience::Offline, Audience::Cloud])
            .count()
            .with_subscriber(assert_snapshot_subscriber!())
            .await,
            0
        );
    }

    #[tokio::test]
    async fn test_validate_no_claim() {
        assert_eq!(
            futures::stream::once(ready(License::default()))
                .validate_audience([Audience::Offline, Audience::Cloud])
                .count()
                .with_subscriber(assert_snapshot_subscriber!())
                .await,
            1
        );
    }
}
