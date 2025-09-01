//! Functions for applying a [`SelectionSet`] to a [`JSONSelection`]. This creates a new
//! `JSONSelection` mapping to the fields on the selection set, and excluding parts of the
//! original `JSONSelection` that are not needed by the selection set.

#![cfg_attr(
    not(test),
    deny(
        clippy::exit,
        clippy::panic,
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::indexing_slicing,
        clippy::unimplemented,
        clippy::todo,
        missing_docs
    )
)]

use apollo_compiler::ExecutableDocument;
use apollo_compiler::Node;
use apollo_compiler::executable::Field;
use apollo_compiler::executable::FieldSet;
use apollo_compiler::executable::Selection;
use apollo_compiler::executable::SelectionSet;
use apollo_compiler::name;
use multimap::MultiMap;

use super::known_var::KnownVariable;
use super::lit_expr::LitExpr;
use super::location::Ranged;
use super::location::WithRange;
use super::parser::MethodArgs;
use super::parser::PathList;
use crate::connectors::JSONSelection;
use crate::connectors::PathSelection;
use crate::connectors::SubSelection;
use crate::connectors::json_selection::Alias;
use crate::connectors::json_selection::NamedSelection;
use crate::connectors::json_selection::NamingPrefix;
use crate::connectors::json_selection::TopLevelSelection;

impl JSONSelection {
    /// Apply a selection set to create a new [`JSONSelection`]
    ///
    /// Operations from the query planner will never contain key fields (because
    /// it already has them from a previous fetch) but we might need those
    /// fields for things like sorting entities in a batch. If the optional
    /// `required_keys` is provided, we'll merge those fields into the selection
    /// set before applying it to the JSONSelection.
    pub fn apply_selection_set(
        &self,
        document: &ExecutableDocument,
        selection_set: &SelectionSet,
        required_keys: Option<&FieldSet>,
    ) -> Self {
        let selection_set = required_keys.map_or_else(
            || selection_set.clone(),
            |keys| {
                keys.selection_set.selections.iter().cloned().fold(
                    selection_set.clone(),
                    |mut acc, selection| {
                        acc.push(selection);
                        acc
                    },
                )
            },
        );

        match &self.inner {
            TopLevelSelection::Named(sub) => Self {
                inner: TopLevelSelection::Named(sub.apply_selection_set(document, &selection_set)),
                spec: self.spec,
            },
            TopLevelSelection::Path(path) => Self {
                inner: TopLevelSelection::Path(path.apply_selection_set(document, &selection_set)),
                spec: self.spec,
            },
        }
    }
}

