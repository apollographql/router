//! Validation of `@is` and `@require` selection maps against other source schemas
//! (`IS_INVALID_FIELDS`, `REQUIRE_INVALID_FIELDS`).
//!
//! The specification places these rules after merging, validating against "the union of other
//! schemas except all fields marked as `@internal`". The union is taken directly over the
//! validated subgraphs here, which gives the same answer without depending on merge output (the
//! specification permits reordering checks).

use apollo_compiler::Name;
use apollo_compiler::Node;
use apollo_compiler::Schema;
use apollo_compiler::collections::IndexSet;
use apollo_compiler::schema::ExtendedType;
use apollo_compiler::schema::FieldDefinition;

use super::CompositeNames;
use super::validation::parse_field_selection_map_argument;
use crate::error::CompositionError;
use crate::error::SingleFederationError;
use crate::link::federation_spec_definition::FEDERATION_EXTERNAL_DIRECTIVE_NAME_IN_SPEC;
use crate::link::federation_spec_definition::FEDERATION_INTERNAL_DIRECTIVE_NAME_IN_SPEC;
use crate::link::spec_definition::SpecDefinition;
use crate::schema::field_selection_map::validate::OutputSchema;
use crate::schema::field_selection_map::validate::field_in_schema;
use crate::schema::field_selection_map::validate::is_composite_in_schema;
use crate::schema::field_selection_map::validate::is_leaf_in_schema;
use crate::schema::field_selection_map::validate::possible_types;
use crate::schema::field_selection_map::validate::validate;
use crate::subgraph::typestate::Subgraph;
use crate::subgraph::typestate::Validated;

/// One subgraph's contribution to a cross-schema output side.
struct Member<'a> {
    schema: &'a Schema,
    internal: Option<Name>,
    external: Option<Name>,
}

/// The union of several subgraph schemas, without `@internal` elements, and optionally without
/// `@external` fields (a requirement must be resolvable where it is declared).
struct SubgraphsOutput<'a> {
    members: Vec<Member<'a>>,
    exclude_external: bool,
}

impl SubgraphsOutput<'_> {
    fn type_is_internal(member: &Member<'_>, type_name: &Name) -> bool {
        member.internal.as_ref().is_some_and(|internal| {
            member
                .schema
                .types
                .get(type_name)
                .is_some_and(|ty| ty.directives().has(internal))
        })
    }
}

impl OutputSchema for SubgraphsOutput<'_> {
    fn field(
        &self,
        type_name: &Name,
        field_name: &Name,
    ) -> Option<(&Node<FieldDefinition>, &Schema)> {
        self.members.iter().find_map(|member| {
            if Self::type_is_internal(member, type_name) {
                return None;
            }
            let field = field_in_schema(member.schema, type_name, field_name)?;
            if member
                .internal
                .as_ref()
                .is_some_and(|internal| field.directives.has(internal))
            {
                return None;
            }
            if self.exclude_external
                && let Some(external) = &member.external
            {
                let type_external = member
                    .schema
                    .types
                    .get(type_name)
                    .is_some_and(|ty| ty.directives().has(external));
                if type_external || field.directives.has(external) {
                    return None;
                }
            }
            Some((field, member.schema))
        })
    }

    fn is_composite(&self, type_name: &Name) -> bool {
        self.members.iter().any(|m| {
            !Self::type_is_internal(m, type_name) && is_composite_in_schema(m.schema, type_name)
        })
    }

    fn is_leaf(&self, type_name: &Name) -> bool {
        self.members
            .iter()
            .any(|m| is_leaf_in_schema(m.schema, type_name))
    }

    fn possible_types(&self, type_name: &Name) -> IndexSet<Name> {
        self.members
            .iter()
            .filter(|m| !Self::type_is_internal(m, type_name))
            .flat_map(|m| possible_types(m.schema, type_name))
            .collect()
    }
}

fn member<'a>(subgraph: &'a Subgraph<Validated>) -> Member<'a> {
    let schema = subgraph.validated_schema();
    let spec = subgraph.metadata().federation_spec_definition();
    let name = |n: &Name| {
        spec.directive_name_in_schema(subgraph.schema(), n)
            .filter(|n| schema.schema().directive_definitions.contains_key(n))
    };
    Member {
        schema: schema.schema(),
        internal: name(&FEDERATION_INTERNAL_DIRECTIVE_NAME_IN_SPEC),
        external: name(&FEDERATION_EXTERNAL_DIRECTIVE_NAME_IN_SPEC),
    }
}

/// Validate every `@is` and `@require` selection map of every source schema against the other
/// subgraphs.
pub(crate) fn validate_selection_maps(subgraphs: &[Subgraph<Validated>]) -> Vec<CompositionError> {
    let mut errors = Vec::new();
    for (index, subgraph) in subgraphs.iter().enumerate() {
        if !subgraph.metadata().is_composite_schema() {
            continue;
        }
        let Some(names) = CompositeNames::new(
            subgraph.schema(),
            subgraph.metadata().federation_spec_definition(),
        ) else {
            continue;
        };
        let schema = subgraph.validated_schema().schema();
        let is_output = SubgraphsOutput {
            members: subgraphs.iter().map(member).collect(),
            exclude_external: false,
        };
        let require_output = SubgraphsOutput {
            members: subgraphs
                .iter()
                .enumerate()
                .filter(|(i, _)| *i != index)
                .map(|(_, s)| member(s))
                .collect(),
            exclude_external: true,
        };
        let mut report = |error: SingleFederationError| {
            errors.push(CompositionError::SubgraphError {
                subgraph: subgraph.name.clone(),
                error,
                locations: Vec::new(),
            })
        };
        for (type_name, ty) in &schema.types {
            let fields: Vec<&Node<FieldDefinition>> = match ty {
                ExtendedType::Object(object) => object.fields.values().map(|f| &f.node).collect(),
                ExtendedType::Interface(interface) => {
                    interface.fields.values().map(|f| &f.node).collect()
                }
                _ => continue,
            };
            for field in fields {
                for argument in &field.arguments {
                    let coordinate = format!("{type_name}.{}({}:)", field.name, argument.name);
                    if let Some(is) = argument.directives.get(&names.is) {
                        let Ok(map) = parse_field_selection_map_argument(
                            is,
                            "is",
                            &coordinate,
                            |message| SingleFederationError::IsInvalidFieldType { message },
                            |message| SingleFederationError::IsInvalidSyntax { message },
                        ) else {
                            continue;
                        };
                        let root = field.ty.inner_named_type();
                        for error in validate(&map, &is_output, root, schema, &argument.ty) {
                            report(SingleFederationError::IsInvalidFields {
                                message: format!(
                                    "The @is directive on \"{coordinate}\" has an invalid field \
                                     selection map \"{map}\": {}.",
                                    error.message
                                ),
                            });
                        }
                    }
                    if let Some(require) = argument.directives.get(&names.require) {
                        let Ok(map) = parse_field_selection_map_argument(
                            require,
                            "require",
                            &coordinate,
                            |message| SingleFederationError::RequireInvalidFieldType { message },
                            |message| SingleFederationError::RequireInvalidSyntax { message },
                        ) else {
                            continue;
                        };
                        for error in
                            validate(&map, &require_output, type_name, schema, &argument.ty)
                        {
                            report(SingleFederationError::RequireInvalidFields {
                                message: format!(
                                    "The @require directive on \"{coordinate}\" has an invalid field \
                                     selection map \"{map}\": {} (a requirement must select fields \
                                     resolvable by another subgraph).",
                                    error.message
                                ),
                            });
                        }
                    }
                }
            }
        }
    }
    errors
}
