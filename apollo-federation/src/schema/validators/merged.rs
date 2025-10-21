use std::sync::LazyLock;

use apollo_compiler::Name;
use apollo_compiler::executable::FieldSet;
use apollo_compiler::schema::Directive;
use apollo_compiler::schema::ExtendedType;
use apollo_compiler::schema::FieldDefinition;
use apollo_compiler::validation::Valid;
use itertools::Itertools;
use regex::Regex;

use crate::bail;
use crate::ensure;
use crate::error::CompositionError;
use crate::error::FederationError;
use crate::error::HasLocations;
use crate::error::SingleFederationError;
use crate::schema::FederationSchema;
use crate::schema::position::CompositeTypeDefinitionPosition;
use crate::schema::position::InterfaceTypeDefinitionPosition;
use crate::schema::position::ObjectFieldDefinitionPosition;
use crate::schema::position::ObjectOrInterfaceTypeDefinitionPosition;
use crate::subgraph::typestate::Subgraph;
use crate::subgraph::typestate::Validated;
use crate::utils::human_readable::human_readable_subgraph_names;

// PORT_NOTE: Named `postMergeValidations` in the JS codebase, but adjusted here to follow the
// naming convention in this directory. Note that this was normally a method in `Merger`, but as
// noted below, the logic overlaps with subgraph validation logic, so to facilitate that future
// de-duplication we're putting it here.
// TODO: The code here largely duplicates logic that is in subgraph schema validation, except that
// when it detects an error, it provides an error in terms of subgraph inputs (rather than what the
// merged/supergraph schema). We could try to avoid that duplication in the future.
pub(crate) fn validate_merged_schema(
    supergraph_schema: &FederationSchema,
    subgraphs: &Vec<Subgraph<Validated>>,
    errors: &mut Vec<CompositionError>,
) -> Result<(), FederationError> {
    for type_pos in supergraph_schema.get_types() {
        let Ok(type_pos) = ObjectOrInterfaceTypeDefinitionPosition::try_from(type_pos) else {
            continue;
        };
        let interface_names = match &type_pos {
            ObjectOrInterfaceTypeDefinitionPosition::Object(type_pos) => {
                &type_pos
                    .get(supergraph_schema.schema())?
                    .implements_interfaces
            }
            ObjectOrInterfaceTypeDefinitionPosition::Interface(type_pos) => {
                &type_pos
                    .get(supergraph_schema.schema())?
                    .implements_interfaces
            }
        };
        for interface_name in interface_names {
            let interface_pos = InterfaceTypeDefinitionPosition::new(interface_name.name.clone());
            for interface_field_pos in interface_pos.fields(supergraph_schema.schema())? {
                let field_pos = type_pos.field(interface_field_pos.field_name.clone());
                if field_pos.get(supergraph_schema.schema()).is_err() {
                    // This means that the type was defined (or at least implemented the interface)
                    // only in subgraphs where the interface didn't have that field.
                    let subgraphs_with_interface_field = subgraphs
                        .iter()
                        .filter(|subgraph| {
                            interface_field_pos.get(subgraph.schema().schema()).is_ok()
                        })
                        .map(|subgraph| subgraph.name.clone())
                        .collect::<Vec<_>>();
                    let subgraphs_with_type_implementing_interface = subgraphs
                        .iter()
                        .filter(|subgraph| {
                            let Some(subgraph_type) =
                                subgraph.schema().schema().types.get(type_pos.type_name())
                            else {
                                return false;
                            };
                            match &subgraph_type {
                                ExtendedType::Object(subgraph_type) => {
                                    subgraph_type.implements_interfaces.contains(interface_name)
                                }
                                ExtendedType::Interface(subgraph_type) => {
                                    subgraph_type.implements_interfaces.contains(interface_name)
                                }
                                _ => false,
                            }
                        })
                        .map(|subgraph| subgraph.name.clone())
                        .collect::<Vec<_>>();
                    errors.push(CompositionError::InterfaceFieldNoImplem {
                        message: format!(
                            "Interface field \"{}\" is declared in {} but type \"{}\", which implements \"{}\" only in {} does not have field \"{}\".",
                            interface_field_pos,
                            human_readable_subgraph_names(subgraphs_with_interface_field.iter()),
                            type_pos,
                            interface_name,
                            human_readable_subgraph_names(subgraphs_with_type_implementing_interface.iter()),
                            interface_field_pos.field_name,
                        )
                    });
                }

                // TODO: Should we validate more? Can we have some invalid implementation of a field
                // post-merging?
            }
        }
    }

    // We need to redo some validation for @requires after merging. The reason is that each subgraph
    // validates that its own @requires are valid relative to its own schema, but "requirements" are
    // really requested from _other_ subgraphs (by definition of @requires really), and there are a
    // few situations (see the details below) where validity within the @requires-declaring subgraph
    // does not entail validity for all subgraphs that would have to provide those "requirements".
    // To summarize, we need to re-validate every @requires against the supergraph to guarantee it
    // will always work at runtime.
    for subgraph in subgraphs.iter() {
        let requires_directive_definition_name = &subgraph
            .metadata()
            .federation_spec_definition()
            .requires_directive_definition(&subgraph.schema())?
            .name;
        let requires_referencers = subgraph
            .schema()
            .referencers
            .get_directive(requires_directive_definition_name)?;
        // Note that @requires is only supported on object fields.
        for parent_field_pos in &requires_referencers.object_fields {
            let Some(requires_directive) = parent_field_pos
                .get(subgraph.schema().schema())?
                .directives
                .get(requires_directive_definition_name)
            else {
                bail!("@requires unexpectedly missing from field that references it");
            };
            let requires_arguments = &subgraph
                .metadata()
                .federation_spec_definition()
                .requires_directive_arguments(requires_directive)?;
            // The type should exist in the supergraph schema. There are a few types we don't merge,
            // but those are from specific link/core features and they shouldn't have @requires. In
            // fact, if we were to not merge a type with a @requires, this would essentially mean
            // that @requires would not work, so its worth catching the issue early if this ever
            // happens for some reason. And of course, the type should be composite since it's also
            // one in at least the subgraph we're currently checking.
            let parent_type_pos_in_supergraph: CompositeTypeDefinitionPosition = supergraph_schema
                .get_type(parent_field_pos.type_name.clone())?
                .try_into()?;

            let Err(error) = FieldSet::parse_and_validate(
                Valid::assume_valid_ref(supergraph_schema.schema()),
                parent_type_pos_in_supergraph.type_name().clone(),
                requires_arguments.fields,
                "field_set.graphql",
            ) else {
                continue;
            };
            // Providing a useful error message to the user here is tricky in the general case
            // because what we checked is that a given subgraph @requires application is invalid "on
            // the supergraph", but the user seeing the error will not have the supergraph, so we
            // need to express the error in terms of the subgraphs.
            //
            // But in practice, there is only a handful of cases that can trigger an error here.
            // Indeed, at this point we know that:
            //  - The @requires application is valid in its original subgraph.
            //  - There was no merging errors (we don't call this method otherwise).
            // This eliminates the risk of the error being due to some invalid syntax, some
            // selection set on a non-composite type or missing selection set on a composite one
            // (merging would have errored), some unknown field in the field set (output types
            // are merged by union, so any field in the subgraph will be in the supergraph), or even
            // any error due to the types of fields involved (because the merged type is always a
            // (non-strict) supertype of its counterpart in any subgraph, and anything that could be
            // queried in a subtype can be queried on a supertype).
            //
            // As such, the only errors that we can have here are due to field arguments: because
            // they are merged by intersection, it _is_ possible that something that is valid in a
            // subgraph is not valid in the supergraph. And the only 2 things that can make such an
            // invalidity are:
            //  1. An argument may not be in the supergraph: it is in the subgraph, but not in all
            //     the subgraphs having the field, and the `@requires` passes a concrete value to
            //     that argument.
            //  2. The type of an argument in the supergraph is a strict subtype of the type of that
            //     argument in the subgraph (the one with the `@requires`) _and_ the @requires
            //     field set relies on the type difference. Now, argument types are input types, and
            //     the only subtyping difference that can occur with input types is related to
            //     nullability (input types support neither interfaces nor unions), so the only case
            //     this can happen is if a field `x` has some argument `a` with type `A` in the
            //     subgraph but type `A!` with no default in the supergraph, _and_ the `@requires`
            //     field set queries that field `x` _without_ a value for `a` (valid when `a` has
            //     type `A` but not with `A!` and no default).
            // So to ensure we provide good error messages, we brute-force detecting those 2
            // possible cases and have a special treatment for each.
            //
            // Note that this detection is based on pattern-matching the error message, which is
            // somewhat fragile, but because we only have 2 cases, we can easily cover them with
            // unit tests, which means there is no practical risk of a message change breaking this
            // code and being released undetected. A cleaner implementation would probably require
            // having error codes and variants for all the GraphQL validations. The apollo-compiler
            // crate has this already, but it's crate-private and potentially unstable, so we can't
            // use that for now.
            for error in FederationError::from(error).into_errors() {
                let SingleFederationError::InvalidGraphQL { message } = error else {
                    errors.push(CompositionError::SubgraphError {
                        subgraph: subgraph.name.to_string(),
                        error,
                        locations: requires_directive.locations(subgraph),
                    });
                    continue;
                };
                if let Some(captures) =
                    APOLLO_COMPILER_UNDEFINED_ARGUMENT_PATTERN.captures(&message)
                {
                    let Some(argument_name) = captures.get(1).map(|m| m.as_str()) else {
                        bail!("Unexpectedly no argument name in undefined argument error regex")
                    };
                    let Some(type_name) = captures.get(2).map(|m| m.as_str()) else {
                        bail!("Unexpectedly no type name in undefined argument error regex")
                    };
                    let Some(field_name) = captures.get(3).map(|m| m.as_str()) else {
                        bail!("Unexpectedly no field name in undefined argument error regex")
                    };
                    add_requires_error(
                        parent_field_pos,
                        requires_directive,
                        &subgraph.name,
                        type_name,
                        field_name,
                        argument_name,
                        |field_definition| {
                            Ok(field_definition.argument_by_name(argument_name).is_none())
                        },
                        |incompatible_subgraphs| {
                            Ok(format!(
                                "cannot provide a value for argument \"{argument_name}\" of field \"{type_name}.{field_name}\" as argument \"{argument_name}\" is not defined in {incompatible_subgraphs}",
                            ))
                        },
                        subgraphs,
                        errors,
                    )?;
                    continue;
                }
                if let Some(captures) = APOLLO_COMPILER_REQUIRED_ARGUMENT_PATTERN.captures(&message)
                {
                    let Some(type_name) = captures.get(1).map(|m| m.as_str()) else {
                        bail!("Unexpectedly no type name in required argument error regex");
                    };
                    let Some(field_name) = captures.get(2).map(|m| m.as_str()) else {
                        bail!("Unexpectedly no field name in required argument error regex");
                    };
                    let Some(argument_name) = captures.get(3).map(|m| m.as_str()) else {
                        bail!("Unexpectedly no argument name in required argument error regex");
                    };
                    add_requires_error(
                        parent_field_pos,
                        requires_directive,
                        &subgraph.name,
                        type_name,
                        field_name,
                        argument_name,
                        |field_definition| {
                            Ok(field_definition
                                .argument_by_name(argument_name)
                                .map(|arg| arg.is_required())
                                .unwrap_or_default())
                        },
                        |incompatible_subgraphs| {
                            Ok(format!(
                                "no value provided for argument \"{argument_name}\" of field \"{type_name}.{field_name}\" but a value is mandatory as \"{argument_name}\" is required in {incompatible_subgraphs}",
                            ))
                        },
                        subgraphs,
                        errors,
                    )?;
                    continue;
                }
                bail!(
                    "Unexpected error throw by {} when evaluated on supergraph: {}",
                    requires_directive,
                    message,
                );
            }
        }
    }

    Ok(())
}