impl SubSelection {
    /// Apply a selection set to create a new [`SubSelection`]
    pub fn apply_selection_set(
        &self,
        document: &ExecutableDocument,
        selection_set: &SelectionSet,
    ) -> Self {
        let mut new_selections = Vec::new();
        let field_map = map_fields_by_name(document, selection_set);

        // When the operation contains __typename, it might be used to complete
        // an entity reference (e.g. `__typename id`) for a subsequent fetch.
        //
        // NOTE: For reasons I don't understand, persisted queries may contain
        // `__typename` for `_entities` queries. We never want to emit
        // `__typename: "_Entity"`, so we'll guard against that case.
        //
        // TODO: this must change before we support interfaces and unions
        // because it will emit the abstract type's name which is invalid.
        if field_map.contains_key("__typename") && selection_set.ty != name!(_Entity) {
            new_selections.push(NamedSelection {
                prefix: NamingPrefix::Alias(Alias::new("__typename")),
                path: PathSelection {
                    path: WithRange::new(
                        PathList::Var(
                            WithRange::new(KnownVariable::Dollar, None),
                            WithRange::new(
                                PathList::Method(
                                    WithRange::new("echo".to_string(), None),
                                    Some(MethodArgs {
                                        args: vec![WithRange::new(
                                            LitExpr::String(selection_set.ty.to_string()),
                                            None,
                                        )],
                                        ..Default::default()
                                    }),
                                    WithRange::new(PathList::Empty, None),
                                ),
                                None,
                            ),
                        ),
                        None,
                    ),
                },
            });
        }

        for selection in &self.selections {
            if let Some(single_key_for_selection) = selection.get_single_key() {
                // In the single-Key case, we can filter out any selections
                // whose single key does not match anything in the field_map.
                if let Some(fields) = field_map.get_vec(single_key_for_selection.as_str()) {
                    for field in fields {
                        let applied_path = selection
                            .path
                            .apply_selection_set(document, &field.selection_set);

                        new_selections.push(NamedSelection {
                            prefix: selection.prefix.clone(),
                            path: applied_path,
                        });
                    }
                } else {
                    // If the selection had a single output key and that key
                    // does not appear in field_map, we can skip the selection.
                }
            } else {
                // If the NamedSelection::Path does not have a single output key
                // (has no alias and is not a single field selection), then it's
                // tricky to know if we should prune the selection, so we
                // conservatively preserve it, using a transformed path.
                new_selections.push(NamedSelection {
                    prefix: selection.prefix.clone(),
                    path: selection.path.apply_selection_set(document, selection_set),
                });
            }
        }

        Self {
            selections: new_selections,
            // Keep the old range even though it may be inaccurate after the
            // removal of selections, since it still indicates where the
            // original SubSelection came from.
            range: self.range.clone(),
        }
    }
}

impl PathSelection {
    /// Apply a selection set to create a new [`PathSelection`]
    pub fn apply_selection_set(
        &self,
        document: &ExecutableDocument,
        selection_set: &SelectionSet,
    ) -> Self {
        Self {
            path: WithRange::new(
                self.path.apply_selection_set(document, selection_set),
                self.path.range(),
            ),
        }
    }
}

impl PathList {
    pub(crate) fn apply_selection_set(
        &self,
        document: &ExecutableDocument,
        selection_set: &SelectionSet,
    ) -> Self {
        match self {
            Self::Var(name, path) => Self::Var(
                name.clone(),
                WithRange::new(
                    path.apply_selection_set(document, selection_set),
                    path.range(),
                ),
            ),
            Self::Key(key, path) => Self::Key(
                key.clone(),
                WithRange::new(
                    path.apply_selection_set(document, selection_set),
                    path.range(),
                ),
            ),
            Self::Expr(expr, path) => Self::Expr(
                expr.clone(),
                WithRange::new(
                    path.apply_selection_set(document, selection_set),
                    path.range(),
                ),
            ),
            Self::Method(method_name, args, path) => Self::Method(
                method_name.clone(),
                args.clone(),
                WithRange::new(
                    path.apply_selection_set(document, selection_set),
                    path.range(),
                ),
            ),
            Self::Question(tail) => Self::Question(WithRange::new(
                tail.apply_selection_set(document, selection_set),
                tail.range(),
            )),
            Self::Selection(sub) => {
                Self::Selection(sub.apply_selection_set(document, selection_set))
            }
            Self::Empty => Self::Empty,
        }
    }
}

fn map_fields_by_name<'a>(
    document: &'a ExecutableDocument,
    set: &'a SelectionSet,
) -> MultiMap<String, &'a Node<Field>> {
    let mut map = MultiMap::new();
    map_fields_by_name_impl(document, set, &mut map);
    map
}

