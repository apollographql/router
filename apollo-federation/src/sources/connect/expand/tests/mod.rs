use insta::assert_debug_snapshot;
use insta::assert_snapshot;

use crate::sources::connect::expand::expand_connectors;
use crate::sources::connect::expand::ExpansionResult;

#[test]
fn it_skips_non_connector_supergraphs() {
    let to_ignore = include_str!("./schemas/ignored.graphql");
    let ExpansionResult::Unchanged = expand_connectors(to_ignore).unwrap() else {
        panic!("expected expansion to ignore non-connector supergraph");
    };
}

#[test]
fn it_expands_a_supergraph() {
    let to_expand = include_str!("./schemas/simple.graphql");
    let ExpansionResult::Expanded {
        raw_sdl,
        api_schema,
        connectors,
    } = expand_connectors(to_expand).unwrap()
    else {
        panic!("expected expansion to actually expand subgraphs");
    };

    assert_snapshot!(api_schema);
    assert_debug_snapshot!(connectors.by_service_name);
    assert_snapshot!(raw_sdl);
}

#[test]
fn it_expands_a_realistic_supergraph() {
    let to_expand = include_str!("./schemas/realistic.graphql");
    let ExpansionResult::Expanded {
        raw_sdl,
        api_schema,
        connectors,
    } = expand_connectors(to_expand).unwrap()
    else {
        panic!("expected expansion to actually expand subgraphs");
    };

    assert_snapshot!(api_schema);
    assert_debug_snapshot!(connectors.by_service_name);
    assert_snapshot!(raw_sdl);
}

#[test]
fn it_expands_steelthread_supergraph() {
    let to_expand = include_str!("./schemas/steelthread.graphql");
    let ExpansionResult::Expanded {
        raw_sdl,
        api_schema,
        connectors,
    } = expand_connectors(to_expand).unwrap()
    else {
        panic!("expected expansion to actually expand subgraphs");
    };

    assert_snapshot!(api_schema);
    assert_debug_snapshot!(connectors.by_service_name);
    assert_snapshot!(raw_sdl);
}
