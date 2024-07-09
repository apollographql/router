//! Functions for applying a [`SelectionSet`] to a [`JSONSelection`]. This creates a new
//! `JSONSelection` mapping to the fields on the selection set, and excluding any parts of the
//! original `JSONSelection` that are not needed by the selection set.
use apollo_compiler::executable::Selection;
use apollo_compiler::executable::SelectionSet;

use crate::sources::connect::json_selection::Alias;
use crate::sources::connect::json_selection::NamedSelection;
use crate::sources::connect::JSONSelection;
use crate::sources::connect::PathSelection;
use crate::sources::connect::SubSelection;

/// Find all fields in a selection set with a given name.
fn find_fields_in_selection_set<'a>(set: &'a SelectionSet, name: &str) -> Vec<&'a Selection> {
    let mut fields = vec![];
    for selection in &set.selections {
        match selection {
            Selection::Field(field) => {
                // TODO: handle include/skip directives, but will affect cache-ability
                if field.name == name {
                    fields.push(selection);
                }
            }
            Selection::FragmentSpread(_) => {
                // TODO: are these always expanded to inline fragments?
                todo!("Handle fragment spread?")
            }
            Selection::InlineFragment(fragment) => {
                // TODO: take directives like skip or include into account?
                // TODO: take type condition into account? or is this out of scope for preview?
                fields.extend(find_fields_in_selection_set(&fragment.selection_set, name));
            }
        }
    }
    fields
}

// TODO: are there any ways this can fail that need to be exposed?
#[derive(Debug, Eq, PartialEq, Clone)]
pub struct ApplySelectionSetError;

impl JSONSelection {
    pub fn apply_selection_set(
        &self,
        selection_set: &SelectionSet,
    ) -> Result<Self, ApplySelectionSetError> {
        match self {
            Self::Named(sub) => Ok(Self::Named(sub.apply_selection_set(selection_set)?)),
            Self::Path(path) => Ok(Self::Path(path.apply_selection_set(selection_set)?)),
        }
    }
}

impl SubSelection {
    fn apply_selection_set(
        &self,
        selection_set: &SelectionSet,
    ) -> Result<Self, ApplySelectionSetError> {
        let mut new_selections = Vec::new();
        for selection in &self.selections {
            match selection {
                NamedSelection::Field(alias, name, sub) => {
                    let key = alias
                        .as_ref()
                        .map(|a| a.name.to_string())
                        .unwrap_or(name.to_string());
                    for field in find_fields_in_selection_set(selection_set, &key) {
                        new_selections.push(NamedSelection::Field(
                            Some(Alias {
                                name: field
                                    .as_field()
                                    .map(|s| s.response_key())
                                    .unwrap()
                                    .to_string(),
                            }),
                            name.clone(),
                            sub.as_ref()
                                .map(|sub| {
                                    sub.apply_selection_set(
                                        &field.as_field().unwrap().selection_set,
                                    )
                                })
                                .transpose()?,
                        ));
                    }
                }
                NamedSelection::Quoted(alias, name, sub) => {
                    let key = alias.name.to_string();
                    for field in find_fields_in_selection_set(selection_set, &key) {
                        new_selections.push(NamedSelection::Quoted(
                            Alias {
                                name: field
                                    .as_field()
                                    .map(|s| s.response_key())
                                    .unwrap()
                                    .to_string(),
                            },
                            name.clone(),
                            sub.as_ref()
                                .map(|sub| {
                                    sub.apply_selection_set(
                                        &field.as_field().unwrap().selection_set,
                                    )
                                })
                                .transpose()?,
                        ));
                    }
                }
                NamedSelection::Path(alias, sub) => {
                    let key = alias.name.to_string();
                    for field in find_fields_in_selection_set(selection_set, &key) {
                        new_selections.push(NamedSelection::Path(
                            Alias {
                                name: field
                                    .as_field()
                                    .map(|s| s.response_key())
                                    .unwrap()
                                    .to_string(),
                            },
                            sub.apply_selection_set(&field.as_field().unwrap().selection_set)?,
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
                            sub.apply_selection_set(&field.as_field().unwrap().selection_set)?,
                        ));
                    }
                }
            }
        }
        // TODO: handle star selections
        let new_star = self.star.as_ref().cloned();

        Ok(Self {
            selections: new_selections,
            star: new_star,
        })
    }
}

impl PathSelection {
    fn apply_selection_set(
        &self,
        selection_set: &SelectionSet,
    ) -> Result<Self, ApplySelectionSetError> {
        match self {
            Self::Var(str, path) => Ok(Self::Var(
                str.clone(),
                Box::new(path.apply_selection_set(selection_set)?),
            )),
            Self::Key(key, path) => Ok(Self::Key(
                key.clone(),
                Box::new(path.apply_selection_set(selection_set)?),
            )),
            Self::Selection(sub) => Ok(Self::Selection(sub.apply_selection_set(selection_set)?)),
            Self::Empty => Ok(Self::Empty),
        }
    }
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;

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
        println!("{json}");

        let schema = apollo_compiler::Schema::parse_and_validate(
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

        let op = apollo_compiler::ExecutableDocument::parse_and_validate(
            &schema,
            "{ t { z: a, y: b, x: d, w: h v: k { u: l t: m } } }",
            "./",
        )
        .unwrap();
        let graphql = &op
            .anonymous_operation
            .as_ref()
            .unwrap()
            .selection_set
            .fields()
            .next()
            .unwrap()
            .selection_set;

        let transformed = json.apply_selection_set(graphql).unwrap();
        println!("{transformed}");
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
}
