//! Source-schema validation of the GraphQL Federation directives (spec §4 "Validate Source
//! Schemas"), plus the Apollo-specific restrictions on federation directives in source schemas.
//!
//! Rules whose output side ranges over other source schemas (`IS_INVALID_FIELDS`,
//! `REQUIRE_INVALID_FIELDS`) run after merging; see [`super::post_merge`].

use apollo_compiler::Name;
use apollo_compiler::Node;
use apollo_compiler::Schema;
use apollo_compiler::ast;
use apollo_compiler::ast::FieldDefinition;
use apollo_compiler::collections::IndexSet;
use apollo_compiler::schema::ExtendedType;

use super::CompositeNames;
use crate::error::FederationError;
use crate::error::MultipleFederationErrors;
use crate::error::SingleFederationError;
use crate::merger::hints::HintCode;
use crate::schema::ValidFederationSchema;
use crate::schema::field_selection_map;
use crate::schema::field_selection_map::SelectedValue;
use crate::schema::field_selection_map::validate::has_arguments;
use crate::schema::field_selection_map::validate::possible_types;
use crate::schema::field_selection_map::value::is_mappable;
use crate::supergraph::CompositionHint;

/// Where a `FieldSelectionMap` directive is applied: `Type.field(argument:)`.
struct ArgumentCoordinate<'a> {
    type_name: &'a Name,
    field: &'a FieldDefinition,
    argument: &'a Node<ast::InputValueDefinition>,
}

impl std::fmt::Display for ArgumentCoordinate<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}.{}({}:)",
            self.type_name, self.field.name, self.argument.name
        )
    }
}

/// Reads the `field` argument of an `@is`/`@require` application and parses it. `Err` carries the
/// error to report (invalid type or invalid syntax).
pub(crate) fn parse_field_selection_map_argument(
    directive: &ast::Directive,
    directive_label: &str,
    coordinate: &dyn std::fmt::Display,
    invalid_type: fn(String) -> SingleFederationError,
    invalid_syntax: fn(String) -> SingleFederationError,
) -> Result<SelectedValue, SingleFederationError> {
    let Some(value) = directive.specified_argument_by_name("field") else {
        return Err(invalid_type(format!(
            "The @{directive_label} directive on \"{coordinate}\" must specify a \"field\" argument."
        )));
    };
    let Some(text) = value.as_str() else {
        return Err(invalid_type(format!(
            "The \"field\" argument of the @{directive_label} directive on \"{coordinate}\" must be \
             a string, but found {}.",
            value.serialize().no_indent()
        )));
    };
    field_selection_map::parse(text).map_err(|error| {
        invalid_syntax(format!(
            "The \"field\" argument of the @{directive_label} directive on \"{coordinate}\" is not a \
             valid field selection map: {error}."
        ))
    })
}

fn is_invalid_field_type(message: String) -> SingleFederationError {
    SingleFederationError::IsInvalidFieldType { message }
}
fn is_invalid_syntax(message: String) -> SingleFederationError {
    SingleFederationError::IsInvalidSyntax { message }
}
fn require_invalid_field_type(message: String) -> SingleFederationError {
    SingleFederationError::RequireInvalidFieldType { message }
}
fn require_invalid_syntax(message: String) -> SingleFederationError {
    SingleFederationError::RequireInvalidSyntax { message }
}

/// The fields of an object or interface type.
fn fields_of(ty: &ExtendedType) -> Option<impl Iterator<Item = &Node<FieldDefinition>>> {
    match ty {
        ExtendedType::Object(object) => Some(
            object
                .fields
                .values()
                .map(|f| &f.node)
                .collect::<Vec<_>>()
                .into_iter(),
        ),
        ExtendedType::Interface(interface) => Some(
            interface
                .fields
                .values()
                .map(|f| &f.node)
                .collect::<Vec<_>>()
                .into_iter(),
        ),
        _ => None,
    }
}

