use std::sync::Arc;

use apollo_compiler::ast::Document;
use tower::ServiceExt;

use crate::plugin::test::MockRouterService;
use crate::plugin::test::MockSupergraphService;
use crate::plugin::Plugin;
use crate::plugin::PluginInit;
use crate::plugins::progressive_override::Config;
use crate::plugins::progressive_override::ProgressiveOverridePlugin;
use crate::plugins::progressive_override::LABELS_TO_OVERRIDE_KEY;
use crate::plugins::progressive_override::UNRESOLVED_LABELS_KEY;
use crate::services::layers::query_analysis::ParsedDocument;
use crate::services::layers::query_analysis::ParsedDocumentInner;
use crate::services::router;
use crate::services::supergraph;
use crate::services::RouterResponse;
use crate::services::SupergraphResponse;
use crate::Context;
use crate::TestHarness;

const SCHEMA: &str = include_str!("testdata/supergraph.graphql");
const SCHEMA_NO_USAGES: &str = include_str!("testdata/supergraph_no_usages.graphql");

async fn get_supergraph_service() -> supergraph::BoxCloneService {
    TestHarness::builder()
        .configuration_json(serde_json::json! {{
            "plugins": {
                "experimental.expose_query_plan": true
            }
        }})
        .unwrap()
        .schema(SCHEMA)
        .build_supergraph()
        .await
        .unwrap()
}

#[tokio::test]
async fn todo() {
    let query = "{ percent0 { foo } }";
    let parsed_doc: ParsedDocument = Arc::from(ParsedDocumentInner {
        ast: Document::parse(query, "query.graphql").unwrap(),
        ..Default::default()
    });

    let context: Context = Context::new();
    context
        .extensions()
        .lock()
        .insert::<ParsedDocument>(parsed_doc);

    let request = supergraph::Request::fake_builder()
        .query(query)
        .context(context)
        .header("Apollo-Expose-Query-Plan", "true")
        .build()
        .unwrap();

    let response = get_supergraph_service()
        .await
        .oneshot(request)
        .await
        .unwrap()
        .next_response()
        .await
        .unwrap();

    insta::assert_json_snapshot!(serde_json::to_value(response).unwrap());

    let context = Context::new();
    context
        .insert(
            LABELS_TO_OVERRIDE_KEY,
            ["percent(100)"]
                .iter()
                .map(|s| Arc::new(s.to_string()))
                .collect::<Vec<_>>(),
        )
        .unwrap();

    let query = "{ percent100 { foo } }";
    let parsed_doc: ParsedDocument = Arc::from(ParsedDocumentInner {
        ast: Document::parse(query, "query.graphql").unwrap(),
        ..Default::default()
    });

    context
        .extensions()
        .lock()
        .insert::<ParsedDocument>(parsed_doc);

    let overridden_request = supergraph::Request::fake_builder()
        .query(query)
        .header("Apollo-Expose-Query-Plan", "true")
        .context(context)
        .build()
        .unwrap();

    let overridden_response = get_supergraph_service()
        .await
        .oneshot(overridden_request)
        .await
        .unwrap()
        .next_response()
        .await
        .unwrap();

    insta::assert_json_snapshot!(serde_json::to_value(overridden_response).unwrap());
}

#[tokio::test]
async fn plugin_disables_itself_with_no_progressive_override_usages() {
    let plugin = ProgressiveOverridePlugin::new(PluginInit::fake_new(
        Config {},
        Arc::new(SCHEMA_NO_USAGES.to_string()),
    ))
    .await
    .unwrap();

    assert!(!plugin.enabled);
}

#[tokio::test]
async fn plugin_enables_itself_with_progressive_override_usages() {
    let plugin = ProgressiveOverridePlugin::new(PluginInit::fake_new(
        Config {},
        Arc::new(SCHEMA.to_string()),
    ))
    .await
    .unwrap();

    assert!(plugin.enabled);
}

