use std::fs::read_to_string;

use insta::assert_debug_snapshot;
use insta::assert_snapshot;
use insta::glob;

use crate::sources::connect::expand::expand_connectors;
use crate::sources::connect::expand::ExpansionResult;

#[test]
fn it_expand_supergraph() {
    insta::with_settings!({prepend_module_to_snapshot => false}, {
        glob!("schemas/expand", "*.graphql", |path| {
            let to_expand = read_to_string(path).unwrap();
            let ExpansionResult::Expanded {
                raw_sdl,
                api_schema,
                connectors,
            } = expand_connectors(&to_expand).unwrap()
            else {
                panic!("expected expansion to actually expand subgraphs for {path:?}");
            };

            assert_snapshot!(api_schema);
            assert_debug_snapshot!(connectors.by_service_name);
            assert_snapshot!(raw_sdl);
        });
    });
}

#[test]
fn it_ignores_supergraph() {
    insta::with_settings!({prepend_module_to_snapshot => false}, {
        glob!("schemas/ignore", "*.graphql", |path| {
            let to_ignore = read_to_string(path).unwrap();
            let ExpansionResult::Unchanged = expand_connectors(&to_ignore).unwrap() else {
                panic!("expected expansion to ignore non-connector supergraph for {path:?}");
            };
        });
    });
}
