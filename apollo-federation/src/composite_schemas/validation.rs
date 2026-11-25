//! Source-schema validation of the GraphQL Federation directives (spec §4 "Validate Source
//! Schemas"), plus the Apollo-specific restrictions on federation directives in source schemas.
//!
//! Rules that validate selection maps against other source schemas (`IS_INVALID_FIELDS`,
//! `REQUIRE_INVALID_FIELDS`) live in [`super::cross_schema`].

use apollo_compiler::Name;
use apollo_compiler::Node;
use apollo_compiler::Schema;
use apollo_compiler::ast;
use apollo_compiler::ast::FieldDefinition;
use apollo_compiler::collections::IndexMap;
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
use crate::schema::field_selection_map::value::Unmappable;
use crate::schema::field_selection_map::value::check_mappable;
use crate::supergraph::CompositionHint;
use crate::supergraph::FEDERATION_ENTITIES_FIELD_NAME;
use crate::supergraph::FEDERATION_ENTITY_TYPE_NAME;
use crate::supergraph::FEDERATION_SERVICE_FIELD_NAME;

/// Where a `FieldSelectionMap` directive is applied: `Type.field(argument:)`.
pub(crate) struct ArgumentCoordinate<'a> {
    pub(crate) type_name: &'a Name,
    pub(crate) field: &'a FieldDefinition,
    pub(crate) argument: &'a Node<ast::InputValueDefinition>,
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
pub(crate) fn fields_of(ty: &ExtendedType) -> Option<&IndexMap<Name, Node<FieldDefinition>>> {
    match ty {
        ExtendedType::Object(object) => Some(&object.fields),
        ExtendedType::Interface(interface) => Some(&interface.fields),
        _ => None,
    }
}

/// Whether `field_name` is one of the query root fields federation adds to every subgraph
/// (`_service`, `_entities`), which source-schema rules about the user's fields do not apply to.
pub(crate) fn is_federation_root_field(field_name: &Name) -> bool {
    *field_name == FEDERATION_SERVICE_FIELD_NAME || *field_name == FEDERATION_ENTITIES_FIELD_NAME
}

/// Whether `field` is a lookup path field: it has no arguments and returns a single (non-list)
/// object type (spec §4 `LOOKUP_NOT_REACHABLE`). The executor needs a single object of a known
/// concrete type to call a nested lookup on, so lists, interfaces and unions end the path.
fn is_lookup_path_field(schema: &Schema, field: &FieldDefinition) -> bool {
    field.arguments.is_empty()
        && !field.ty.is_list()
        && matches!(
            schema.types.get(field.ty.inner_named_type()),
            Some(ExtendedType::Object(_))
        )
}

/// The lookup path types: the query root type and the types reachable from it through lookup path
/// fields. Lookup fields may only be declared on these types (spec §4 `LOOKUP_NOT_REACHABLE`).
pub(crate) fn lookup_reachable_types(schema: &Schema) -> IndexSet<Name> {
    let mut reachable = IndexSet::default();
    let Some(query) = schema.schema_definition.query.as_ref() else {
        return reachable;
    };
    let mut queue = vec![Name::clone(query)];
    while let Some(type_name) = queue.pop() {
        if !reachable.insert(type_name.clone()) {
            continue;
        }
        let Some(fields) = schema.types.get(&type_name).and_then(fields_of) else {
            continue;
        };
        for field in fields.values() {
            let target = field.ty.inner_named_type();
            if is_lookup_path_field(schema, field) && !reachable.contains(target) {
                queue.push(target.clone());
            }
        }
    }
    reachable
}

struct SourceSchemaValidator<'a> {
    schema: &'a Schema,
    names: &'a CompositeNames,
    /// Whether the schema applies `@lookup`. Only source schemas are barred from `@requires`,
    /// `@key(resolvable:)` and `@context`/`@fromContext`.
    is_source_schema: bool,
    errors: MultipleFederationErrors,
    hints: Vec<CompositionHint>,
}

