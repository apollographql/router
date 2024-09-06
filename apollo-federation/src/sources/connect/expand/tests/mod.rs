/// Macro to quickly expand a test case based on a test schema name.
macro_rules! test_expands {
    ($($name:ident),* $(,)*) => {
        $(
            mod $name {
                #[test]
                fn it_expands_supergraph() {
                    use insta::assert_debug_snapshot;
                    use insta::assert_snapshot;

                    use crate::sources::connect::expand::expand_connectors;
                    use crate::sources::connect::expand::ExpansionResult;

                    let to_expand = include_str!(concat!("schemas/", stringify!($name), ".graphql"));
                    let ExpansionResult::Expanded {
                        raw_sdl,
                        api_schema,
                        connectors,
                    } = expand_connectors(to_expand).unwrap()
                    else {
                        panic!(
                            concat!(
                                "expected expansion to actually expand subgraphs for schemas/",
                                stringify!($name),
                                ".graphql"
                            )
                        );
                    };

                    assert_snapshot!(api_schema);
                    assert_debug_snapshot!(connectors.by_service_name);
                    assert_snapshot!(raw_sdl);
                }
            }
        )*
    };
}
macro_rules! test_ignores {
    ($($name:ident),* $(,)*) => {
        $(
            mod $name {
                #[test]
                fn it_ignores_supergraph() {
                    use crate::sources::connect::expand::expand_connectors;
                    use crate::sources::connect::expand::ExpansionResult;

                    let to_ignore = include_str!(concat!("schemas/", stringify!($name), ".graphql"));
                    let ExpansionResult::Unchanged = expand_connectors(to_ignore).unwrap() else {
                        panic!(
                            concat!(
                                "expected expansion to ignore non-connector supergraph for schemas/",
                                stringify!($name),
                                ".graphql"
                            )
                        );
                    };
                }
            }
        )*
    };
}

test_expands! {
    nested_inputs,
    realistic,
    simple,
    steelthread,
    types_used_twice,
    carryover,
    circular,
    normalize_names
}

test_ignores! {
    directives,
    ignored,
}