/// The composite types reachable from the query root by following fields without arguments that
/// do not return lists: where lookup fields may be declared (spec §2 "@lookup").
pub(crate) fn lookup_reachable_types(schema: &Schema) -> IndexSet<Name> {
    let mut reachable = IndexSet::default();
    let Some(query) = schema.schema_definition.query.as_ref() else {
        return reachable;
    };
    let mut queue = vec![query.name.clone()];
    while let Some(type_name) = queue.pop() {
        if !reachable.insert(type_name.clone()) {
            continue;
        }
        let Some(fields) = schema.types.get(&type_name).and_then(fields_of) else {
            continue;
        };
        for field in fields {
            if !field.arguments.is_empty() || field.ty.is_list() {
                continue;
            }
            let target = field.ty.inner_named_type();
            if matches!(
                schema.types.get(target),
                Some(ExtendedType::Object(_) | ExtendedType::Interface(_))
            ) && !reachable.contains(target)
            {
                queue.push(target.clone());
            }
        }
    }
    reachable
}

struct SourceSchemaValidator<'a> {
    schema: &'a Schema,
    names: &'a CompositeNames,
    errors: MultipleFederationErrors,
    hints: Vec<CompositionHint>,
}

/// Validate a GraphQL Federation source schema. Returns the hints (warnings) raised; errors are
/// returned as a `FederationError`. Federation subgraphs are not affected.
pub(crate) fn validate_source_schema(
    schema: &ValidFederationSchema,
) -> Result<Vec<CompositionHint>, FederationError> {
    let Some(metadata) = schema.subgraph_metadata() else {
        return Ok(Vec::new());
    };
    if !metadata.is_composite_schema() {
        return Ok(Vec::new());
    }
    let Some(names) = CompositeNames::new(schema, metadata.federation_spec_definition()) else {
        return Ok(Vec::new());
    };
    let mut validator = SourceSchemaValidator {
        schema: schema.schema(),
        names: &names,
        errors: MultipleFederationErrors { errors: Vec::new() },
        hints: Vec::new(),
    };
    validator.validate();
    validator.errors.into_result()?;
    Ok(validator.hints)
}