#[tokio::test]
async fn plugin_router_service_adds_all_arbitrary_labels_to_context() {
    // This test ensures that the _router_service_ adds all of the arbitrary
    // labels to the context so coprocessors can resolve them. At this stage,
    // there's no concern about any percentage-based labels yet.
    let mut mock_service = MockRouterService::new();
    mock_service.expect_call().returning(move |request| {
        let labels_on_context = request
            .context
            .get::<_, Vec<Arc<String>>>(UNRESOLVED_LABELS_KEY)
            .unwrap()
            .unwrap();

        // this plugin handles the percent-based labels, so we don't want to add
        // those to the context for other coprocessors to resolve
        assert!(!labels_on_context.contains(&Arc::new("percent(0)".to_string())));
        assert!(!labels_on_context.contains(&Arc::new("percent(100)".to_string())));
        assert!(labels_on_context.len() == 3);
        assert!(vec!["bar", "baz", "foo"]
            .into_iter()
            .all(|s| labels_on_context.contains(&Arc::new(s.to_string()))));
        RouterResponse::fake_builder().build()
    });

    let service_stack = ProgressiveOverridePlugin::new(PluginInit::fake_new(
        Config {},
        Arc::new(SCHEMA.to_string()),
    ))
    .await
    .unwrap()
    .router_service(mock_service.boxed());

    let _ = service_stack
        .oneshot(router::Request::fake_builder().build().unwrap())
        .await;
}

struct LabelAssertions {
    query: &'static str,
    expected_labels: Vec<&'static str>,
    absent_labels: Vec<&'static str>,
    labels_from_coprocessors: Vec<&'static str>,
}

// We're testing a few things with this function. For a given query, we want to
// assert:
// 1. The expected labels are present in the context
// 2. The absent labels are not present in the context
//
// Additionally, we can simulate the inclusion of any other labels that may have
// been provided by "coprocessors".
async fn assert_expected_and_absent_labels_for_supergraph_service(
    label_assertions: LabelAssertions,
) {
    let LabelAssertions {
        query,
        expected_labels,
        absent_labels,
        labels_from_coprocessors,
    } = label_assertions;

    let mut mock_service = MockSupergraphService::new();

    mock_service.expect_call().returning(move |request| {
        let labels_to_override = request
            .context
            .get::<_, Vec<String>>(LABELS_TO_OVERRIDE_KEY)
            .unwrap()
            .unwrap();

        for label in &expected_labels {
            assert!(labels_to_override.contains(&label.to_string()));
        }
        for label in &absent_labels {
            assert!(!labels_to_override.contains(&label.to_string()));
        }
        SupergraphResponse::fake_builder().build()
    });

    let service_stack = ProgressiveOverridePlugin::new(PluginInit::fake_new(
        Config {},
        Arc::new(SCHEMA.to_string()),
    ))
    .await
    .unwrap()
    .supergraph_service(mock_service.boxed());

    // plugin depends on the parsed document being in the context so we'll add
    // it ourselves for testing purposes
    let parsed_doc: ParsedDocument = Arc::from(ParsedDocumentInner {
        ast: Document::parse(query, "query.graphql").unwrap(),
        ..Default::default()
    });

    let context = Context::new();
    context
        .extensions()
        .lock()
        .insert::<ParsedDocument>(parsed_doc);

    context
        .insert(
            LABELS_TO_OVERRIDE_KEY,
            labels_from_coprocessors
                .iter()
                .map(|s| s.to_string())
                .collect::<Vec<_>>(),
        )
        .unwrap();

    let request = supergraph::Request::fake_builder()
        .context(context)
        .query(query)
        .build()
        .unwrap();

    let _ = service_stack.oneshot(request).await;
}

#[tokio::test]
async fn plugin_supergraph_service_adds_percent_labels_to_context() {
    let label_assertions = LabelAssertions {
        query: "{ percent100 { foo } }",
        expected_labels: vec!["percent(100)"],
        absent_labels: vec!["percent(0)"],
        labels_from_coprocessors: vec![],
    };
    assert_expected_and_absent_labels_for_supergraph_service(label_assertions).await;
}

#[tokio::test]
async fn plugin_supergraph_service_trims_extraneous_labels() {
    let label_assertions = LabelAssertions {
        query: "{ percent100 { foo } }",
        // the foo label is relevant to the `foo` field (and resolved by the
        // "coprocessor"), so we expect it to be preserved
        expected_labels: vec!["percent(100)", "foo"],
        // `baz` exists in the schema but is not relevant to this query, so we expect it to be trimmed
        // `bogus` is not in the schema at all, so we expect it to be trimmed
        absent_labels: vec!["percent(0)", "bogus", "baz"],
        labels_from_coprocessors: vec!["foo", "baz", "bogus"],
    };
    assert_expected_and_absent_labels_for_supergraph_service(label_assertions).await;
}

#[tokio::test]
async fn plugin_supergraph_service_trims_0pc_label() {
    let label_assertions = LabelAssertions {
        query: "{ percent0 { foo } }",
        expected_labels: vec!["foo"],
        // the router will always resolve percent(0) to false
        absent_labels: vec!["percent(0)"],
        labels_from_coprocessors: vec!["foo"],
    };
    assert_expected_and_absent_labels_for_supergraph_service(label_assertions).await;
}
