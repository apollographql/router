use std::fs::read_to_string;

use insta::assert_debug_snapshot;
use insta::assert_snapshot;
use insta::glob;

use crate::ApiSchemaOptions;
use crate::connectors::expand::ExpansionResult;
use crate::connectors::expand::expand_connectors;
use crate::schema::FederationSchema;
use crate::supergraph::extract_subgraphs_from_supergraph;

#[test]
fn it_expand_supergraph() {
    insta::with_settings!({prepend_module_to_snapshot => false}, {
        glob!("schemas/expand", "*.graphql", |path| {
            let to_expand = read_to_string(path).unwrap();
            let ExpansionResult::Expanded {
                raw_sdl,
                api_schema,
                connectors,
            } = expand_connectors(&to_expand, &ApiSchemaOptions { include_defer: true, ..Default::default() }).unwrap()
            else {
                panic!("expected expansion to actually expand subgraphs for {path:?}");
            };

            assert_snapshot!("api", api_schema);
            assert_debug_snapshot!("connectors", connectors.by_service_name);
            assert_snapshot!("supergraph", raw_sdl);
        });
    });
}

/// @cacheTag: The expanded supergraph's @join__directive `graphs`
/// list includes all synthetic connector subgraphs, but only one owns the
/// field — `extract_subgraphs_from_supergraph` must tolerate this.
#[test]
fn cache_tag_on_connector_field_does_not_crash_extraction() {
    let to_expand = read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/src/connectors/expand/tests/schemas/expand/cache_tag_on_connector.graphql"
    ))
    .unwrap();

    let ExpansionResult::Expanded { raw_sdl, .. } =
        expand_connectors(&to_expand, &ApiSchemaOptions::default()).unwrap()
    else {
        panic!("expected expansion");
    };

    let schema = apollo_compiler::Schema::parse_and_validate(&raw_sdl, "expanded.graphql")
        .expect("expanded supergraph should be valid GraphQL");
    let fed_schema =
        FederationSchema::new(schema.into_inner()).expect("should create FederationSchema");

    extract_subgraphs_from_supergraph(&fed_schema, Some(true))
        .expect("extract_subgraphs_from_supergraph should succeed");
}

#[test]
fn it_ignores_supergraph() {
    insta::with_settings!({prepend_module_to_snapshot => false}, {
        glob!("schemas/ignore", "*.graphql", |path| {
            let to_ignore = read_to_string(path).unwrap();
            let ExpansionResult::Unchanged = expand_connectors(&to_ignore, &ApiSchemaOptions::default()).unwrap() else {
                panic!("expected expansion to ignore non-connector supergraph for {path:?}");
            };
        });
    });
}
