pub(crate) const CONNECT_SPAN_NAME: &str = "connect";

#[cfg(test)]
mod tests {
    use tower::ServiceExt;
    use tower_service::Service;
    use tracing_fluent_assertions::AssertionRegistry;
    use tracing_fluent_assertions::AssertionsLayer;
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::Registry;

    use super::CONNECT_SPAN_NAME;
    use crate::services::supergraph;

    #[tokio::test]
    async fn connector_tracing_span() {
        let assertion_registry = AssertionRegistry::default();
        let base_subscriber = Registry::default();
        let subscriber = base_subscriber.with(AssertionsLayer::new(&assertion_registry));
        let _guard = tracing::subscriber::set_default(subscriber);

        let found_connector_span = assertion_registry
            .build()
            .with_name(CONNECT_SPAN_NAME)
            .with_span_field("apollo.connector.field")
            .with_span_field("apollo.connector.type")
            .with_span_field("apollo.connector.detail")
            .was_entered()
            .was_exited()
            .finalize();

        let mut test_harness = crate::TestHarness::builder()
            .schema(include_str!("testdata/tracing.graphql"))
            .build_supergraph()
            .await
            .expect("expecting valid supergraph");

        let request = supergraph::Request::fake_builder()
            .query(" { users { id } }")
            .build()
            .expect("expecting valid request");

        let response = test_harness
            .ready()
            .await
            .unwrap()
            .call(request)
            .await
            .unwrap()
            .next_response()
            .await
            .unwrap();

        assert!(response.errors.is_empty());
        found_connector_span.assert();
    }
}
