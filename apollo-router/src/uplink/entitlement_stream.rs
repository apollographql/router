// With regards to ELv2 licensing, this entire file is license key functionality

// tonic does not derive `Eq` for the gRPC message types, which causes a warning from Clippy. The
// current suggestion is to explicitly allow the lint in the module that imports the protos.
// Read more: https://github.com/hyperium/tonic/issues/1056
#![allow(clippy::derive_partial_eq_without_eq)]

use std::pin::Pin;
use std::str::FromStr;
use std::task::Context;
use std::task::Poll;
use std::time::Instant;
use std::time::SystemTime;

use displaydoc::Display;
use futures::stream::Fuse;
use futures::Stream;
use futures::StreamExt;
use graphql_client::GraphQLQuery;
use pin_project_lite::pin_project;
use thiserror::Error;
use tokio_util::time::DelayQueue;

use crate::router::Event;
use crate::uplink::entitlement::Entitlement;
use crate::uplink::entitlement::EntitlementReport;
use crate::uplink::entitlement::EntitlementState;
use crate::uplink::entitlement_stream::entitlement_request::EntitlementRequestRouterEntitlements;
use crate::uplink::entitlement_stream::entitlement_request::FetchErrorCode;
use crate::uplink::UplinkRequest;
use crate::uplink::UplinkResponse;

#[derive(GraphQLQuery)]
#[graphql(
    query_path = "src/uplink/entitlement_query.graphql",
    schema_path = "src/uplink/uplink.graphql",
    request_derives = "Debug",
    response_derives = "PartialEq, Debug, Deserialize",
    deprecated = "warn"
)]
pub(crate) struct EntitlementRequest {}

impl From<UplinkRequest> for entitlement_request::Variables {
    fn from(req: UplinkRequest) -> Self {
        entitlement_request::Variables {
            api_key: req.api_key,
            graph_ref: req.graph_ref,
            unless_id: req.id,
        }
    }
}

impl From<entitlement_request::ResponseData> for UplinkResponse<Entitlement> {
    fn from(response: entitlement_request::ResponseData) -> Self {
        match response.router_entitlements {
            EntitlementRequestRouterEntitlements::RouterEntitlementsResult(result) => {
                if let Some(entitlement) = result.entitlement {
                    match Entitlement::from_str(&entitlement.jwt) {
                        Ok(entitlement) => UplinkResponse::Result {
                            response: entitlement,
                            id: result.id,
                            // this will truncate the number of seconds to under u64::MAX, which should be
                            // a large enough delay anyway
                            delay: result.min_delay_seconds as u64,
                        },
                        Err(error) => UplinkResponse::Error {
                            retry_later: true,
                            code: "INVALID_ENTITLEMENT".to_string(),
                            message: error.to_string(),
                        },
                    }
                } else {
                    UplinkResponse::Result {
                        response: Entitlement::default(),
                        id: result.id,
                        // this will truncate the number of seconds to under u64::MAX, which should be
                        // a large enough delay anyway
                        delay: result.min_delay_seconds as u64,
                    }
                }
            }
            EntitlementRequestRouterEntitlements::Unchanged(response) => {
                UplinkResponse::Unchanged {
                    id: Some(response.id),
                    delay: Some(response.min_delay_seconds as u64),
                }
            }
            EntitlementRequestRouterEntitlements::FetchError(error) => UplinkResponse::Error {
                retry_later: error.code == FetchErrorCode::RETRY_LATER,
                code: match error.code {
                    FetchErrorCode::AUTHENTICATION_FAILED => "AUTHENTICATION_FAILED".to_string(),
                    FetchErrorCode::ACCESS_DENIED => "ACCESS_DENIED".to_string(),
                    FetchErrorCode::UNKNOWN_REF => "UNKNOWN_REF".to_string(),
                    FetchErrorCode::RETRY_LATER => "RETRY_LATER".to_string(),
                    FetchErrorCode::Other(other) => other,
                },
                message: error.message,
            },
        }
    }
}

