use apollo_compiler::hir;
use serde::Deserialize;
use serde::Serialize;
use serde_json_bytes::ByteString;

use super::Fragments;
use crate::json_ext::Object;
use crate::json_ext::PathElement;
use crate::spec::FieldType;
use crate::spec::Schema;
use crate::spec::SpecError;
use crate::spec::TYPENAME;

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub(crate) enum Selection {
    Field {
        name: ByteString,
        alias: Option<ByteString>,
        selection_set: Option<Vec<Selection>>,
        field_type: FieldType,
        skip: Skip,
        include: Include,
    },
    InlineFragment {
        // Optional in specs but we fill it with the current type if not specified
        type_condition: String,
        skip: Skip,
        include: Include,
        known_type: Option<String>,
        selection_set: Vec<Selection>,
    },
    FragmentSpread {
        name: String,
        known_type: Option<String>,
        skip: Skip,
        include: Include,
    },
}

impl Selection {
    pub(crate) fn from_hir(
        selection: &hir::Selection,
        current_type: &FieldType,
        schema: &Schema,
        mut count: usize,
    ) -> Result<Option<Self>, SpecError> {
        // The RECURSION_LIMIT is chosen to be:
        //   < # expected to cause stack overflow &&
        //   > # expected in a legitimate query
        const RECURSION_LIMIT: usize = 512;
        if count > RECURSION_LIMIT {
            tracing::error!("selection processing recursion limit({RECURSION_LIMIT}) exceeded");
            return Err(SpecError::RecursionLimitExceeded);
        }
        count += 1;
        Ok(match selection {
            // Spec: https://spec.graphql.org/draft/#Field
            hir::Selection::Field(field) => {
                let skip = field
                    .directives()
                    .iter()
                    .find_map(parse_skip_hir)
                    .unwrap_or(Skip::No);
                if skip.statically_skipped() {
                    return Ok(None);
                }
                let include = field
                    .directives()
                    .iter()
                    .find_map(parse_include_hir)
                    .unwrap_or(Include::Yes);
                if include.statically_skipped() {
                    return Ok(None);
                }
                let field_type = match field.name() {
                    TYPENAME => FieldType::String,
                    "__schema" => FieldType::Introspection("__Schema".to_string()),
                    "__type" => FieldType::Introspection("__Type".to_string()),
                    field_name => {
                        let name = current_type
                            .inner_type_name()
                            .ok_or_else(|| SpecError::InvalidType(current_type.to_string()))?;
                        //looking into object types
                        schema
                            .object_types
                            .get(name)
                            .and_then(|ty| ty.field(field_name))
                            // otherwise, it might be an interface
                            .or_else(|| {
                                schema
                                    .interfaces
                                    .get(name)
                                    .and_then(|ty| ty.field(field_name))
                            })
                            .ok_or_else(|| {
                                SpecError::InvalidField(
                                    field_name.to_owned(),
                                    current_type.to_string(),
                                )
                            })?
                            .clone()
                    }
                };

                let alias = field.alias().map(|x| x.0.as_str().into());

                let selection_set = if field_type.is_builtin_scalar() {
                    None
                } else {
                    let selection = field.selection_set().selection();
                    if selection.is_empty() {
                        None
                    } else {
                        Some(
                            selection
                                .iter()
                                .filter_map(|selection| {
                                    Selection::from_hir(selection, &field_type, schema, count)
                                        .transpose()
                                })
                                .collect::<Result<_, _>>()?,
                        )
                    }
                };

                Some(Self::Field {
                    alias,
                    name: field.name().into(),
                    selection_set,
                    field_type,
                    skip,
                    include,
                })
            }
            // Spec: https://spec.graphql.org/draft/#InlineFragment
            hir::Selection::InlineFragment(inline_fragment) => {
                let skip = inline_fragment
                    .directives()
                    .iter()
                    .find_map(parse_skip_hir)
                    .unwrap_or(Skip::No);
                if skip.statically_skipped() {
                    return Ok(None);
                }
                let include = inline_fragment
                    .directives()
                    .iter()
                    .find_map(parse_include_hir)
                    .unwrap_or(Include::Yes);
                if include.statically_skipped() {
                    return Ok(None);
                }

                let type_condition = inline_fragment
                    .type_condition()
                    .map(|s| s.to_owned())
                    // if we can't get a type name from the current type, that means we're applying
                    // a fragment onto a scalar
                    .or_else(|| current_type.inner_type_name().map(|s| s.to_string()))
                    .ok_or_else(|| SpecError::InvalidType(current_type.to_string()))?;

                let fragment_type = FieldType::Named(type_condition.clone());

                let selection_set = inline_fragment
                    .selection_set()
                    .selection()
                    .iter()
                    .filter_map(|selection| {
                        Selection::from_hir(selection, &fragment_type, schema, count).transpose()
                    })
                    .collect::<Result<_, _>>()?;

                let known_type = current_type.inner_type_name().map(|s| s.to_string());
                Some(Self::InlineFragment {
                    type_condition,
                    selection_set,
                    skip,
                    include,
                    known_type,
                })
            }
            // Spec: https://spec.graphql.org/draft/#FragmentSpread
            hir::Selection::FragmentSpread(fragment_spread) => {
                let skip = fragment_spread
                    .directives()
                    .iter()
                    .find_map(parse_skip_hir)
                    .unwrap_or(Skip::No);
                if skip.statically_skipped() {
                    return Ok(None);
                }
                let include = fragment_spread
                    .directives()
                    .iter()
                    .find_map(parse_include_hir)
                    .unwrap_or(Include::Yes);
                if include.statically_skipped() {
                    return Ok(None);
                }
                Some(Self::FragmentSpread {
                    name: fragment_spread.name().to_owned(),
                    known_type: current_type.inner_type_name().map(|s| s.to_string()),
                    skip,
                    include,
                })
            }
        })
    }

