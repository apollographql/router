use std::future::Ready;
use std::io;
use std::sync::Arc;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;
use std::task::Context;
use std::task::Poll;

use apollo_compiler::ExecutableDocument;
use apollo_compiler::Schema as CompilerSchema;
use apollo_federation::query_plan::serializable_document::SerializableDocument;
use serde_json_bytes::Value;
use serde_json_bytes::json;
use tokio::sync::mpsc;
use tower::BoxError;
use tower::Layer;
use tower::Service;
use tower::ServiceExt;

use super::EventSubscriptionLayer;
use crate::Context as RouterContext;
use crate::configuration::events::EventsConfiguration;
use crate::json_ext::Path;
use crate::plugins::subscription::SubscriptionConfig;
use crate::plugins::subscription::event::EventRuntime;
use crate::query_planner::OperationKind;
use crate::query_planner::fetch::Variables;
use crate::query_planner::subscription::SubscriptionNode;
use crate::services::FetchResponse;
use crate::services::fetch::SubscriptionRequest;
use crate::spec::Schema;

const EVENT_SCHEMA: &str =
    include_str!("../../../../../tests/integration/subscriptions/fixtures/event_stream.graphql");
const EVENT_SUBGRAPH_SCHEMA: &str = r#"
    type Query {
        _noop: Boolean
    }

    type Subscription {
        productUpdated(id: ID!): Product
        notAnEvent: Product
    }

    type Product {
        id: ID!
    }
"#;

fn runtime(provider_type: &str, provider_options: &str) -> Arc<EventRuntime> {
    let configuration: EventsConfiguration = serde_yaml::from_str(&format!(
        r#"
providers:
  broker:
    type: {provider_type}
sources:
  product-updates:
    provider: broker
    policy: live
    provider_options: {provider_options}
policies:
  live: {{}}
"#
    ))
    .expect("test event configuration is valid");
    let schema = Arc::new(
        Schema::parse(EVENT_SCHEMA, &Default::default()).expect("test supergraph is valid"),
    );
    Arc::new(EventRuntime::try_new(schema, configuration).expect("event runtime is valid"))
}

fn request(
    query: &str,
    service_name: &str,
    subscription_config: Option<SubscriptionConfig>,
) -> SubscriptionRequest {
    let schema = CompilerSchema::parse_and_validate(EVENT_SUBGRAPH_SCHEMA, "events.graphql")
        .expect("test subgraph schema is valid");
    let operation = ExecutableDocument::parse_and_validate(&schema, query, "operation.graphql")
        .expect("test operation is valid");
    let subscription_node = SubscriptionNode {
        service_name: Arc::from(service_name),
        variable_usages: Vec::new(),
        operation: SerializableDocument::from_parsed(operation),
        operation_name: None,
        operation_kind: OperationKind::Subscription,
        input_rewrites: None,
        output_rewrites: None,
    };
    let (sender, _receiver) = mpsc::channel(1);
    let supergraph_request = Arc::new(
        http::Request::builder()
            .body(crate::graphql::Request::builder().build())
            .expect("request is valid"),
    );

    SubscriptionRequest::builder()
        .context(RouterContext::new())
        .subscription_node(subscription_node)
        .supergraph_request(supergraph_request)
        .variables(Variables::default())
        .current_dir(Path(Vec::new()))
        .sender(sender)
        .subscription_config(subscription_config.unwrap_or_default())
        .build()
}

#[derive(Clone)]
struct ReadinessProbe {
    polls: Arc<AtomicUsize>,
}

impl Service<SubscriptionRequest> for ReadinessProbe {
    type Response = FetchResponse;
    type Error = BoxError;
    type Future = Ready<Result<Self::Response, Self::Error>>;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.polls.fetch_add(1, Ordering::Relaxed);
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, _request: SubscriptionRequest) -> Self::Future {
        std::future::ready(Ok((Value::default(), Vec::new())))
    }
}

#[tokio::test]
async fn delegates_readiness_to_the_fallback_service() {
    let polls = Arc::new(AtomicUsize::new(0));
    let mut service =
        EventSubscriptionLayer::new(runtime("nats_core", "{}")).layer(ReadinessProbe {
            polls: polls.clone(),
        });

    service.ready().await.expect("fallback is ready");

    assert_eq!(polls.load(Ordering::Relaxed), 1);
}

#[tokio::test]
async fn delegates_non_event_subscriptions_to_the_fallback_service() {
    let calls = Arc::new(AtomicUsize::new(0));
    let fallback_calls = calls.clone();
    let fallback = tower::service_fn(move |_request: SubscriptionRequest| {
        fallback_calls.fetch_add(1, Ordering::Relaxed);
        async { Ok::<_, BoxError>((json!({"fallback": true}), Vec::new())) }
    });
    let service = EventSubscriptionLayer::new(runtime("nats_core", "{}")).layer(fallback);

    let response = service
        .oneshot(request(
            "subscription { notAnEvent { id } }",
            "events",
            None,
        ))
        .await
        .expect("fallback call succeeds");

    assert_eq!(response.0, json!({"fallback": true}));
    assert_eq!(calls.load(Ordering::Relaxed), 1);
}

#[tokio::test]
async fn propagates_fallback_service_errors() {
    let fallback = tower::service_fn(|_request: SubscriptionRequest| async {
        Err::<FetchResponse, BoxError>(io::Error::other("fallback failed").into())
    });
    let service = EventSubscriptionLayer::new(runtime("nats_core", "{}")).layer(fallback);

    let error = service
        .oneshot(request(
            "subscription { notAnEvent { id } }",
            "events",
            None,
        ))
        .await
        .expect_err("fallback error is propagated");

    assert_eq!(error.to_string(), "fallback failed");
}

#[tokio::test]
async fn intercepts_event_subscriptions_independently_of_provider() {
    for (provider_type, provider_options) in [
        ("nats_core", "{}"),
        ("nats_jetstream", "{ stream: PRODUCTS }"),
        ("kafka", "{}"),
        ("redis_pubsub", "{}"),
    ] {
        let calls = Arc::new(AtomicUsize::new(0));
        let fallback_calls = calls.clone();
        let fallback = tower::service_fn(move |_request: SubscriptionRequest| {
            fallback_calls.fetch_add(1, Ordering::Relaxed);
            async { Ok::<_, BoxError>((json!({"fallback": true}), Vec::new())) }
        });
        let service =
            EventSubscriptionLayer::new(runtime(provider_type, provider_options)).layer(fallback);
        let subscription_config = SubscriptionConfig {
            max_opened_subscriptions: Some(0),
            ..Default::default()
        };

        let (_, errors) = service
            .oneshot(request(
                "subscription ProductUpdated($id: ID!) { productUpdated(id: $id) { id } }",
                "events",
                Some(subscription_config),
            ))
            .await
            .unwrap_or_else(|error| panic!("{provider_type} service call failed: {error}"));

        assert_eq!(calls.load(Ordering::Relaxed), 0, "{provider_type}");
        assert_eq!(errors.len(), 1, "{provider_type}");
        assert_eq!(
            errors[0]
                .extensions
                .get("code")
                .and_then(|value| value.as_str()),
            Some("SUBSCRIPTION_MAX_LIMIT"),
            "{provider_type}"
        );
    }
}