#[derive(Error, Display, Debug)]
pub(crate) enum Error {
    /// invalid entitlement: {0}
    InvalidEntitlement(jsonwebtoken::errors::Error),

    /// entitlement violations: {0}
    EntitlementViolations(EntitlementReport),
}

pin_project! {
    /// This stream wrapper will cause check the current entitlement at the point of warn_at or halt_at.
    /// This means that the state machine can be kept clean, and not have to deal with setting it's own timers and also avoids lots of racy scenarios as entitlement checks are guaranteed to happen after an entitlement update even if they were in the past.
    #[must_use = "streams do nothing unless polled"]
    #[project = EntitlementExpanderProj]
    pub(crate) struct EntitlementExpander<Upstream>
    where
        Upstream: Stream<Item = Entitlement>,
    {
        #[pin]
        checks: DelayQueue<Event>,
        #[pin]
        upstream: Fuse<Upstream>,
    }
}

impl<Upstream> Stream for EntitlementExpander<Upstream>
where
    Upstream: Stream<Item = Entitlement>,
{
    type Item = Event;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let mut this = self.project();
        let checks = this.checks.poll_expired(cx);
        // Only check downstream if checks was not Some
        let next = if matches!(checks, Poll::Ready(Some(_))) {
            // It doesn't matter what we set this to. The first match arm below will match.
            // Wrapping this in an Option makes readability worse.
            Poll::Ready(None)
        } else {
            // Poll upstream. Note that it is OK for this to be called again after it has finished as the stream is fused and if it is exhausted it will return Poll::Ready(None).
            this.upstream.poll_next(cx)
        };

        match (checks, next) {
            // Checks has an expired claim that needs checking.
            // This is the ONLY arm where upstream.poll_next has not been called, and this is OK because we are not returning pending.
            (Poll::Ready(Some(item)), _) => Poll::Ready(Some(item.into_inner())),
            // Upstream has a new entitlement with a claim
            (_, Poll::Ready(Some(entitlement))) if entitlement.claims.is_some() => {
                // If we got a new entitlement then we need to reset the stream of events and return the new entitlement event.
                reset_checks_for_entitlement(&mut this.checks, entitlement)
            }
            // Upstream has a new entitlement with no claim
            (_, Poll::Ready(Some(_))) => {
                // As we have no claim clear the checks
                this.checks.clear();
                Poll::Ready(Some(Event::UpdateEntitlement(EntitlementState::Unentitled)))
            }
            // If either checks or upstream returned pending then we need to return pending.
            // It is the responsibility of upstream and checks to schedule wakup.
            // If we have got to this line then checks.poll_expired and upstream.poll_next *will* have been called.
            (Poll::Pending, _) | (_, Poll::Pending) => Poll::Pending,
            // If both stream are exhausted then return none.
            (Poll::Ready(None), Poll::Ready(None)) => Poll::Ready(None),
        }
    }
}

