use apollo_compiler::hir;
use serde::Deserialize;
use serde::Serialize;
use serde_json_bytes::ByteString;

use super::Fragments;
use crate::json_ext::Object;
use crate::json_ext::PathElement;
use crate::spec::query::subselections::DEFER_DIRECTIVE_NAME;
use crate::spec::query::DeferStats;
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
        include_skip: IncludeSkip,
    },
    InlineFragment {
        // Optional in specs but we fill it with the current type if not specified
        type_condition: String,
        include_skip: IncludeSkip,
        defer: Condition,
        defer_label: Option<String>,
        known_type: Option<String>,
        selection_set: Vec<Selection>,
    },
    FragmentSpread {
        name: String,
        known_type: Option<String>,
        include_skip: IncludeSkip,
        defer: Condition,
        defer_label: Option<String>,
    },
}

impl Selection {
    pub(crate) fn from_hir(
        selection: &hir::Selection,
        current_type: &FieldType,
        schema: &Schema,
        mut count: usize,
        defer_stats: &mut DeferStats,
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
                let include_skip = IncludeSkip::parse(field.directives());
                if include_skip.statically_skipped() {
                    return Ok(None);
                }
                let field_type = match field.name() {
                    TYPENAME => FieldType::new_named("String"),
                    "__schema" => FieldType::new_named("__Schema"),
                    "__type" => FieldType::new_named("__Type"),
                    field_name => {
                        let name = current_type
                            .inner_type_name()
                            .ok_or_else(|| SpecError::InvalidType(current_type.to_string()))?;
                        let definitions = &schema.type_system.definitions;
                        //looking into object types
                        definitions
                            .objects
                            .get(name)
                            .and_then(|ty| ty.fields().find(|f| f.name() == field_name))
                            // otherwise, it might be an interface
                            .or_else(|| {
                                definitions
                                    .interfaces
                                    .get(name)
                                    .and_then(|ty| ty.fields().find(|f| f.name() == field_name))
                            })
                            .ok_or_else(|| {
                                SpecError::InvalidField(
                                    field_name.to_owned(),
                                    current_type
                                        .inner_type_name()
                                        .map(ToString::to_string)
                                        .unwrap_or_else(|| current_type.to_string()),
                                )
                            })?
                            .ty()
                            .into()
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
                                    Selection::from_hir(
                                        selection,
                                        &field_type,
                                        schema,
                                        count,
                                        defer_stats,
                                    )
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
                    include_skip,
                })
            }
            // Spec: https://spec.graphql.org/draft/#InlineFragment
            hir::Selection::InlineFragment(inline_fragment) => {
                let include_skip = IncludeSkip::parse(inline_fragment.directives());
                if include_skip.statically_skipped() {
                    return Ok(None);
                }
                let (defer, defer_label) = parse_defer(inline_fragment.directives(), defer_stats);

                let type_condition = inline_fragment
                    .type_condition()
                    .map(|s| s.to_owned())
                    // if we can't get a type name from the current type, that means we're applying
                    // a fragment onto a scalar
                    .or_else(|| current_type.inner_type_name().map(|s| s.to_string()))
                    .ok_or_else(|| SpecError::InvalidType(current_type.to_string()))?;

                let fragment_type = FieldType::new_named(type_condition.clone());
                let known_type = current_type.inner_type_name().map(|s| s.to_string());

                // this is the type we pass when extracting the fragment's selections
                // If the type condition is a union or interface and the current type implements it, then we want
                // to keep the current type when extracting the fragment's selections, as it is more precise
                // than the interface.
                // If it is not, then we use the type condition
                let relevant_type = if schema.is_interface(type_condition.as_str()) {
                    // Query validation should have already verified that current type implements that interface
                    debug_assert!(
                        schema.is_subtype(
                            type_condition.as_str(),
                            current_type.inner_type_name().unwrap_or("")
                        ) || schema.is_implementation(
                            type_condition.as_str(),
                            current_type.inner_type_name().unwrap_or(""))
                     ||
                        // if the current type and the type condition are both the same interface, it is still valid
                        type_condition.as_str()
                            == current_type.inner_type_name().unwrap_or("")
                    );
                    let relevant_type = schema.most_precise(current_type, &fragment_type);
                    debug_assert!(relevant_type.is_some());
                    relevant_type.unwrap_or(&fragment_type)
                } else {
                    &fragment_type
                };

                let selection_set = inline_fragment
                    .selection_set()
                    .selection()
                    .iter()
                    .filter_map(|selection| {
                        Selection::from_hir(selection, relevant_type, schema, count, defer_stats)
                            .transpose()
                    })
                    .collect::<Result<_, _>>()?;

                Some(Self::InlineFragment {
                    type_condition,
                    selection_set,
                    include_skip,
                    defer,
                    defer_label,
                    known_type,
                })
            }
            // Spec: https://spec.graphql.org/draft/#FragmentSpread
            hir::Selection::FragmentSpread(fragment_spread) => {
                let include_skip = IncludeSkip::parse(fragment_spread.directives());
                if include_skip.statically_skipped() {
                    return Ok(None);
                }
                let (defer, defer_label) = parse_defer(fragment_spread.directives(), defer_stats);
                Some(Self::FragmentSpread {
                    name: fragment_spread.name().to_owned(),
                    known_type: current_type.inner_type_name().map(|s| s.to_string()),
                    include_skip,
                    defer,
                    defer_label,
                })
            }
        })
    }

    pub(crate) fn is_typename_field(&self) -> bool {
        matches!(self, Selection::Field {name, ..} if name.as_str() == TYPENAME)
    }

    pub(crate) fn output_key_if_typename_field(&self) -> Option<ByteString> {
        match self {
            Selection::Field { name, alias, .. } if name.as_str() == TYPENAME => {
                alias.as_ref().or(Some(name)).cloned()
            }
            _ => None,
        }
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

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub(crate) struct IncludeSkip {
    include: Condition,
    skip: Condition,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub(crate) enum Condition {
    Yes,
    No,
    Variable(String),
}

/// Returns the `if` condition and the `label`
fn parse_defer(
    directives: &[hir::Directive],
    defer_stats: &mut DeferStats,
) -> (Condition, Option<String>) {
    for directive in directives {
        if directive.name() == DEFER_DIRECTIVE_NAME {
            let condition = Condition::parse(directive).unwrap_or(Condition::Yes);
            match &condition {
                Condition::Yes => {
                    defer_stats.has_defer = true;
                    defer_stats.has_unconditional_defer = true;
                }
                Condition::No => {}
                Condition::Variable(name) => {
                    defer_stats.has_defer = true;
                    defer_stats
                        .conditional_defer_variable_names
                        .insert(name.clone());
                }
            }
            let label = if condition != Condition::No {
                directive
                    .argument_by_name("label")
                    .and_then(|value| value.as_str())
                    .map(|str| str.to_owned())
            } else {
                None
            };
            return (condition, label);
        }
    }
    (Condition::No, None)
}

impl IncludeSkip {
    pub(crate) fn parse(directives: &[hir::Directive]) -> Self {
        let mut include = None;
        let mut skip = None;
        for directive in directives {
            if include.is_none() && directive.name() == "include" {
                include = Condition::parse(directive)
            }
            if skip.is_none() && directive.name() == "skip" {
                skip = Condition::parse(directive)
            }
        }
        Self {
            include: include.unwrap_or(Condition::Yes),
            skip: skip.unwrap_or(Condition::No),
        }
    }

    pub(crate) fn statically_skipped(&self) -> bool {
        matches!(self.skip, Condition::Yes) || matches!(self.include, Condition::No)
    }

    pub(crate) fn should_skip(&self, variables: &Object) -> bool {
        // Using .unwrap_or is legit here because
        // validate_variables should have already checked that
        // the variable is present and it is of the correct type
        self.skip.eval(variables).unwrap_or(false) || !self.include.eval(variables).unwrap_or(true)
    }
}

impl Condition {
    pub(crate) fn parse(directive: &hir::Directive) -> Option<Self> {
        match directive.argument_by_name("if")? {
            hir::Value::Boolean { value: true, .. } => Some(Condition::Yes),
            hir::Value::Boolean { value: false, .. } => Some(Condition::No),
            hir::Value::Variable(variable) => Some(Condition::Variable(variable.name().to_owned())),
            _ => None,
        }
    }

    pub(crate) fn eval(&self, variables: &Object) -> Option<bool> {
        match self {
            Condition::Yes => Some(true),
            Condition::No => Some(false),
            Condition::Variable(variable_name) => variables
                .get(variable_name.as_str())
                .and_then(|v| v.as_bool()),
        }
    }
}
