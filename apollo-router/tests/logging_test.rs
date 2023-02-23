use std::sync::Arc;
use std::sync::Mutex;

use apollo_router::graphql;
use apollo_router::services::router;
use apollo_router::services::supergraph;
use tower::ServiceExt;
use tracing::field;
use tracing::Level;
use tracing::Metadata;
use tracing::Subscriber;
use tracing_core::dispatcher;
use tracing_core::Dispatch;
use tracing_subscriber::registry::LookupSpan;
use tracing_subscriber::Registry;

struct TestLogSubscriber {
    registry: Registry,
    event_metadata: Arc<Mutex<Vec<&'static Metadata<'static>>>>,
}

impl Subscriber for TestLogSubscriber {
    fn enabled(&self, _metadata: &Metadata<'_>) -> bool {
        true
    }

    fn new_span(&self, span: &tracing_core::span::Attributes<'_>) -> tracing_core::span::Id {
        self.registry.new_span(span)
    }

    fn record(&self, span: &tracing_core::span::Id, values: &tracing_core::span::Record<'_>) {
        self.registry.record(span, values)
    }

    fn record_follows_from(&self, span: &tracing_core::span::Id, follows: &tracing_core::span::Id) {
        self.registry.record_follows_from(span, follows)
    }

    fn event(&self, event: &tracing::Event<'_>) {
        if event.metadata().target().starts_with("apollo_router")
            && event.metadata().level() == &Level::INFO
        {
            self.event_metadata.lock().unwrap().push(event.metadata());
        }
    }

    fn enter(&self, span: &tracing_core::span::Id) {
        self.registry.enter(span)
    }

    fn exit(&self, span: &tracing_core::span::Id) {
        self.registry.exit(span)
    }
}

impl<'a> LookupSpan<'a> for TestLogSubscriber {
    type Data = tracing_subscriber::registry::Data<'a>;

    fn span_data(&'a self, id: &tracing::Id) -> Option<Self::Data> {
        self.registry.span_data(id)
    }
}

async fn setup_router(config: serde_json::Value) -> router::BoxCloneService {
    apollo_router::TestHarness::builder()
        .with_subgraph_network_requests()
        .configuration_json(config)
        .unwrap()
        .schema(include_str!("fixtures/supergraph.graphql"))
        .build_router()
        .await
        .unwrap()
}

async fn query_with_router(
    router: router::BoxCloneService,
    request: supergraph::Request,
) -> graphql::Response {
    serde_json::from_slice(
        router
            .oneshot(request.try_into().unwrap())
            .await
            .unwrap()
            .next_response()
            .await
            .unwrap()
            .unwrap()
            .to_vec()
            .as_slice(),
    )
    .unwrap()
}

#[derive(Default, Clone, PartialEq, Debug)]
struct LoggingCount {
    supergraph_request_headers_count: usize,
    supergraph_request_body_count: usize,
    supergraph_response_body_count: usize,
    supergraph_response_headers_count: usize,
    subgraph_request_body_count: usize,
    subgraph_request_headers_count: usize,
    subgraph_response_body_count: usize,
    subgraph_response_headers_count: usize,
}

impl LoggingCount {
    const RESPONSE_BODY: &'static str = "http.response.body";
    const RESPONSE_HEADERS: &'static str = "http.response.headers";
    const REQUEST_HEADERS: &'static str = "http.request.headers";
    const REQUEST_BODY: &'static str = "http.request.body";

    fn count(&mut self, fields: &field::FieldSet) {
        let fields_name: Vec<&str> = fields.iter().map(|f| f.name()).collect();
        if fields_name.contains(&"apollo.subgraph.name") {
            if fields_name.contains(&Self::RESPONSE_BODY) {
                self.subgraph_response_body_count += 1;
            }
            if fields_name.contains(&Self::RESPONSE_HEADERS) {
                self.subgraph_response_headers_count += 1;
            }
            if fields_name.contains(&Self::REQUEST_HEADERS) {
                self.subgraph_request_headers_count += 1;
            }
            if fields_name.contains(&Self::REQUEST_BODY) {
                self.subgraph_request_body_count += 1;
            }
        } else {
            if fields_name.contains(&Self::RESPONSE_BODY) {
                self.supergraph_response_body_count += 1;
            }
            if fields_name.contains(&Self::RESPONSE_HEADERS) {
                self.supergraph_response_headers_count += 1;
            }
            if fields_name.contains(&Self::REQUEST_HEADERS) {
                self.supergraph_request_headers_count += 1;
            }
            if fields_name.contains(&Self::REQUEST_BODY) {
                self.supergraph_request_body_count += 1;
            }
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn simple_query_should_display_logs_for_subgraph_and_supergraph() {
    let logging_config = serde_json::json!({
        "when_header": [{
            "name": "custom-header",
            "match": "^foo.*",
            "headers": true
        }]
    });
    let request = supergraph::Request::fake_builder()
        .header("custom-header", "foobar")
        .query(r#"{ topProducts { upc name reviews {id product { name } author { id name } } } }"#)
        .variable("topProductsFirst", 2_i32)
        .variable("reviewsForAuthorAuthorId", 1_i32)
        .build()
        .expect("expecting valid request");

    let event_store = Arc::new(Mutex::new(Vec::new()));
    dispatcher::set_global_default(Dispatch::new(TestLogSubscriber {
        event_metadata: event_store.clone(),
        registry: Registry::default(),
    }))
    .expect("subscriber must be set");

    let router = setup_router(serde_json::json!({"telemetry": {"tracing": {},
    "experimental_logging": logging_config,
    "apollo": {
        "schema_id": ""
    }}}))
    .await;
    let actual = query_with_router(router, request).await;

    assert_eq!(0, actual.errors.len());
    let mut logging_count = LoggingCount::default();
    for event in &*event_store.lock().unwrap() {
        logging_count.count(event.fields());
    }

    assert_eq!(logging_count.supergraph_request_headers_count, 1);
    assert_eq!(logging_count.supergraph_request_body_count, 0);
    assert_eq!(logging_count.supergraph_response_body_count, 0);
    assert_eq!(logging_count.supergraph_response_headers_count, 1);
    assert_eq!(logging_count.subgraph_response_body_count, 0);
    assert_eq!(logging_count.subgraph_response_headers_count, 4);
    assert_eq!(logging_count.subgraph_request_headers_count, 4);
    assert_eq!(logging_count.subgraph_request_body_count, 0);
}