// This matches the error message for `DiagnosticData::UndefinedArgument` as defined in
// https://github.com/apollographql/apollo-rs/blob/apollo-compiler%401.28.0/crates/apollo-compiler/src/validation/diagnostics.rs#L36
static APOLLO_COMPILER_UNDEFINED_ARGUMENT_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"the argument `((?-u:\w)+)` is not supported by `((?-u:\w)+)\.((?-u:\w)+)`"#)
        .unwrap()
});

// This matches the error message for `DiagnosticData::RequiredArgument` as defined in
// https://github.com/apollographql/apollo-rs/blob/apollo-compiler%401.28.0/crates/apollo-compiler/src/validation/diagnostics.rs#L88
static APOLLO_COMPILER_REQUIRED_ARGUMENT_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r#"the required argument `((?-u:\w)+)\.((?-u:\w)+)\(((?-u:\w)+):\)` is not provided"#,
    )
    .unwrap()
});

#[allow(clippy::too_many_arguments)]
fn add_requires_error(
    requires_parent_field_pos: &ObjectFieldDefinitionPosition,
    requires_application: &Directive,
    subgraph_name: &str,
    type_name: &str,
    field_name: &str,
    argument_name: &str,
    is_field_incompatible: impl Fn(&FieldDefinition) -> Result<bool, FederationError>,
    message_for_incompatible_subgraphs: impl Fn(&str) -> Result<String, FederationError>,
    subgraphs: &Vec<Subgraph<Validated>>,
    errors: &mut Vec<CompositionError>,
) -> Result<(), FederationError> {
    let type_name = Name::new(type_name)?;
    let field_name = Name::new(field_name)?;
    let argument_name = Name::new(argument_name)?;
    let mut locations = Vec::with_capacity(subgraphs.len());
    let incompatible_subgraph_names = subgraphs
        .iter()
        .map(|other_subgraph| {
            if other_subgraph.name == subgraph_name {
                return Ok(None);
            }
            let Ok(type_pos_in_other_subgraph) =
                other_subgraph.schema().get_type(type_name.clone())
            else {
                return Ok(None);
            };
            let Ok(type_pos_in_other_subgraph) =
                ObjectOrInterfaceTypeDefinitionPosition::try_from(type_pos_in_other_subgraph)
            else {
                return Ok(None);
            };
            let Some(field_in_other_subgraph) = type_pos_in_other_subgraph
                .field(field_name.clone())
                .try_get(other_subgraph.schema().schema())
            else {
                return Ok(None);
            };
            let is_field_incompatible = is_field_incompatible(field_in_other_subgraph)?;
            if is_field_incompatible {
                locations.extend(other_subgraph.node_locations(field_in_other_subgraph));
                Ok::<_, FederationError>(Some(other_subgraph.name.to_string()))
            } else {
                Ok(None)
            }
        })
        .process_results(|iter| iter.flatten().collect::<Vec<_>>())?;
    ensure!(
        !incompatible_subgraph_names.is_empty(),
        "Got error on argument \"{}\" of field \"{}\" but no \"incompatible\" subgraphs",
        argument_name,
        field_name,
    );
    let incompatible_subgraph_names =
        human_readable_subgraph_names(incompatible_subgraph_names.into_iter());
    let message = message_for_incompatible_subgraphs(&incompatible_subgraph_names)?;

    errors.push(CompositionError::SubgraphError {
        subgraph: subgraph_name.to_string(),
        error: SingleFederationError::RequiresInvalidFields {
            coordinate: requires_parent_field_pos.to_string(),
            application: requires_application.to_string(),
            message,
        },
        locations,
    });
    Ok(())
}
