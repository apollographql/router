use apollo_compiler::Name;
use apollo_compiler::Node;
use apollo_compiler::Schema;
use apollo_compiler::ast::InputValueDefinition;
use apollo_compiler::ast::Type;
use apollo_compiler::ast::Value;
use apollo_compiler::coordinate::DirectiveArgumentCoordinate;
use apollo_compiler::coordinate::FieldArgumentCoordinate;
use apollo_compiler::coordinate::TypeAttributeCoordinate;
use apollo_compiler::schema::ExtendedType;
use itertools::Itertools;

use crate::error::CompositionError;
use crate::error::HasLocations;
use crate::schema::FederationSchema;
use crate::subgraph::typestate::Subgraph;
use crate::subgraph::typestate::Validated;
use crate::utils::human_readable::human_readable_subgraph_names;

/// The supergraph keeps `@oneOf` if any subgraph applies it, so a default value written in a
/// subgraph without `@oneOf` can become invalid after merging. This reports those defaults against
/// the subgraphs that declared them, rather than letting supergraph validation fail without
/// pointing back to any subgraph.
pub(crate) fn validate_one_of_default_values(
    supergraph_schema: &FederationSchema,
    subgraphs: &[Subgraph<Validated>],
    errors: &mut Vec<CompositionError>,
) {
    let schema = supergraph_schema.schema();
    let has_one_of = schema.types.values().any(|ty| match ty {
        ExtendedType::InputObject(input) => input.is_one_of(),
        _ => false,
    });
    if !has_one_of {
        return;
    }

    for (coordinate, definition) in default_values(schema) {
        let Some(default_value) = &definition.default_value else {
            continue;
        };
        let Some(violation) = find_violation(schema, &definition.ty, default_value) else {
            continue;
        };

        let declaring_subgraphs = subgraphs
            .iter()
            .filter(|subgraph| {
                coordinate
                    .lookup(subgraph.schema().schema())
                    .is_some_and(|def| def.default_value.is_some())
            })
            .collect_vec();
        let one_of_subgraphs = subgraphs.iter().filter(|subgraph| {
            subgraph
                .schema()
                .schema()
                .get_input_object(&violation.type_name)
                .is_some_and(|input| input.is_one_of())
        });
        let message = format!(
            "The default value of \"{coordinate}\" in {} is invalid because \"{}\" is marked @oneOf in {}: {}.",
            human_readable_subgraph_names(declaring_subgraphs.iter().map(|s| &s.name)),
            violation.type_name,
            human_readable_subgraph_names(one_of_subgraphs.map(|s| &s.name)),
            violation.reason,
        );
        let locations = declaring_subgraphs
            .iter()
            .filter_map(|subgraph| {
                coordinate
                    .lookup(subgraph.schema().schema())
                    .map(|def| def.locations(subgraph))
            })
            .flatten()
            .collect();
        errors.push(match coordinate {
            DefaultValueCoordinate::InputField(_) => {
                CompositionError::InputFieldDefaultMismatch { message, locations }
            }
            DefaultValueCoordinate::FieldArgument(_)
            | DefaultValueCoordinate::DirectiveArgument(_) => {
                CompositionError::ArgumentDefaultMismatch { message, locations }
            }
        });
    }
}

enum DefaultValueCoordinate {
    FieldArgument(FieldArgumentCoordinate),
    InputField(TypeAttributeCoordinate),
    DirectiveArgument(DirectiveArgumentCoordinate),
}

impl DefaultValueCoordinate {
    fn lookup<'schema>(
        &self,
        schema: &'schema Schema,
    ) -> Option<&'schema Node<InputValueDefinition>> {
        match self {
            Self::FieldArgument(coord) => coord.lookup(schema).ok(),
            Self::InputField(coord) => coord.lookup_input_field(schema).ok(),
            Self::DirectiveArgument(coord) => coord.lookup(schema).ok(),
        }
    }
}

impl std::fmt::Display for DefaultValueCoordinate {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::FieldArgument(coord) => coord.fmt(f),
            Self::InputField(coord) => coord.fmt(f),
            Self::DirectiveArgument(coord) => coord.fmt(f),
        }
    }
}

/// Every argument and input field definition in the schema that can hold a default value.
fn default_values(
    schema: &Schema,
) -> impl Iterator<Item = (DefaultValueCoordinate, &Node<InputValueDefinition>)> {
    let type_defaults = schema.types.iter().flat_map(|(type_name, ty)| {
        let field_arguments = match ty {
            ExtendedType::Object(object) => Some(&object.fields),
            ExtendedType::Interface(interface) => Some(&interface.fields),
            _ => None,
        }
        .into_iter()
        .flatten()
        .flat_map(move |(field_name, field)| {
            field.arguments.iter().map(move |argument| {
                let coordinate = FieldArgumentCoordinate {
                    ty: type_name.clone(),
                    field: field_name.clone(),
                    argument: argument.name.clone(),
                };
                (DefaultValueCoordinate::FieldArgument(coordinate), argument)
            })
        });
        let input_fields = match ty {
            ExtendedType::InputObject(input) => Some(&input.fields),
            _ => None,
        }
        .into_iter()
        .flatten()
        .map(move |(field_name, field)| {
            let coordinate = TypeAttributeCoordinate {
                ty: type_name.clone(),
                attribute: field_name.clone(),
            };
            (DefaultValueCoordinate::InputField(coordinate), field)
        });
        field_arguments.chain(input_fields)
    });
    let directive_defaults =
        schema
            .directive_definitions
            .iter()
            .flat_map(|(directive_name, directive)| {
                directive.arguments.iter().map(move |argument| {
                    let coordinate = DirectiveArgumentCoordinate {
                        directive: directive_name.clone(),
                        argument: argument.name.clone(),
                    };
                    (
                        DefaultValueCoordinate::DirectiveArgument(coordinate),
                        argument,
                    )
                })
            });
    type_defaults.chain(directive_defaults)
}

struct OneOfViolation {
    type_name: Name,
    reason: String,
}

/// Walks a value against its type and returns the first `@oneOf` input object it doesn't satisfy.
fn find_violation(schema: &Schema, ty: &Type, value: &Value) -> Option<OneOfViolation> {
    match value {
        Value::List(items) => {
            // A single value is coerced to a list, so the item type applies either way.
            let item_type = ty.item_type();
            items
                .iter()
                .find_map(|item| find_violation(schema, item_type, item))
        }
        Value::Object(fields) => {
            let input = schema.get_input_object(ty.inner_named_type())?;
            if input.is_one_of() {
                if fields.len() != 1 {
                    return Some(OneOfViolation {
                        type_name: input.name.clone(),
                        reason: format!(
                            "@oneOf input object \"{}\" must specify exactly one key, but {} were given",
                            input.name,
                            fields.len()
                        ),
                    });
                }
                if let Some((field_name, _)) = fields.iter().find(|(_, value)| value.is_null()) {
                    return Some(OneOfViolation {
                        type_name: input.name.clone(),
                        reason: format!(
                            "@oneOf input object \"{}\" field \"{field_name}\" must be non-null",
                            input.name
                        ),
                    });
                }
            }
            fields.iter().find_map(|(field_name, value)| {
                let field = input.fields.get(field_name)?;
                find_violation(schema, &field.ty, value)
            })
        }
        _ => None,
    }
}