fn map_fields_by_name_impl<'a>(
    document: &'a ExecutableDocument,
    set: &'a SelectionSet,
    map: &mut MultiMap<String, &'a Node<Field>>,
) {
    for selection in &set.selections {
        match selection {
            Selection::Field(field) => {
                map.insert(field.name.to_string(), field);
            }
            Selection::FragmentSpread(f) => {
                if let Some(fragment) = f.fragment_def(document) {
                    map_fields_by_name_impl(document, &fragment.selection_set, map);
                }
            }
            Selection::InlineFragment(fragment) => {
                map_fields_by_name_impl(document, &fragment.selection_set, map);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use apollo_compiler::ExecutableDocument;
    use apollo_compiler::Schema;
    use apollo_compiler::executable::FieldSet;
    use apollo_compiler::executable::SelectionSet;
    use apollo_compiler::name;
    use apollo_compiler::validation::Valid;
    use pretty_assertions::assert_eq;

    use crate::assert_snapshot;

    fn selection_set(schema: &Valid<Schema>, s: &str) -> (ExecutableDocument, SelectionSet) {
        let document = ExecutableDocument::parse_and_validate(schema, s, "./").unwrap();
        let selection_set = document
            .operations
            .anonymous
            .as_ref()
            .unwrap()
            .selection_set
            .fields()
            .next()
            .unwrap()
            .selection_set
            .clone();
        (document.into_inner(), selection_set)
    }

    #[test]
    fn test() {
        let json = super::JSONSelection::parse(
            r###"
        $.result {
          a
          b: c
          d: e.f
          g
          h: 'i-j'
          k: { l m: n }
        }
        "###,
        )
        .unwrap();

        let schema = Schema::parse_and_validate(
            r###"
            type Query {
                t: T
            }

            type T {
                a: String
                b: String
                d: String
                g: String
                h: String
                k: K
            }

            type K {
              l: String
              m: String
            }
            "###,
            "./",
        )
        .unwrap();

        let (document, selection_set) = selection_set(
            &schema,
            "{ t { z: a, y: b, x: d, w: h v: k { u: l t: m } } }",
        );

        let transformed = json.apply_selection_set(&document, &selection_set, None);
        assert_eq!(
            transformed.to_string(),
            r###"$.result {
  a
  b: c
  d: e.f
  h: "i-j"
  k: {
    l
    m: n
  }
}"###
        );
    }

    #[test]
    fn test_star() {
        let json_selection = super::JSONSelection::parse(
            r###"
        $.result {
          a
          b_alias: b
          c {
            d
            e_alias: e
            h: "h"
            i: "i"
            group: {
              j
              k
            }
          }
          path_to_f: c.f
        }
        "###,
        )
        .unwrap();

        let schema = Schema::parse_and_validate(
            r###"
            type Query {
                t: T
            }

            type T {
                a: String
                b_alias: String
                c: C
                path_to_f: String
            }

            type C {
                d: String
                e_alias: String
                h: String
                i: String
                group: Group
            }

            type Group {
                j: String
                k: String
            }
            "###,
            "./",
        )
        .unwrap();

        let (document, selection_set) = selection_set(
            &schema,
            "{ t { a b_alias c { e: e_alias h group { j } } path_to_f } }",
        );

        let transformed = json_selection.apply_selection_set(&document, &selection_set, None);
        assert_eq!(
            transformed.to_string(),
            r###"$.result {
  a
  b_alias: b
  c {
    e_alias: e
    h: "h"
    group: {
      j
    }
  }
  path_to_f: c.f
}"###
        );

        let data = serde_json_bytes::json!({
            "result": {
                "a": "a",
                "b": "b",
                "c": {
                  "d": "d",
                  "e": "e",
                  "f": "f",
                  "g": "g",
                  "h": "h",
                  "i": "i",
                  "j": "j",
                  "k": "k",
                },
            }
        });
        let result = transformed.apply_to(&data);
        assert_eq!(
            result,
            (
                Some(serde_json_bytes::json!(
                {
                    "a": "a",
                    "b_alias": "b",
                    "c": {
                        "e_alias": "e",
                        "h": "h",
                        "group": {
                          "j": "j"
                        },
                    },
                    "path_to_f": "f",
                })),
                vec![]
            )
        );
    }

    #[test]
    fn test_depth() {
        let json = super::JSONSelection::parse(
            r###"
        $.result {
          a {
            b {
              renamed: c
            }
          }
        }
        "###,
        )
        .unwrap();

        let schema = Schema::parse_and_validate(
            r###"
            type Query {
                t: T
            }

            type T {
              a: A
            }

            type A {
              b: B
            }

            type B {
              renamed: String
            }
            "###,
            "./",
        )
        .unwrap();

        let (document, selection_set) = selection_set(&schema, "{ t { a { b { renamed } } } }");

        let transformed = json.apply_selection_set(&document, &selection_set, None);
        assert_eq!(
            transformed.to_string(),
            r###"$.result {
  a {
    b {
      renamed: c
    }
  }
}"###
        );

        let data = serde_json_bytes::json!({
            "result": {
              "a": {
                "b": {
                  "c": "c",
                }
              }
            }
          }
        );
        let result = transformed.apply_to(&data);
        assert_eq!(
            result,
            (
                Some(serde_json_bytes::json!({"a": { "b": { "renamed": "c" } } } )),
                vec![]
            )
        );
    }

    #[test]
    fn test_typename() {
        let json = super::JSONSelection::parse(
            r###"
            $.result {
              id
              author: {
                id: authorId
              }
            }
            "###,
        )
        .unwrap();

        let schema = Schema::parse_and_validate(
            r###"
        type Query {
            t: T
        }

        type T {
            id: ID
            author: A
        }

        type A {
            id: ID
        }
        "###,
            "./",
        )
        .unwrap();

        let (document, selection_set) =
            selection_set(&schema, "{ t { id __typename author { __typename id } } }");

        let transformed = json.apply_selection_set(&document, &selection_set, None);
        assert_eq!(
            transformed.to_string(),
            r###"$.result {
  __typename: $->echo("T")
  id
  author: {
    __typename: $->echo("A")
    id: authorId
  }
}"###
        );
    }

    #[test]
    fn test_fragments() {
        let json = super::JSONSelection::parse(
            r###"
            reviews: result {
                id
                product: { upc: product_upc }
                author: { id: author_id }
            }
            "###,
        )
        .unwrap();

        let schema = Schema::parse_and_validate(
            r###"
        type Query {
            _entities(representations: [_Any!]!): [_Entity]
        }

        scalar _Any

        union _Entity = Product

        type Product {
            upc: String
            reviews: [Review]
        }

        type Review {
            id: ID
            product: Product
            author: User
        }

        type User {
            id: ID
        }
        "###,
            "./",
        )
        .unwrap();

        let (document, selection_set) = selection_set(
            &schema,
            "query ($representations: [_Any!]!) {
                _entities(representations: $representations) {
                    ..._generated_onProduct1_0
                }
            }
            fragment _generated_onProduct1_0 on Product {
                reviews {
                    id
                    product {
                        __typename
                        upc
                    }
                    author {
                        __typename
                        id
                    }
                }
            }",
        );

        let transformed = json.apply_selection_set(&document, &selection_set, None);
        assert_eq!(
            transformed.to_string(),
            r###"reviews: result {
  id
  product: {
    __typename: $->echo("Product")
    upc: product_upc
  }
  author: {
    __typename: $->echo("User")
    id: author_id
  }
}"###
        );
    }

    #[test]
    fn test_ensuring_key_fields() {
        let json = super::JSONSelection::parse(
            r###"
            id
            store { id }
            name
            price
            "###,
        )
        .unwrap();

        let schema = Schema::parse_and_validate(
            r###"
        type Query {
            product: Product
        }

        type Product {
            id: ID!
            store: Store!
            name: String!
            price: String
        }

        type Store {
          id: ID!
        }
        "###,
            "./",
        )
        .unwrap();

        let (document, selection_set) = selection_set(&schema, "{ product { name price } }");

        let keys =
            FieldSet::parse_and_validate(&schema, name!(Product), "id store { id }", "").unwrap();

        let transformed = json.apply_selection_set(&document, &selection_set, Some(&keys));
        assert_snapshot!(transformed.to_string(), @r"
        id
        store {
          id
        }
        name
        price
        ");
    }
}
