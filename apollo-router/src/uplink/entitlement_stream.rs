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
use futures::Stream;
use graphql_client::GraphQLQuery;
use pin_project_lite::pin_project;
use thiserror::Error;
use tokio_util::time::DelayQueue;

use crate::router::Event;
use crate::uplink::entitlement::Entitlement;
use crate::uplink::entitlement::EntitlementReport;
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
    struct EntitlementExpander<Upstream>
    where
        Upstream: Stream<Item = Event>,
    {
        #[pin]
        checks: DelayQueue<Event>,
        #[pin]
        upstream: Upstream,
    }
}

impl<Upstream> Stream for EntitlementExpander<Upstream>
where
    Upstream: Stream<Item = Event>,
{
    type Item = Event;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let mut this = self.project();
        let active_checks = !this.checks.is_empty();
        match this.checks.poll_expired(cx) {
            // We have an expired claim that needs checking
            Poll::Ready(Some(item)) => Poll::Ready(Some(item.into_inner())),

            // No expired claim is ready so go for upstream
            Poll::Pending | Poll::Ready(None) => {
                let next = this.upstream.poll_next(cx);
                match (&next, active_checks) {
                    // Upstream has a new event with a claim
                    (
                        Poll::Ready(Some(Event::UpdateEntitlement(Entitlement {
                            claims: Some(claim),
                            ..
                        }))),
                        _,
                    ) => {
                        // We got a new claim, so clear the previous checks.
                        this.checks.clear();
                        // Insert the new checks.
                        this.checks.insert_at(
                            Event::WarnEntitlement,
                            (to_positive_instant(claim.warn_at)).into(),
                        );
                        this.checks.insert_at(
                            Event::HaltEntitlement,
                            (to_positive_instant(claim.halt_at)).into(),
                        );
                        next
                    }
                    // Upstream has a new event with no claim
                    (Poll::Ready(Some(_)), _) => {
                        // As we have no claim clear the checks
                        this.checks.clear();
                        next
                    }
                    // There were still active checks, so go for pending
                    (Poll::Ready(None), true) => Poll::Pending,
                    // Upstream is exhausted and there are no checks left
                    (Poll::Ready(None), false) => Poll::Ready(None),
                    // Upstream not exhausted
                    (Poll::Pending, _) => Poll::Pending,
                }
            }
        }
    }
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

trait EventStreamExt: Stream<Item = Event> {
    fn expand_entitlements(self) -> EntitlementExpander<Self>
    where
        Self: Sized,
    {
        EntitlementExpander {
            checks: Default::default(),
            upstream: self,
        }
    }
}

impl<T: Stream<Item = Event>> EventStreamExt for T {}

#[cfg(test)]
mod test {

    use std::time::Duration;
    use std::time::Instant;
    use std::time::SystemTime;

    use futures::SinkExt;
    use futures::StreamExt;

    use crate::router::Event;
    use crate::uplink::entitlement::Audience;
    use crate::uplink::entitlement::Claims;
    use crate::uplink::entitlement::Entitlement;
    use crate::uplink::entitlement::OneOrMany;
    use crate::uplink::entitlement_stream::to_positive_instant;
    use crate::uplink::entitlement_stream::EntitlementRequest;
    use crate::uplink::entitlement_stream::EventStreamExt;
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
    async fn entitlement_expander_claim() {
        let events_stream = futures::stream::iter(vec![entitlement_with_claim(0, 15)])
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
    async fn entitlement_expander_no_claim() {
        let events_stream = futures::stream::iter(vec![entitlement_with_no_claim()])
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
            entitlement_with_claim(0, 15),
        ])
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

    #[tokio::test]
    async fn entitlement_expander_claim_pause_claim() {
        let (mut tx, rx) = futures::channel::mpsc::channel(10);
        let events_stream = rx.expand_entitlements().map(SimpleEvent::from);

        tokio::task::spawn(async move {
            // This simulates a new claim coming in before in between the warning and halt
            let _ = tx.send(entitlement_with_claim(0, 30)).await;
            tokio::time::sleep(Duration::from_millis(5)).await;
            let _ = tx.send(entitlement_with_claim(0, 30)).await;
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

    fn entitlement_with_claim(warn_delta: u64, halt_delta: u64) -> Event {
        let now = SystemTime::now();
        Event::UpdateEntitlement(Entitlement {
            claims: Some(Claims {
                iss: "".to_string(),
                sub: "".to_string(),
                aud: OneOrMany::One(Audience::SelfHosted),
                warn_at: now + Duration::from_millis(warn_delta),
                halt_at: now + Duration::from_millis(halt_delta),
            }),
            configuration_restrictions: vec![],
        })
    }

    fn entitlement_with_no_claim() -> Event {
        Event::UpdateEntitlement(Entitlement {
            claims: None,
            configuration_restrictions: vec![],
        })
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
                Event::UpdateEntitlement(_) => SimpleEvent::UpdateEntitlement,
                Event::WarnEntitlement => SimpleEvent::WarnEntitlement,
                Event::HaltEntitlement => SimpleEvent::HaltEntitlement,
                Event::NoMoreEntitlement => SimpleEvent::NoMoreEntitlement,
                Event::Shutdown => SimpleEvent::Shutdown,
            }
        }
    }
}
