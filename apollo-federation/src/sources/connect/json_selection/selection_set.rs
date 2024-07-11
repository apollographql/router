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

use apollo_compiler::executable::Field;
use apollo_compiler::executable::Selection;
use apollo_compiler::executable::SelectionSet;
use apollo_compiler::Node;
use indexmap::IndexMap;
use multimap::MultiMap;

use crate::sources::connect::json_selection::Alias;
use crate::sources::connect::json_selection::NamedSelection;
use crate::sources::connect::JSONSelection;
use crate::sources::connect::Key;
use crate::sources::connect::PathSelection;
use crate::sources::connect::SubSelection;

/// Create a new version of `Self` by applying a selection set.
pub trait ApplySelectionSet {
    /// Apply a selection set to create a new version of `Self`
    fn apply_selection_set(&self, selection_set: &SelectionSet) -> Self;
}

impl ApplySelectionSet for JSONSelection {
    fn apply_selection_set(&self, selection_set: &SelectionSet) -> Self {
        match self {
            Self::Named(sub) => Self::Named(sub.apply_selection_set(selection_set)),
            Self::Path(path) => Self::Path(path.apply_selection_set(selection_set)),
        }
    }
}

impl ApplySelectionSet for SubSelection {
    fn apply_selection_set(&self, selection_set: &SelectionSet) -> Self {
        let mut new_selections = Vec::new();
        let mut dropped_fields = IndexMap::new();
        let mut referenced_fields = HashSet::new();
        let field_map = map_fields_by_name(selection_set);
        for selection in &self.selections {
            match selection {
                NamedSelection::Field(alias, name, sub) => {
                    let key = alias
                        .as_ref()
                        .map(|a| a.name.to_string())
                        .unwrap_or(name.to_string());
                    if let Some(fields) = field_map.get_vec(&key) {
                        if self.star.is_some() {
                            referenced_fields.insert(key.clone());
                        }
                        for field in fields {
                            let field_response_key = field.response_key().to_string();
                            let alias = if field_response_key == *name {
                                None
                            } else {
                                Some(Alias {
                                    name: field_response_key.clone(),
                                })
                            };
                            new_selections.push(NamedSelection::Field(
                                alias,
                                name.clone(),
                                sub.as_ref()
                                    .map(|sub| sub.apply_selection_set(&field.selection_set)),
                            ));
                        }
                    } else if self.star.is_some() {
                        dropped_fields.insert(key.clone(), ());
                    }
                }
                NamedSelection::Quoted(alias, name, sub) => {
                    let key = alias.name.to_string();
                    if let Some(fields) = field_map.get_vec(&key) {
                        if self.star.is_some() {
                            referenced_fields.insert(key.clone());
                        }
                        for field in fields {
                            new_selections.push(NamedSelection::Quoted(
                                Alias {
                                    name: field.response_key().to_string(),
                                },
                                name.clone(),
                                sub.as_ref()
                                    .map(|sub| sub.apply_selection_set(&field.selection_set)),
                            ));
                        }
                    } else if self.star.is_some() {
                        dropped_fields.insert(key.clone(), ());
                    }
                }
                NamedSelection::Path(alias, path_selection) => {
                    let key = alias.name.to_string();
                    if let Some(fields) = field_map.get_vec(&key) {
                        if self.star.is_some() {
                            if let Some(name) = key_name(path_selection) {
                                referenced_fields.insert(name);
                            }
                        }
                        for field in fields {
                            new_selections.push(NamedSelection::Path(
                                Alias {
                                    name: field.response_key().to_string(),
                                },
                                path_selection.apply_selection_set(&field.selection_set),
                            ));
                        }
                    } else if self.star.is_some() {
                        if let Some(name) = key_name(path_selection) {
                            dropped_fields.insert(name, ());
                        }
                    }
                }
                NamedSelection::Group(alias, sub) => {
                    let key = alias.name.to_string();
                    if let Some(fields) = field_map.get_vec(&key) {
                        for field in fields {
                            new_selections.push(NamedSelection::Group(
                                Alias {
                                    name: field.response_key().to_string(),
                                },
                                sub.apply_selection_set(&field.selection_set),
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
                let mut unused = "__unused__".to_owned();
                unused.push_str(dropped.as_str());
                new_selections.push(NamedSelection::Field(
                    Some(Alias { name: unused }),
                    dropped.clone(),
                    None,
                ));
            }
        }

        Self {
            selections: new_selections,
            star: new_star,
        }
    }
}

impl ApplySelectionSet for PathSelection {
    fn apply_selection_set(&self, selection_set: &SelectionSet) -> Self {
        match self {
            Self::Var(str, path) => Self::Var(
                str.clone(),
                Box::new(path.apply_selection_set(selection_set)),
            ),
            Self::Key(key, path) => Self::Key(
                key.clone(),
                Box::new(path.apply_selection_set(selection_set)),
            ),
            Self::Selection(sub) => Self::Selection(sub.apply_selection_set(selection_set)),
            Self::Empty => Self::Empty,
        }
    }
}

#[inline]
fn key_name(path_selection: &PathSelection) -> Option<String> {
    match path_selection {
        PathSelection::Key(Key::Field(name), _) => Some(name.clone()),
        PathSelection::Key(Key::Quoted(name), _) => Some(name.clone()),
        _ => None,
    }
}

fn map_fields_by_name(set: &SelectionSet) -> MultiMap<String, &Node<Field>> {
    let mut map = MultiMap::new();
    map_fields_by_name_impl(set, &mut map);
    map
}

fn map_fields_by_name_impl<'a>(set: &'a SelectionSet, map: &mut MultiMap<String, &'a Node<Field>>) {
    for selection in &set.selections {
        match selection {
            Selection::Field(field) => {
                map.insert(field.name.to_string(), field);
            }
            Selection::FragmentSpread(_) => {
                // Ignore - these are always expanded into inline fragments
            }
            Selection::InlineFragment(fragment) => {
                map_fields_by_name_impl(&fragment.selection_set, map);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use apollo_compiler::executable::SelectionSet;
    use apollo_compiler::validation::Valid;
    use apollo_compiler::Schema;
    use pretty_assertions::assert_eq;

    use crate::sources::connect::json_selection::selection_set::ApplySelectionSet;
    use crate::sources::connect::ApplyTo;

    fn selection_set(schema: &Valid<Schema>, s: &str) -> SelectionSet {
        apollo_compiler::ExecutableDocument::parse_and_validate(schema, s, "./")
            .unwrap()
            .anonymous_operation
            .as_ref()
            .unwrap()
            .selection_set
            .fields()
            .next()
            .unwrap()
            .selection_set
            .clone()
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

        let selection_set = selection_set(
            &schema,
            "{ t { z: a, y: b, x: d, w: h v: k { u: l t: m } } }",
        );

        let transformed = json.apply_selection_set(&selection_set);
        assert_eq!(
            transformed.to_string(),
            r###".result {
  z: a
  y: c
  x: .e.f
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

        let selection_set = selection_set(
            &schema,
            "{ t { a b_alias c { e: e_alias h group { j } } path_to_f } }",
        );

        let transformed = json_selection.apply_selection_set(&selection_set);
        assert_eq!(
            transformed.to_string(),
            r###".result {
  a
  b_alias: b
  c {
    e
    h: "h"
    group: {
      j
    }
    __unused__d: d
    __unused__i: i
    rest: *
  }
  path_to_f: .c.f
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

        let selection_set = selection_set(&schema, "{ t { a { b { renamed } } } }");

        let transformed = json.apply_selection_set(&selection_set);
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
}