/// This function takes an entitlement and returns the appropriate event for that entitlement.
/// If warn at or halt at are in the future it will register appropriate checks to trigger at such times.
fn reset_checks_for_entitlement(
    checks: &mut DelayQueue<Event>,
    entitlement: Entitlement,
) -> Poll<Option<Event>> {
    // We got a new claim, so clear the previous checks.
    checks.clear();
    let claims = entitlement.claims.as_ref().expect("claims is gated, qed");
    let halt_at = to_positive_instant(claims.halt_at);
    let warn_at = to_positive_instant(claims.warn_at);
    let now = Instant::now();
    // Insert the new checks. If any of the boundaries are in the past then just return the immediate result
    if halt_at > now {
        // Only add halt if it isn't immediately going to be triggered.
        checks.insert_at(
            Event::UpdateEntitlement(EntitlementState::EntitledHalt),
            (halt_at).into(),
        );
    } else {
        return Poll::Ready(Some(Event::UpdateEntitlement(
            EntitlementState::EntitledHalt,
        )));
    }
    if warn_at > now {
        // Only add warn if it isn't immediately going to be triggered and halt is not already set.
        // Something that is halted is by definition also warn.
        checks.insert_at(
            Event::UpdateEntitlement(EntitlementState::EntitledWarn),
            (warn_at).into(),
        );
    } else {
        return Poll::Ready(Some(Event::UpdateEntitlement(
            EntitlementState::EntitledWarn,
        )));
    }

    Poll::Ready(Some(Event::UpdateEntitlement(EntitlementState::Entitled)))
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

pub(crate) trait EntitlementStreamExt: Stream<Item = Entitlement> {
    fn expand_entitlements(self) -> EntitlementExpander<Self>
    where
        Self: Sized,
    {
        EntitlementExpander {
            checks: Default::default(),
            upstream: self.fuse(),
        }
    }
}

impl<T: Stream<Item = Entitlement>> EntitlementStreamExt for T {}

#[cfg(test)]
mod test {
    use std::time::Duration;
    use std::time::Instant;
    use std::time::SystemTime;

    use futures::SinkExt;
    use futures::StreamExt;
    use futures_test::stream::StreamTestExt;

    use crate::router::Event;
    use crate::uplink::entitlement::Audience;
    use crate::uplink::entitlement::Claims;
    use crate::uplink::entitlement::Entitlement;
    use crate::uplink::entitlement::EntitlementState;
    use crate::uplink::entitlement::OneOrMany;
    use crate::uplink::entitlement_stream::to_positive_instant;
    use crate::uplink::entitlement_stream::EntitlementRequest;
    use crate::uplink::entitlement_stream::EntitlementStreamExt;
    use crate::uplink::stream_from_uplink;

    #[tokio::test]
    async fn integration_test() {
        if let (Ok(apollo_key), Ok(apollo_graph_ref)) = (
            std::env::var("TEST_APOLLO_KEY"),
            std::env::var("TEST_APOLLO_GRAPH_REF"),
        ) {
            let results = stream_from_uplink::<EntitlementRequest, Entitlement>(
                apollo_key,
                apollo_graph_ref,
                None,
                Duration::from_secs(1),
                Duration::from_secs(5),
            )
            .take(1)
            .collect::<Vec<_>>()
            .await;

            assert!(results
                .get(0)
                .expect("expected one result")
                .as_ref()
                .expect("entitlement should be OK")
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
    async fn entitlement_expander() {
        let events_stream = futures::stream::iter(vec![entitlement_with_claim(15, 30)])
            .expand_entitlements()
            .map(SimpleEvent::from);

        let events = events_stream.collect::<Vec<_>>().await;
        assert_eq!(
            events,
            &[
                SimpleEvent::UpdateEntitlement,
                SimpleEvent::WarnEntitlement,
                SimpleEvent::HaltEntitlement
            ]
        );
    }

    #[tokio::test]
    async fn entitlement_expander_warn_now() {
        let events_stream = futures::stream::iter(vec![entitlement_with_claim(0, 15)])
            .interleave_pending()
            .expand_entitlements()
            .map(SimpleEvent::from);

        let events = events_stream.collect::<Vec<_>>().await;
        assert_eq!(
            events,
            &[SimpleEvent::WarnEntitlement, SimpleEvent::HaltEntitlement]
        );
    }

    #[tokio::test]
    async fn entitlement_expander_halt_now() {
        let events_stream = futures::stream::iter(vec![entitlement_with_claim(0, 0)])
            .interleave_pending()
            .expand_entitlements()
            .map(SimpleEvent::from);

        let events = events_stream.collect::<Vec<_>>().await;
        assert_eq!(events, &[SimpleEvent::HaltEntitlement]);
    }

    #[tokio::test]
    async fn entitlement_expander_no_claim() {
        let events_stream = futures::stream::iter(vec![entitlement_with_no_claim()])
            .interleave_pending()
            .expand_entitlements()
            .map(SimpleEvent::from);

        let events = events_stream.collect::<Vec<_>>().await;
        assert_eq!(events, &[SimpleEvent::UpdateEntitlement]);
    }

    #[tokio::test]
    async fn entitlement_expander_claim_no_claim() {
        let events_stream = futures::stream::iter(vec![
            entitlement_with_claim(10, 10),
            entitlement_with_no_claim(),
        ])
        .interleave_pending()
        .expand_entitlements()
        .map(SimpleEvent::from);

        let events = events_stream.collect::<Vec<_>>().await;
        assert_eq!(
            events,
            &[
                SimpleEvent::UpdateEntitlement,
                SimpleEvent::UpdateEntitlement
            ]
        );
    }

    #[tokio::test]
    async fn entitlement_expander_no_claim_claim() {
        let events_stream = futures::stream::iter(vec![
            entitlement_with_no_claim(),
            entitlement_with_claim(15, 30),
        ])
        .interleave_pending()
        .expand_entitlements()
        .map(SimpleEvent::from);

        let events = events_stream.collect::<Vec<_>>().await;
        assert_eq!(
            events,
            &[
                SimpleEvent::UpdateEntitlement,
                SimpleEvent::UpdateEntitlement,
                SimpleEvent::WarnEntitlement,
                SimpleEvent::HaltEntitlement
            ]
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn entitlement_expander_claim_pause_claim() {
        let (mut tx, rx) = futures::channel::mpsc::channel(10);
        let events_stream = rx.expand_entitlements().map(SimpleEvent::from);

        tokio::task::spawn(async move {
            // This simulates a new claim coming in before in between the warning and halt
            let _ = tx.send(entitlement_with_claim(15, 45)).await;
            tokio::time::sleep(Duration::from_millis(20)).await;
            let _ = tx.send(entitlement_with_claim(15, 30)).await;
        });
        let events = events_stream.collect::<Vec<_>>().await;
        assert_eq!(
            events,
            &[
                SimpleEvent::UpdateEntitlement,
                SimpleEvent::WarnEntitlement,
                SimpleEvent::UpdateEntitlement,
                SimpleEvent::WarnEntitlement,
                SimpleEvent::HaltEntitlement
            ]
        );
    }

    fn entitlement_with_claim(warn_delta: u64, halt_delta: u64) -> Entitlement {
        let now = SystemTime::now();
        Entitlement {
            claims: Some(Claims {
                iss: "".to_string(),
                sub: "".to_string(),
                aud: OneOrMany::One(Audience::SelfHosted),
                warn_at: now + Duration::from_millis(warn_delta),
                halt_at: now + Duration::from_millis(halt_delta),
            }),
        }
    }

    fn entitlement_with_no_claim() -> Entitlement {
        Entitlement { claims: None }
    }

    #[derive(Eq, PartialEq, Debug)]
    enum SimpleEvent {
        UpdateConfiguration,
        NoMoreConfiguration,
        UpdateSchema,
        NoMoreSchema,
        UpdateEntitlement,
        HaltEntitlement,
        WarnEntitlement,
        NoMoreEntitlement,
        Shutdown,
    }

    impl From<Event> for SimpleEvent {
        fn from(value: Event) -> Self {
            match value {
                Event::UpdateConfiguration(_) => SimpleEvent::UpdateConfiguration,
                Event::NoMoreConfiguration => SimpleEvent::NoMoreConfiguration,
                Event::UpdateSchema(_) => SimpleEvent::UpdateSchema,
                Event::NoMoreSchema => SimpleEvent::NoMoreSchema,
                Event::UpdateEntitlement(EntitlementState::EntitledHalt) => {
                    SimpleEvent::HaltEntitlement
                }
                Event::UpdateEntitlement(EntitlementState::EntitledWarn) => {
                    SimpleEvent::WarnEntitlement
                }
                Event::UpdateEntitlement(_) => SimpleEvent::UpdateEntitlement,
                Event::NoMoreEntitlement => SimpleEvent::NoMoreEntitlement,
                Event::Shutdown => SimpleEvent::Shutdown,
            }
        }
    }
}