    pub(crate) fn is_typename_field(&self) -> bool {
        matches!(self, Selection::Field {name, ..} if name.as_str() == TYPENAME)
    }

    pub(crate) fn contains_error_path(&self, path: &[PathElement], fragments: &Fragments) -> bool {
        match (path.get(0), self) {
            (None, _) => true,
            (
                Some(PathElement::Key(key)),
                Selection::Field {
                    name,
                    alias,
                    selection_set,
                    ..
                },
            ) => {
                if alias.as_ref().unwrap_or(name).as_str() == key.as_str() {
                    match selection_set {
                        // if we don't select after that field, the path should stop there
                        None => path.len() == 1,
                        Some(set) => set
                            .iter()
                            .any(|selection| selection.contains_error_path(&path[1..], fragments)),
                    }
                } else {
                    false
                }
            }
            (
                Some(PathElement::Fragment(fragment)),
                Selection::InlineFragment {
                    type_condition,
                    selection_set,
                    ..
                },
            ) => {
                if fragment.as_str() == type_condition.as_str() {
                    selection_set
                        .iter()
                        .any(|selection| selection.contains_error_path(&path[1..], fragments))
                } else {
                    false
                }
            }
            (Some(PathElement::Fragment(fragment)), Self::FragmentSpread { name, .. }) => {
                if let Some(f) = fragments.get(name) {
                    if fragment.as_str() == f.type_condition.as_str() {
                        f.selection_set
                            .iter()
                            .any(|selection| selection.contains_error_path(&path[1..], fragments))
                    } else {
                        false
                    }
                } else {
                    false
                }
            }
            (_, Self::FragmentSpread { name, .. }) => {
                if let Some(f) = fragments.get(name) {
                    f.selection_set
                        .iter()
                        .any(|selection| selection.contains_error_path(path, fragments))
                } else {
                    false
                }
            }
            (Some(PathElement::Index(_)), _) | (Some(PathElement::Flatten), _) => {
                self.contains_error_path(&path[1..], fragments)
            }
            (Some(PathElement::Key(_)), Selection::InlineFragment { selection_set, .. }) => {
                selection_set
                    .iter()
                    .any(|selection| selection.contains_error_path(path, fragments))
            }
            (Some(PathElement::Fragment(_)), Selection::Field { .. }) => false,
        }
    }
}

pub(crate) fn parse_skip_hir(directive: &hir::Directive) -> Option<Skip> {
    if directive.name() != "skip" {
        return None;
    }
    match directive.argument_by_name("if")? {
        hir::Value::Boolean(true) => Some(Skip::Yes),
        hir::Value::Boolean(false) => Some(Skip::No),
        hir::Value::Variable(variable) => Some(Skip::Variable(variable.name().to_owned())),
        _ => None,
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub(crate) enum Skip {
    Yes,
    No,
    Variable(String),
}

impl Skip {
    pub(crate) fn should_skip(&self, variables: &Object) -> Option<bool> {
        match self {
            Skip::Yes => Some(true),
            Skip::No => Some(false),
            Skip::Variable(variable_name) => variables
                .get(variable_name.as_str())
                .and_then(|v| v.as_bool()),
        }
    }
    pub(crate) fn statically_skipped(&self) -> bool {
        matches!(self, Skip::Yes)
    }
}

pub(crate) fn parse_include_hir(directive: &hir::Directive) -> Option<Include> {
    if directive.name() != "include" {
        return None;
    }
    match directive.argument_by_name("if")? {
        hir::Value::Boolean(true) => Some(Include::Yes),
        hir::Value::Boolean(false) => Some(Include::No),
        hir::Value::Variable(variable) => Some(Include::Variable(variable.name().to_owned())),
        _ => None,
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub(crate) enum Include {
    Yes,
    No,
    Variable(String),
}

impl Include {
    pub(crate) fn should_include(&self, variables: &Object) -> Option<bool> {
        match self {
            Include::Yes => Some(true),
            Include::No => Some(false),
            Include::Variable(variable_name) => variables
                .get(variable_name.as_str())
                .and_then(|v| v.as_bool()),
        }
    }
    pub(crate) fn statically_skipped(&self) -> bool {
        matches!(self, Include::No)
    }
}
