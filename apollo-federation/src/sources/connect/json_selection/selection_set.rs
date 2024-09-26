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

use std::collections::HashSet;

use apollo_compiler::collections::IndexMap;
use apollo_compiler::executable::Field;
use apollo_compiler::executable::Selection;
use apollo_compiler::executable::SelectionSet;
use apollo_compiler::ExecutableDocument;
use apollo_compiler::Node;
use multimap::MultiMap;

use super::known_var::KnownVariable;
use super::lit_expr::LitExpr;
use super::location::Ranged;
use super::location::WithRange;
use super::parser::MethodArgs;
use super::parser::PathList;
use crate::sources::connect::json_selection::Alias;
use crate::sources::connect::json_selection::NamedSelection;
use crate::sources::connect::JSONSelection;
use crate::sources::connect::Key;
use crate::sources::connect::PathSelection;
use crate::sources::connect::SubSelection;

impl JSONSelection {
    /// Apply a selection set to create a new [`JSONSelection`]
    pub fn apply_selection_set(
        &self,
        document: &ExecutableDocument,
        selection_set: &SelectionSet,
    ) -> Self {
        match self {
            Self::Named(sub) => Self::Named(sub.apply_selection_set(document, selection_set)),
            Self::Path(path) => Self::Path(path.apply_selection_set(document, selection_set)),
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
        let mut dropped_fields = IndexMap::default();
        let mut referenced_fields = HashSet::new();
        let field_map = map_fields_by_name(document, selection_set);

        // When the operation contains __typename, it might be used to complete
        // an entity reference (e.g. `__typename id`) for a subsequent fetch.
        // We don't have a way to inject static values into the mapping, so for
        // now we'll hardcode a special "variable" prefix that returns values
        // based on the key. ({ "Product": "Product" }).
        //
        // TODO: when we support abstract types, we'll want to first check if
        // the user defined a __typename mapping.
        if field_map.contains_key("__typename") {
            new_selections.push(NamedSelection::Path(
                Alias::new("__typename"),
                PathSelection {
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
            ));
        }

        for selection in &self.selections {
            match selection {
                NamedSelection::Field(alias, name, sub) => {
                    let key = alias
                        .as_ref()
                        .map(|a| a.name.as_str())
                        .unwrap_or(name.as_str());
                    if let Some(fields) = field_map.get_vec(key) {
                        if self.star.is_some() {
                            referenced_fields.insert(key);
                        }
                        for field in fields {
                            let field_response_key = field.response_key().as_str();
                            new_selections.push(NamedSelection::Field(
                                if field_response_key == name.as_str() {
                                    None
                                } else {
                                    Some(Alias::new(field_response_key))
                                },
                                name.clone(),
                                sub.as_ref().map(|sub| {
                                    sub.apply_selection_set(document, &field.selection_set)
                                }),
                            ));
                        }
                    } else if self.star.is_some() {
                        dropped_fields.insert(key, ());
                    }
                }
                NamedSelection::Path(alias, path_selection) => {
                    let key = alias.name.as_str();
                    if let Some(fields) = field_map.get_vec(key) {
                        if self.star.is_some() {
                            if let Some(name) = key_name(path_selection) {
                                referenced_fields.insert(name);
                            }
                        }
                        for field in fields {
                            new_selections.push(NamedSelection::Path(
                                Alias::new(field.response_key().as_str()),
                                path_selection.apply_selection_set(document, &field.selection_set),
                            ));
                        }
                    } else if self.star.is_some() {
                        if let Some(name) = key_name(path_selection) {
                            dropped_fields.insert(name, ());
                        }
                    }
                }
                NamedSelection::Group(alias, sub) => {
                    let key = alias.name.as_str();
                    if let Some(fields) = field_map.get_vec(key) {
                        for field in fields {
                            new_selections.push(NamedSelection::Group(
                                Alias::new(field.response_key().as_str()),
                                sub.apply_selection_set(document, &field.selection_set),
                            ));
                        }
                    }
                }
            }
        }
        let new_star = self.star.as_ref().cloned();
        if new_star.is_some() {
            // Alias fields that were dropped from the original selection to prevent them from
            // being picked up by the star.
            dropped_fields.retain(|key, _| !referenced_fields.contains(key));
            for (dropped, _) in dropped_fields {
                let name = format!("__unused__{dropped}");
                new_selections.push(NamedSelection::Field(
                    Some(Alias::new(name.as_str())),
                    WithRange::new(Key::field(dropped), None),
                    None,
                ));
            }
        }

        Self {
            selections: new_selections,
            star: new_star,
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
    pub fn apply_selection_set(
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
            Self::Selection(sub) => {
                Self::Selection(sub.apply_selection_set(document, selection_set))
            }
            Self::Empty => Self::Empty,
        }
    }
}

#[inline]
fn key_name(path_selection: &PathSelection) -> Option<&str> {
    match path_selection.path.as_ref() {
        PathList::Key(key, _) => Some(key.as_str()),
        _ => None,
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
                map_fields_by_name_impl(
                    &document,
                    &f.fragment_def(document)
                        .expect("fragment exists")
                        .selection_set,
                    map,
                );
                // Ignore - these are always expanded into inline fragments
            }
            Selection::InlineFragment(fragment) => {
                map_fields_by_name_impl(&document, &fragment.selection_set, map);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use apollo_compiler::executable::SelectionSet;
    use apollo_compiler::validation::Valid;
    use apollo_compiler::ExecutableDocument;
    use apollo_compiler::Schema;
    use pretty_assertions::assert_eq;

    fn selection_set(schema: &Valid<Schema>, s: &str) -> (ExecutableDocument, SelectionSet) {
        let document =
            apollo_compiler::ExecutableDocument::parse_and_validate(schema, s, "./").unwrap();
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
        .result {
          a
          b: c
          d: .e.f
          g
          h: 'i-j'
          k: { l m: n }
        }
        "###,
        )
        .unwrap()
        .1;

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

        let transformed = json.apply_selection_set(&document, &selection_set);
        assert_eq!(
            transformed.to_string(),
            r###".result {
  z: a
  y: c
  x: e.f
  w: "i-j"
  v: {
    u: l
    t: n
  }
}"###
        );
    }

    #[test]
    fn test_star() {
        let json_selection = super::JSONSelection::parse(
            r###"
        .result {
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
            rest: *
          }
          path_to_f: .c.f
          rest: *
        }
        "###,
        )
        .unwrap()
        .1;

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

        let transformed = json_selection.apply_selection_set(&document, &selection_set);
        assert_eq!(
            transformed.to_string(),
            r###".result {
  a
  b_alias: b
  c {
    e
    "h"
    group: {
      j
    }
    __unused__d: d
    __unused__i: i
    rest: *
  }
  path_to_f: c.f
  rest: *
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
                        "e": "e",
                        "h": "h",
                        "group": {
                          "j": "j"
                        },
                        "__unused__d": "d",
                        "__unused__i": "i",
                        "rest": {
                            "f": "f",
                            "g": "g",
                            "j": "j",
                            "k": "k",
                        }
                    },
                    "path_to_f": "f",
                    "rest": {}
                })),
                vec![]
            )
        );
    }

    #[test]
    fn test_depth() {
        let json = super::JSONSelection::parse(
            r###"
        .result {
          a {
            b {
              renamed: c
            }
          }
        }
        "###,
        )
        .unwrap()
        .1;

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

        let transformed = json.apply_selection_set(&document, &selection_set);
        assert_eq!(
            transformed.to_string(),
            r###".result {
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
            .result {
              id
              author: {
                id: authorId
              }
            }
            "###,
        )
        .unwrap()
        .1;

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

        let transformed = json.apply_selection_set(&document, &selection_set);
        assert_eq!(
            transformed.to_string(),
            r###".result {
  __typename: $->echo("T")
  id
  author: {
    __typename: $->echo("A")
    id: authorId
  }
}"###
        );
    }
}
