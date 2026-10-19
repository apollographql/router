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

#[test]
fn it_preserves_one_of_on_input_objects() {
    let to_expand =
        read_to_string("src/connectors/expand/tests/schemas/expand/recursive_input.graphql")
            .expect("fixture should be readable")
            .replace(
                "input AnInput\n  @join__type(graph: CONNECTORS)\n{",
                "input AnInput\n  @join__type(graph: CONNECTORS)\n  @oneOf\n{",
            );
    assert!(
        to_expand.contains("@oneOf"),
        "fixture should have @oneOf applied"
    );

    let ExpansionResult::Expanded { raw_sdl, .. } =
        expand_connectors(&to_expand, &ApiSchemaOptions::default())
            .expect("expansion should succeed")
    else {
        panic!("expected expansion to actually expand subgraphs");
    };

    let expanded = apollo_compiler::Schema::parse(&raw_sdl, "expanded.graphql")
        .expect("expanded supergraph should parse");
    let an_input = expanded
        .get_input_object("AnInput")
        .expect("AnInput should exist");
    assert!(an_input.is_one_of(), "AnInput should keep @oneOf");
}