impl SourceSchemaValidator<'_> {
    fn error(&mut self, error: SingleFederationError) {
        self.errors.push(error.into());
    }

    fn validate(&mut self) {
        let reachable = lookup_reachable_types(self.schema);
        let internal_types: IndexSet<Name> = self
            .schema
            .types
            .iter()
            .filter(|(_, ty)| ty.directives().has(&self.names.internal))
            .map(|(name, _)| name.clone())
            .collect();

        for (type_name, ty) in &self.schema.types {
            self.validate_type_level_directives(type_name, ty);
            let Some(fields) = fields_of(ty) else {
                continue;
            };
            let type_is_internal = internal_types.contains(type_name);
            let interface_type_names = match ty {
                ExtendedType::Object(object) => object
                    .implements_interfaces
                    .iter()
                    .map(|i| i.name.clone())
                    .collect(),
                _ => Vec::new(),
            };
            for field in fields {
                self.validate_field(type_name, field, &reachable);
                // `REFERENCE_TO_INTERNAL_TYPE`: internal types are local to their source schema,
                // so only public elements of this same schema can reference them.
                let field_is_internal =
                    type_is_internal || field.directives.has(&self.names.internal);
                if !field_is_internal {
                    self.check_internal_reference(
                        &internal_types,
                        field.ty.inner_named_type(),
                        &format!("field \"{type_name}.{}\"", field.name),
                    );
                    for argument in &field.arguments {
                        if argument.directives.has(&self.names.require) {
                            continue;
                        }
                        self.check_internal_reference(
                            &internal_types,
                            argument.ty.inner_named_type(),
                            &format!(
                                "argument \"{type_name}.{}({}:)\"",
                                field.name, argument.name
                            ),
                        );
                    }
                }
                self.validate_require_consistency(type_name, field, &interface_type_names);
            }
        }
        self.validate_internal_key_fields();
    }

    fn check_internal_reference(
        &mut self,
        internal_types: &IndexSet<Name>,
        referenced: &Name,
        element: &str,
    ) {
        if internal_types.contains(referenced) {
            self.error(SingleFederationError::ReferenceToInternalType {
                message: format!(
                    "The {element} references the @internal type \"{referenced}\"; only internal \
                     fields may reference internal types."
                ),
            });
        }
    }

    fn validate_type_level_directives(&mut self, type_name: &Name, ty: &ExtendedType) {
        for directive in ty.directives().iter() {
            if Some(&directive.name) == self.names.context.as_ref() {
                self.error(SingleFederationError::ContextInSourceSchemaUnsupported {
                    message: format!(
                        "The @{} directive on type \"{type_name}\" is not yet supported in a \
                         GraphQL Federation source schema.",
                        directive.name
                    ),
                });
            }
            if directive.name == self.names.key
                && directive.specified_argument_by_name("resolvable").is_some()
            {
                self.error(SingleFederationError::KeyResolvableInSourceSchema {
                    message: format!(
                        "The @key directive on type \"{type_name}\" specifies \"resolvable\", which \
                         a GraphQL Federation source schema cannot use: a key is resolvable when \
                         a @lookup field resolves the entity by it."
                    ),
                });
            }
        }
    }

    fn validate_field(
        &mut self,
        type_name: &Name,
        field: &Node<FieldDefinition>,
        reachable: &IndexSet<Name>,
    ) {
        let coordinate = format!("{type_name}.{}", field.name);
        if let Some(requires) = &self.names.requires
            && field.directives.has(requires)
        {
            self.error(SingleFederationError::RequiresInSourceSchema {
                message: format!(
                    "The field \"{coordinate}\" uses @{requires}, which a GraphQL Federation source \
                     schema cannot use: express the requirement with @require on an argument \
                     instead."
                ),
            });
        }
        let is_lookup = field.directives.has(&self.names.lookup);
        if is_lookup {
            self.validate_lookup(type_name, field, reachable);
        }
        for argument in &field.arguments {
            let argument_coordinate = ArgumentCoordinate {
                type_name,
                field,
                argument,
            };
            if let Some(from_context) = &self.names.from_context
                && argument.directives.has(from_context)
            {
                self.error(SingleFederationError::ContextInSourceSchemaUnsupported {
                    message: format!(
                        "The @{from_context} directive on \"{argument_coordinate}\" is not yet \
                         supported in a GraphQL Federation source schema."
                    ),
                });
            }
            if let Some(is) = argument.directives.get(&self.names.is) {
                if !is_lookup {
                    self.error(SingleFederationError::IsInvalidUsage {
                        message: format!(
                            "The @is directive on \"{argument_coordinate}\" can only be applied to \
                             arguments of @lookup fields."
                        ),
                    });
                }
                match parse_field_selection_map_argument(
                    is,
                    "is",
                    &argument_coordinate,
                    is_invalid_field_type,
                    is_invalid_syntax,
                ) {
                    Ok(map) => {
                        if has_arguments(&map) {
                            self.error(SingleFederationError::IsFieldsHasArguments {
                                message: format!(
                                    "The @is directive on \"{argument_coordinate}\" selects \
                                     fields with arguments; an @is selection map must consist of \
                                     plain field paths."
                                ),
                            });
                        }
                    }
                    Err(error) => self.error(error),
                }
            }
            if let Some(require) = argument.directives.get(&self.names.require) {
                if is_lookup {
                    self.error(SingleFederationError::RequireInvalidUsage {
                        message: format!(
                            "The @require directive on \"{argument_coordinate}\" is not allowed: \
                             arguments of @lookup fields are the entity's stable key and cannot \
                             be requirements."
                        ),
                    });
                }
                if let Err(error) = parse_field_selection_map_argument(
                    require,
                    "require",
                    &argument_coordinate,
                    require_invalid_field_type,
                    require_invalid_syntax,
                ) {
                    self.error(error);
                }
            }
        }
    }

    fn validate_lookup(
        &mut self,
        type_name: &Name,
        field: &Node<FieldDefinition>,
        reachable: &IndexSet<Name>,
    ) {
        let coordinate = format!("{type_name}.{}", field.name);
        if field.arguments.is_empty() {
            self.error(SingleFederationError::LookupMustHaveArguments {
                message: format!(
                    "The @lookup field \"{coordinate}\" must declare at least one argument."
                ),
            });
        }
        if field.ty.is_list() {
            self.error(SingleFederationError::LookupReturnsList {
                message: format!(
                    "The @lookup field \"{coordinate}\" must not return a list, but returns \"{}\".",
                    field.ty
                ),
            });
        }
        if field.ty.is_non_null() {
            self.hints.push(CompositionHint {
                definition: HintCode::LookupReturnsNonNullableType.definition(),
                message: format!(
                    "The @lookup field \"{coordinate}\" returns the non-nullable type \"{}\"; a \
                     lookup should return a nullable type so that an entity that does not exist \
                     resolves to null rather than failing the response.",
                    field.ty
                ),
                locations: Vec::new(),
            });
        }
        if !reachable.contains(type_name) {
            self.error(SingleFederationError::LookupNotReachable {
                message: format!(
                    "The @lookup field \"{coordinate}\" is not reachable from the query root: a \
                     lookup must be declared on the query root type, or on a type reached from it \
                     through fields without arguments that do not return lists."
                ),
            });
        }
        let return_type = field.ty.inner_named_type();
        let concrete_types = possible_types(self.schema, return_type);
        for argument in &field.arguments {
            // A requirement is not part of the key (and is rejected on lookups anyway).
            if argument.directives.has(&self.names.require) {
                continue;
            }
            let map = match argument.directives.get(&self.names.is) {
                Some(is) => match parse_field_selection_map_argument(
                    is,
                    "is",
                    &"",
                    is_invalid_field_type,
                    is_invalid_syntax,
                ) {
                    Ok(map) => map,
                    // Reported by `validate_field`.
                    Err(_) => continue,
                },
                None => SelectedValue::field(argument.name.clone()),
            };
            for concrete in &concrete_types {
                if !is_mappable(&map, self.schema, concrete) {
                    self.error(SingleFederationError::LookupKeyMissingForType {
                        message: format!(
                            "The argument \"{coordinate}({}:)\" of the @lookup field cannot be \
                             mapped for the possible type \"{concrete}\" of \"{return_type}\": \
                             {}.",
                            argument.name,
                            if argument.directives.has(&self.names.is) {
                                format!("no alternative of its @is selection map applies to \"{concrete}\" with fields \"{concrete}\" defines")
                            } else {
                                format!("\"{concrete}\" has no field named \"{}\"", argument.name)
                            }
                        ),
                    });
                }
            }
        }
    }

    /// `REQUIRE_INCONSISTENT_ON_IMPLEMENTATION`: an argument carries `@require` on an interface
    /// field if and only if it does on the corresponding field of every implementation.
    fn validate_require_consistency(
        &mut self,
        type_name: &Name,
        field: &Node<FieldDefinition>,
        interface_type_names: &[Name],
    ) {
        for interface_name in interface_type_names {
            let Some(ExtendedType::Interface(interface)) = self.schema.types.get(interface_name)
            else {
                continue;
            };
            let Some(interface_field) = interface.fields.get(&field.name) else {
                continue;
            };
            for interface_argument in &interface_field.arguments {
                let on_interface = interface_argument.directives.has(&self.names.require);
                let on_implementation = field
                    .arguments
                    .iter()
                    .find(|a| a.name == interface_argument.name)
                    .is_some_and(|a| a.directives.has(&self.names.require));
                if on_interface != on_implementation {
                    self.error(SingleFederationError::RequireInconsistentOnImplementation {
                        message: format!(
                            "The argument \"{interface_name}.{}({}:)\" is{} annotated with \
                             @require, but the corresponding argument of the implementing field \
                             \"{type_name}.{}\" is{}.",
                            field.name,
                            interface_argument.name,
                            if on_interface { "" } else { " not" },
                            field.name,
                            if on_implementation { "" } else { " not" },
                        ),
                    });
                }
            }
        }
    }

    /// Internal fields have no semantic equivalent in other source schemas, so they cannot be part
    /// of a `@key` (spec §2 "@internal").
    fn validate_internal_key_fields(&mut self) {
        for (type_name, ty) in &self.schema.types {
            let Some(fields) = fields_of(ty) else {
                continue;
            };
            let internal_fields: Vec<Name> = fields
                .filter(|f| f.directives.has(&self.names.internal))
                .map(|f| f.name.clone())
                .collect();
            if internal_fields.is_empty() {
                continue;
            }
            for key in ty.directives().get_all(&self.names.key) {
                let Some(fields) = key
                    .specified_argument_by_name("fields")
                    .and_then(|v| v.as_str())
                else {
                    continue;
                };
                for internal in &internal_fields {
                    if fields
                        .split(|c: char| !(c == '_' || c.is_ascii_alphanumeric()))
                        .any(|token| token == internal.as_str())
                    {
                        self.error(SingleFederationError::KeyInvalidFields {
                            target_type: type_name.clone(),
                            application: format!("@key(fields: \"{fields}\")"),
                            message: format!(
                                "field \"{type_name}.{internal}\" is @internal and cannot be part \
                                 of a key"
                            ),
                        });
                    }
                }
            }
        }
    }
}
