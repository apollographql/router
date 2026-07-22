use std::fs::read_to_string;

use insta::assert_debug_snapshot;
use insta::assert_snapshot;
use insta::glob;

use crate::ApiSchemaOptions;
use crate::connectors::expand::ExpansionResult;
use crate::connectors::expand::expand_connectors;

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

/// `Connectors.by_coordinate` must be a lossless re-index of `by_service_name`:
/// coordinates are unique per connector, so the two indices cover exactly the
/// same connector set, and every connector is reachable by its
/// `ConnectId::coordinate()`. This is the index the source-aware dispatch path
/// (`resolve_connector`, B-3) resolves against, so parity with the
/// service-name-keyed set is load-bearing.
#[test]
fn by_coordinate_is_a_lossless_reindex() {
    glob!("schemas/expand", "*.graphql", |path| {
        let to_expand = read_to_string(path).unwrap();
        let ExpansionResult::Expanded { connectors, .. } = expand_connectors(
            &to_expand,
            &ApiSchemaOptions {
                include_defer: true,
                ..Default::default()
            },
        )
        .unwrap() else {
            panic!("expected expansion to actually expand subgraphs for {path:?}");
        };

        // Lossless: one coordinate entry per connector, none collapsed.
        assert_eq!(
            connectors.by_coordinate.len(),
            connectors.by_service_name.len(),
            "coordinate re-index lost or merged connectors for {path:?}"
        );

        // Every connector is reachable by its own coordinate, and the entry
        // found there is the same connector (same id) as in by_service_name.
        for connector in connectors.by_service_name.values() {
            let coordinate = connector.id.coordinate();
            let found = connectors.by_coordinate.get(&coordinate).unwrap_or_else(|| {
                panic!("connector {coordinate} missing from by_coordinate for {path:?}")
            });
            assert_eq!(found.id, connector.id, "coordinate {coordinate} resolved to the wrong connector for {path:?}");
        }
    });
}

/// `unexpanded_connectors` (source-aware path) builds the connector set from the
/// raw subgraphs without expansion: `by_coordinate` is the 1:1 authoritative
/// index, `by_service_name` is keyed by the connector's *original* subgraph name
/// (so `spec/schema.rs`'s is-connector membership test works on the raw SDL's
/// `join__Graph` names), and a non-connector supergraph yields `None`.
#[test]
fn unexpanded_connectors_indexes_by_coordinate_and_subgraph_name() {
    use crate::connectors::expand::unexpanded_connectors;

    let sdl = read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/src/connectors/expand/tests/schemas/expand/steelthread.graphql"
    ))
    .unwrap();

    let connectors = unexpanded_connectors(&sdl)
        .unwrap()
        .expect("steelthread has connectors");

    // by_coordinate is 1:1 and authoritative — every connector reachable by its
    // own coordinate, keyed on the original `connectors` subgraph.
    assert!(!connectors.by_coordinate.is_empty());
    for connector in connectors.by_coordinate.values() {
        assert_eq!(
            connectors.by_coordinate.get(&connector.id.coordinate()).map(|c| &c.id),
            Some(&connector.id),
        );
        assert!(
            connector.id.coordinate().starts_with("connectors:"),
            "coordinate should carry the raw subgraph name: {}",
            connector.id.coordinate()
        );
    }

    // by_service_name is keyed by the *original* subgraph name (many-to-one):
    // all steelthread connectors live in `connectors`, so that single key is
    // present and the map is smaller than the 1:1 coordinate index.
    assert!(connectors.by_service_name.contains_key("connectors"));
    assert!(
        connectors.by_service_name.len() <= connectors.by_coordinate.len(),
        "by_service_name collapses connectors sharing a subgraph name"
    );
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