/// Validate the GraphQL Federation directives of a subgraph that applies any of them, and, for a
/// source schema (one that applies `@lookup`), the federation directives it cannot use. Returns
/// the hints (warnings) raised; errors are returned as a `FederationError`. Subgraphs that apply
/// none of these directives are not affected.
pub(crate) fn validate_source_schema(
    schema: &ValidFederationSchema,
) -> Result<Vec<CompositionHint>, FederationError> {
    let Some(metadata) = schema.subgraph_metadata() else {
        return Ok(Vec::new());
    };
    if !metadata.uses_composite_directives() {
        return Ok(Vec::new());
    }
    let Some(names) = CompositeNames::new(schema, metadata.federation_spec_definition()) else {
        return Ok(Vec::new());
    };
    let mut validator = SourceSchemaValidator {
        schema: schema.schema(),
        names: &names,
        is_source_schema: metadata.is_composite_schema(),
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
            // Both object and interface types can implement interfaces.
            let interface_type_names: Vec<Name> = match ty {
                ExtendedType::Object(object) => &object.implements_interfaces,
                ExtendedType::Interface(interface) => &interface.implements_interfaces,
                _ => continue,
            }
            .iter()
            .map(|i| Name::clone(i))
            .collect();
            for field in fields.values() {
                self.validate_field(type_name, field, &reachable);
                // Internal types are local to their source schema, so only internal elements of
                // this same schema can reference them.
                let field_is_internal =
                    type_is_internal || field.directives.has(&self.names.internal);
                self.validate_require_consistency(type_name, field, &interface_type_names);
                if field_is_internal && field.directives.has(&self.names.override_) {
                    self.error(SingleFederationError::InternalOverrideCollision {
                        message: format!(
                            "The field \"{type_name}.{}\" is @internal, directly or through its \
                             type, and cannot also use @{}.",
                            field.name, self.names.override_
                        ),
                    });
                }
                if field_is_internal {
                    continue;
                }
                // `REFERENCE_TO_INTERNAL_TYPE`: an internal type (annotated, or with only internal
                // fields) is not part of the composite schema, so a public field cannot return it.
                // Root operation types always are.
                let return_type = field.ty.inner_named_type();
                let is_root = self
                    .schema
                    .schema_definition
                    .iter_root_operations()
                    .any(|(_, root)| **root == *return_type);
                if !is_root && self.is_internal_type(&internal_types, return_type) {
                    let internal_type = if internal_types.contains(return_type) {
                        format!("the @internal type \"{return_type}\"")
                    } else {
                        format!("the type \"{return_type}\", whose fields are all @internal")
                    };
                    self.error(SingleFederationError::ReferenceToInternalType {
                        message: format!(
                            "The field \"{type_name}.{}\" references {internal_type}; internal \
                             types are local to their source schema, so only internal fields may \
                             reference them.",
                            field.name
                        ),
                    });
                }
                // `INTERNAL_INPUT_TYPE_REFERENCED_PUBLICLY`: `@require` arguments are supplied by
                // the executor and removed from the composite schema, so they may use internal
                // input types.
                for argument in &field.arguments {
                    let argument_type = argument.ty.inner_named_type();
                    if internal_types.contains(argument_type)
                        && !argument.directives.has(&self.names.require)
                    {
                        let argument_coordinate = ArgumentCoordinate {
                            type_name,
                            field,
                            argument,
                        };
                        self.error(SingleFederationError::InternalInputTypeReferencedPublicly {
                            message: format!(
                                "The argument \"{argument_coordinate}\" references the @internal \
                                 input type \"{argument_type}\"; only @require arguments and \
                                 arguments of internal fields may reference internal input types."
                            ),
                        });
                    }
                }
            }
        }
        self.validate_internal_union_members(&internal_types);
        self.validate_internal_input_fields(&internal_types);
        self.validate_internal_usage(&internal_types, &reachable);
    }

    /// `REFERENCE_TO_INTERNAL_TYPE`: a union cannot include an internal type (annotated, or with
    /// only internal fields), which is not merged.
    fn validate_internal_union_members(&mut self, internal_types: &IndexSet<Name>) {
        for (type_name, ty) in &self.schema.types {
            let ExtendedType::Union(union_) = ty else {
                continue;
            };
            // Federation's own `_Entity` union (in a subgraph without `@lookup`) lists every
            // entity; an internal entity type is reported by `INTERNAL_INVALID_USAGE` instead.
            if *type_name == FEDERATION_ENTITY_TYPE_NAME {
                continue;
            }
            for member in &union_.members {
                if !self.is_internal_type(internal_types, member) {
                    continue;
                }
                let internal_type = if internal_types.contains(&**member) {
                    format!("the @internal type \"{}\"", member.as_str())
                } else {
                    format!(
                        "the type \"{}\", whose fields are all @internal",
                        member.as_str()
                    )
                };
                self.error(SingleFederationError::ReferenceToInternalType {
                    message: format!(
                        "The union \"{type_name}\" includes {internal_type}; internal types are \
                         local to their source schema and cannot be union members."
                    ),
                });
            }
        }
    }

    /// `INTERNAL_INPUT_TYPE_REFERENCED_PUBLICLY`: a public input object type cannot have fields
    /// of an internal input type, which does not exist in the composite schema.
    fn validate_internal_input_fields(&mut self, internal_types: &IndexSet<Name>) {
        for (type_name, ty) in &self.schema.types {
            let ExtendedType::InputObject(input) = ty else {
                continue;
            };
            if internal_types.contains(type_name) {
                continue;
            }
            for field in input.fields.values() {
                let field_type = field.ty.inner_named_type();
                if internal_types.contains(field_type) {
                    self.error(SingleFederationError::InternalInputTypeReferencedPublicly {
                        message: format!(
                            "The input field \"{type_name}.{}\" references the @internal input \
                             type \"{field_type}\"; only fields of other internal input types may \
                             reference internal input types.",
                            field.name
                        ),
                    });
                }
            }
        }
    }

    fn validate_type_level_directives(&mut self, type_name: &Name, ty: &ExtendedType) {
        if !self.is_source_schema {
            return;
        }
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
                         a GraphQL Federation source schema (a subgraph that applies @lookup) \
                         cannot use: a source schema resolves entities through its @lookup fields."
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
        if self.is_source_schema
            && let Some(requires) = &self.names.requires
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
        // `@external` on a type applies to every field of the type.
        let field_is_external = field.directives.has(&self.names.external)
            || self
                .schema
                .types
                .get(type_name)
                .is_some_and(|ty| ty.directives().has(&self.names.external));
        let is_lookup = field.directives.has(&self.names.lookup);
        if is_lookup {
            self.validate_lookup(type_name, field, reachable);
        }
        // The key each lookup argument maps to: its parsed `@is` map, or its own name.
        let mut key_maps: Vec<(&Node<ast::InputValueDefinition>, SelectedValue)> = Vec::new();
        for argument in &field.arguments {
            let argument_coordinate = ArgumentCoordinate {
                type_name,
                field,
                argument,
            };
            if self.is_source_schema
                && let Some(from_context) = &self.names.from_context
                && argument.directives.has(from_context)
            {
                self.error(SingleFederationError::ContextInSourceSchemaUnsupported {
                    message: format!(
                        "The @{from_context} directive on \"{argument_coordinate}\" is not yet \
                         supported in a GraphQL Federation source schema."
                    ),
                });
            }
            let key_map = if let Some(is) = argument.directives.get(&self.names.is) {
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
                        Some(map)
                    }
                    Err(error) => {
                        self.error(error);
                        None
                    }
                }
            } else {
                Some(SelectedValue::field(argument.name.clone()))
            };
            if let Some(require) = argument.directives.get(&self.names.require) {
                if field_is_external {
                    self.error(SingleFederationError::ExternalRequireCollision {
                        message: format!(
                            "The @{} directive on \"{argument_coordinate}\" is not allowed: \
                             the field \"{coordinate}\" is @{}, directly or through its type.",
                            self.names.require, self.names.external
                        ),
                    });
                }
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
            } else if is_lookup && let Some(key_map) = key_map {
                // A requirement is not part of the key (and is rejected on lookups anyway).
                key_maps.push((argument, key_map));
            }
        }
        if is_lookup {
            self.validate_lookup_keys(type_name, field, &key_maps);
        }
    }

    /// The rules on a `@lookup` field itself; its keys are checked by
    /// [`Self::validate_lookup_keys`].
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
        // An interface is never a lookup path type, but "not reachable" would not tell the author
        // that the directive itself is misplaced.
        if matches!(
            self.schema.types.get(type_name),
            Some(ExtendedType::Interface(_))
        ) {
            self.error(SingleFederationError::LookupNotReachable {
                message: format!(
                    "The @lookup field \"{coordinate}\" is declared by the interface type \
                     \"{type_name}\", but @lookup is only valid on fields of object types. An \
                     implementing field is a lookup field only if it is annotated with @lookup \
                     itself."
                ),
            });
        } else if !reachable.contains(type_name) {
            self.error(SingleFederationError::LookupNotReachable {
                message: format!(
                    "The @lookup field \"{coordinate}\" is not reachable from the query root: a \
                     lookup must be declared on the query root type, or on a type reached from it \
                     through fields without arguments that return a single object type."
                ),
            });
        }
    }

    /// `LOOKUP_KEY_MISSING_FOR_TYPE`: every key argument of a lookup maps to fields of every
    /// possible type the lookup returns.
    fn validate_lookup_keys(
        &mut self,
        type_name: &Name,
        field: &Node<FieldDefinition>,
        key_maps: &[(&Node<ast::InputValueDefinition>, SelectedValue)],
    ) {
        let return_type = field.ty.inner_named_type();
        let concrete_types = possible_types(self.schema, return_type);
        for (argument, map) in key_maps {
            let coordinate = ArgumentCoordinate {
                type_name,
                field,
                argument,
            };
            for concrete in &concrete_types {
                let Err(unmappable) = check_mappable(map, self.schema, concrete) else {
                    continue;
                };
                let reason = match (argument.directives.has(&self.names.is), unmappable) {
                    (false, _) => {
                        format!("\"{concrete}\" has no field named \"{}\"", argument.name)
                    }
                    (true, Unmappable::NotAdmitted) => format!(
                        "no alternative of its @is selection map \"{map}\" applies to \"{concrete}\""
                    ),
                    (true, Unmappable::MissingField { type_name, field }) => format!(
                        "its @is selection map \"{map}\" selects \"{type_name}.{field}\", which is \
                         not defined"
                    ),
                };
                let target = if concrete == return_type {
                    format!("\"{concrete}\"")
                } else {
                    format!("the possible type \"{concrete}\" of \"{return_type}\"")
                };
                self.error(SingleFederationError::LookupKeyMissingForType {
                    message: format!(
                        "The argument \"{coordinate}\" of the @lookup field cannot be mapped for \
                         {target}: {reason}."
                    ),
                });
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

    /// Whether `type_name` is internal: annotated with `@internal`, or all of its fields are.
    fn is_internal_type(&self, internal_types: &IndexSet<Name>, type_name: &Name) -> bool {
        internal_types.contains(type_name)
            || self
                .schema
                .types
                .get(type_name)
                .and_then(fields_of)
                .is_some_and(|fields| {
                    fields
                        .values()
                        .all(|field| field.directives.has(&self.names.internal))
                })
    }

    /// `INTERNAL_INVALID_USAGE`: `@internal` hides lookups used only for entity resolution, so it
    /// may only be applied to `@lookup` fields and the lookup path leading to them. Applying it to
    /// a type is shorthand for applying it to each of the type's fields. Every internal field that
    /// is not a `@lookup` must be declared on a lookup path type, must itself be a lookup path
    /// field, and must return an internal type.
    ///
    /// Public fields that return internal types (annotated, or with only internal fields) are not
    /// checked here: `REFERENCE_TO_INTERNAL_TYPE` reports them.
    fn validate_internal_usage(
        &mut self,
        internal_types: &IndexSet<Name>,
        lookup_path_types: &IndexSet<Name>,
    ) {
        for (type_name, ty) in &self.schema.types {
            let Some(fields) = fields_of(ty) else {
                continue;
            };
            let type_is_internal = internal_types.contains(type_name);
            let is_lookup_path_type = lookup_path_types.contains(type_name);
            let is_query_root = self
                .schema
                .schema_definition
                .query
                .as_ref()
                .is_some_and(|query| **query == *type_name);
            let mut reported_type = false;
            for field in fields.values() {
                let field_is_internal = field.directives.has(&self.names.internal);
                if !(type_is_internal || field_is_internal)
                    || field.directives.has(&self.names.lookup)
                    || (is_query_root && is_federation_root_field(&field.name))
                {
                    continue;
                }
                let subject = if field_is_internal {
                    format!("The @internal field \"{type_name}.{}\"", field.name)
                } else {
                    format!(
                        "The field \"{type_name}.{}\" of the @internal type \"{type_name}\"",
                        field.name
                    )
                };
                if !is_lookup_path_type {
                    // Report an internal type once rather than once per field.
                    if !field_is_internal {
                        if reported_type {
                            continue;
                        }
                        reported_type = true;
                        self.error(SingleFederationError::InternalInvalidUsage {
                            message: format!(
                                "The @internal type \"{type_name}\" is not on a lookup path: only \
                                 @lookup fields and the lookup path leading to them can be \
                                 internal."
                            ),
                        });
                        continue;
                    }
                    self.error(SingleFederationError::InternalInvalidUsage {
                        message: format!(
                            "{subject} is not on a lookup path: only @lookup fields and the lookup \
                             path leading to them can be internal."
                        ),
                    });
                    continue;
                }
                if !is_lookup_path_field(self.schema, field) {
                    self.error(SingleFederationError::InternalInvalidUsage {
                        message: format!(
                            "{subject} must be a @lookup field, or a lookup path field without \
                             arguments that returns a single object type."
                        ),
                    });
                    continue;
                }
                let target = field.ty.inner_named_type();
                if !self.is_internal_type(internal_types, target) {
                    self.error(SingleFederationError::InternalInvalidUsage {
                        message: format!(
                            "{subject} returns the type \"{target}\", which must also be internal: \
                             annotate it with @internal, or annotate all of its fields with \
                             @internal."
                        ),
                    });
                }
            }
        }
    }
}
