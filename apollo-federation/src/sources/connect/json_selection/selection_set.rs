//! Functions for applying a [`SelectionSet`] to a [`JSONSelection`]. This creates a new
//! `JSONSelection` mapping to the fields on the selection set, and excluding parts of the
//! original `JSONSelection` that are not needed by the selection set.

use std::collections::HashSet;

use apollo_compiler::executable::Selection;
use apollo_compiler::executable::SelectionSet;

use crate::sources::connect::json_selection::Alias;
use crate::sources::connect::json_selection::NamedSelection;
use crate::sources::connect::JSONSelection;
use crate::sources::connect::Key;
use crate::sources::connect::PathSelection;
use crate::sources::connect::SubSelection;

/// Find all fields in a selection set with a given name.
fn find_fields_in_selection_set<'a>(set: &'a SelectionSet, name: &str) -> Vec<&'a Selection> {
    let mut fields = vec![];
    for selection in &set.selections {
        match selection {
            Selection::Field(field) => {
                if field.name == name {
                    fields.push(selection);
                }
            }
            Selection::FragmentSpread(_) => {
                // Ignore - these are always expanded into inline fragments
            }
            Selection::InlineFragment(fragment) => {
                fields.extend(find_fields_in_selection_set(&fragment.selection_set, name));
            }
        }
    }
    fields
}

impl JSONSelection {
    pub fn apply_selection_set(&self, selection_set: &SelectionSet) -> Self {
        match self {
            Self::Named(sub) => Self::Named(sub.apply_selection_set(selection_set)),
            Self::Path(path) => Self::Path(path.apply_selection_set(selection_set)),
        }
    }
}

impl SubSelection {
    fn apply_selection_set(&self, selection_set: &SelectionSet) -> Self {
        let mut new_selections = Vec::new();
        let mut dropped_fields = HashSet::new();
        let mut referenced_fields = HashSet::new();
        for selection in &self.selections {
            match selection {
                NamedSelection::Field(alias, name, sub) => {
                    let key = alias
                        .as_ref()
                        .map(|a| a.name.to_string())
                        .unwrap_or(name.to_string());
                    let fields = find_fields_in_selection_set(selection_set, &key);
                    if self.star.is_some() {
                        if fields.is_empty() {
                            dropped_fields.insert(key.clone());
                        } else {
                            referenced_fields.insert(key.clone());
                        }
                    }
                    for field in fields {
                        let field_response_key = field
                            .as_field()
                            .map(|s| s.response_key())
                            .unwrap()
                            .to_string();
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
                            sub.as_ref().map(|sub| {
                                sub.apply_selection_set(&field.as_field().unwrap().selection_set)
                            }),
                        ));
                    }
                }
                NamedSelection::Quoted(alias, name, sub) => {
                    let key = alias.name.to_string();
                    let fields = find_fields_in_selection_set(selection_set, &key);
                    if self.star.is_some() {
                        if fields.is_empty() {
                            dropped_fields.insert(key.clone());
                        } else {
                            referenced_fields.insert(key.clone());
                        }
                    }
                    for field in fields {
                        new_selections.push(NamedSelection::Quoted(
                            Alias {
                                name: field
                                    .as_field()
                                    .map(|s| s.response_key())
                                    .unwrap()
                                    .to_string(),
                            },
                            name.clone(),
                            sub.as_ref().map(|sub| {
                                sub.apply_selection_set(&field.as_field().unwrap().selection_set)
                            }),
                        ));
                    }
                }
                NamedSelection::Path(alias, path_selection) => {
                    let key = alias.name.to_string();
                    let fields = find_fields_in_selection_set(selection_set, &key);
                    if self.star.is_some() {
                        if let PathSelection::Key(key, _) = path_selection {
                            if let Key::Field(name) | Key::Quoted(name) = key {
                                if fields.is_empty() {
                                    dropped_fields.insert(name.clone());
                                } else {
                                    referenced_fields.insert(name.clone());
                                }
                            }
                        }
                    }
                    for field in fields {
                        new_selections.push(NamedSelection::Path(
                            Alias {
                                name: field
                                    .as_field()
                                    .map(|s| s.response_key())
                                    .unwrap()
                                    .to_string(),
                            },
                            path_selection
                                .apply_selection_set(&field.as_field().unwrap().selection_set),
                        ));
                    }
                }
                NamedSelection::Group(alias, sub) => {
                    let key = alias.name.to_string();
                    for field in find_fields_in_selection_set(selection_set, &key) {
                        new_selections.push(NamedSelection::Group(
                            Alias {
                                name: field
                                    .as_field()
                                    .map(|s| s.response_key())
                                    .unwrap()
                                    .to_string(),
                            },
                            sub.apply_selection_set(&field.as_field().unwrap().selection_set),
                        ));
                    }
                }
            }
        }
        let new_star = self.star.as_ref().cloned();
        if new_star.is_some() {
            // Alias fields that were dropped from the original selection to prevent them from
            // being picked up by the star.
            dropped_fields.retain(|key| !referenced_fields.contains(key));
            for dropped in dropped_fields {
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

impl PathSelection {
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

#[cfg(test)]
mod tests {
    use apollo_compiler::executable::SelectionSet;
    use apollo_compiler::validation::Valid;
    use apollo_compiler::Schema;
    use pretty_assertions::assert_eq;

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
          b
          c
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
                b: String
                c: String
                d: String
            }
            "###,
            "./",
        )
        .unwrap();

        let selection_set = selection_set(&schema, "{ t { a b } }");

        let transformed = json_selection.apply_selection_set(&selection_set);
        assert_eq!(
            transformed.to_string(),
            r###".result {
  a
  b
  __unused__c: c
  rest: *
}"###
        );

        let data = serde_json_bytes::json!({
            "result": {
                "a": "a",
                "b": "b",
                "c": "c",
                "d": "d",
            }
        });
        let result = transformed.apply_to(&data);
        assert_eq!(
            result,
            (
                Some(
                    serde_json_bytes::json!({"a": "a", "b": "b", "__unused__c": "c", "rest": { "d": "d" }})
                ),
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
