//! Functions for output compatibility between graphql-js and apollo-rs
//!
//! apollo-rs produces different SDL than graphql-js based tools. For example, it chooses to
//! include directive applications by default where graphql-js does not support doing that
//! at all.
//!
//! This module contains functions that modify an apollo-rs schema to produce the same output as a
//! graphql-js schema would.

use apollo_compiler::ExecutableDocument;
use apollo_compiler::Name;
use apollo_compiler::Node;
use apollo_compiler::Schema;
use apollo_compiler::ast::DirectiveDefinition;
use apollo_compiler::ast::Value;
use apollo_compiler::collections::IndexMap;
use apollo_compiler::executable;
use apollo_compiler::schema;
use apollo_compiler::schema::Directive;
use apollo_compiler::schema::ExtendedType;
use apollo_compiler::schema::InputValueDefinition;
use apollo_compiler::schema::Type;
use apollo_compiler::validation::Valid;
use either::Either;

use crate::error::FederationError;
use crate::error::MultipleFederationErrors;
use crate::error::SingleFederationError;
use crate::schema::position::DirectiveDefinitionPosition;
use crate::schema::position::InputObjectFieldDefinitionPosition;
use crate::schema::position::InterfaceFieldDefinitionPosition;
use crate::schema::position::ObjectFieldDefinitionPosition;
use crate::schema::position::ObjectOrInterfaceFieldDefinitionPosition;

/// Return true if a directive application is "semantic", meaning it's observable in introspection.
fn is_semantic_directive_application(directive: &Directive) -> bool {
    match directive.name.as_str() {
        "specifiedBy" => true,
        // graphql-js’ intropection returns `isDeprecated: false` for `@deprecated(reason: null)`,
        // which is arguably a bug. Do the same here for now.
        // TODO: remove this and allow `isDeprecated: true`, `deprecatedReason: null`
        // after we fully move to Rust introspection?
        "deprecated"
            if directive
                .specified_argument_by_name("reason")
                .is_some_and(|value| value.is_null()) =>
        {
            false
        }
        "deprecated" => true,
        _ => false,
    }
}

/// Remove `reason` argument from a `@deprecated` directive if it has the default value, just to match graphql-js output.
fn standardize_deprecated(directive: &mut Directive) {
    if directive.name == "deprecated"
        && directive
            .specified_argument_by_name("reason")
            .and_then(|value| value.as_str())
            .is_some_and(|reason| reason == "No longer supported")
    {
        directive.arguments.clear();
    }
}

/// Retain only semantic directives in a directive list from the high-level schema representation.
fn retain_semantic_directives(directives: &mut schema::DirectiveList) {
    directives
        .0
        .retain(|directive| is_semantic_directive_application(directive));

    for directive in directives {
        standardize_deprecated(directive.make_mut());
    }
}

/// Retain only semantic directives in a directive list from the AST-level schema representation.
fn retain_semantic_directives_ast(directives: &mut apollo_compiler::ast::DirectiveList) {
    directives
        .0
        .retain(|directive| is_semantic_directive_application(directive));

    for directive in directives {
        standardize_deprecated(directive.make_mut());
    }
}

/// Remove non-semantic directive applications from the schema representation.
/// This only keeps directive applications that are observable in introspection.
pub(crate) fn remove_non_semantic_directives(schema: &mut Schema) {
    let root_definitions = schema.schema_definition.make_mut();
    retain_semantic_directives(&mut root_definitions.directives);

    for ty in schema.types.values_mut() {
        match ty {
            ExtendedType::Object(object) => {
                let object = object.make_mut();
                retain_semantic_directives(&mut object.directives);
                for field in object.fields.values_mut() {
                    let field = field.make_mut();
                    retain_semantic_directives_ast(&mut field.directives);
                    for arg in &mut field.arguments {
                        let arg = arg.make_mut();
                        retain_semantic_directives_ast(&mut arg.directives);
                    }
                }
            }
            ExtendedType::Interface(interface) => {
                let interface = interface.make_mut();
                retain_semantic_directives(&mut interface.directives);
                for field in interface.fields.values_mut() {
                    let field = field.make_mut();
                    retain_semantic_directives_ast(&mut field.directives);
                    for arg in &mut field.arguments {
                        let arg = arg.make_mut();
                        retain_semantic_directives_ast(&mut arg.directives);
                    }
                }
            }
            ExtendedType::InputObject(input_object) => {
                let input_object = input_object.make_mut();
                retain_semantic_directives(&mut input_object.directives);
                for field in input_object.fields.values_mut() {
                    let field = field.make_mut();
                    retain_semantic_directives_ast(&mut field.directives);
                }
            }
            ExtendedType::Union(union_) => {
                let union_ = union_.make_mut();
                retain_semantic_directives(&mut union_.directives);
            }
            ExtendedType::Scalar(scalar) => {
                let scalar = scalar.make_mut();
                retain_semantic_directives(&mut scalar.directives);
            }
            ExtendedType::Enum(enum_) => {
                let enum_ = enum_.make_mut();
                retain_semantic_directives(&mut enum_.directives);
                for value in enum_.values.values_mut() {
                    let value = value.make_mut();
                    retain_semantic_directives_ast(&mut value.directives);
                }
            }
        }
    }

    for directive in schema.directive_definitions.values_mut() {
        let directive = directive.make_mut();
        for arg in &mut directive.arguments {
            let arg = arg.make_mut();
            retain_semantic_directives_ast(&mut arg.directives);
        }
    }
}

// Just a boolean with a `?` operator
type CoerceResult = Result<(), ()>;

#[derive(Default)]
struct CoerceOptions {
    coerce_non_list_values_for_list_types: bool,
}

/// This function, when passed a GraphQL value from a schema or operation, will:
/// 1. Coerce enum values for the String type and string values for enum types. This is not a
///    GraphQL spec coercion rule, but instead for backwards compatibility with JS logic, which has
///    schema and operation representations that effectively did the same coercions.
/// 2. Optionally coerce non-list values for list types. This is technically a GraphQL spec coercion
///    rule, but really we're doing it here for backward compatibility with JS logic, which would do
///    this coercion for its operation and selection set representations.
/// 3. Partially validate that the value is correct for its type. We purposefully do not validate
///    custom scalars (since router can't know the validation rules for those) and variables (since
///    the validation logic here is only really used for default values in schemas, which can't
///    contain variables). However, for backward compatibility with JS logic, we additionally omit
///    checking that:
///    - object values have required fields for input object types
///    - integer values are within the range of a 32-bit signed integer for the Int type
///    - float values are a finite IEEE 754 value for the Float type
///    - default values don't contain cycles (this is distinct from input object cycle checks)
///
/// Note that coercion still proceeds if a validation error was found. This is to mimic JS logic,
/// which coerced before validating in separate passes. Also, we don't return error messages because
/// the JS logic similarly didn't (in the future, we'll get better messages from apollo-rs when it
/// starts validating default values).
fn coerce_value(
    types: &IndexMap<Name, ExtendedType>,
    target: &mut Node<Value>,
    ty: &Type,
    options: &CoerceOptions,
) -> CoerceResult {
    match ty {
        Type::NonNullNamed(ty) => {
            if target.is_null() {
                // A null value is invalid for a non-null type.
                return Err(());
            }
            coerce_value(types, target, &Type::Named(ty.clone()), options)
        }
        Type::NonNullList(ty) => {
            if target.is_null() {
                // A null value is invalid for a non-null type.
                return Err(());
            }
            coerce_value(types, target, &Type::List(ty.clone()), options)
        }
        Type::List(ty) => {
            if target.is_null() {
                // Null is always valid for a nullable type.
                return Ok(());
            }
            if matches!(target.as_ref(), Value::Variable(_)) {
                // As mentioned above, we don't validate variables, since this validation is only
                // used for schema default values (which don't contain variables). Note that we
                // don't have variable definitions, but the GraphQL spec validation rule
                // https://spec.graphql.org/September2025/#AreTypesCompatible()
                // guarantees that a variable's type must have the same level of list wrapping as
                // its location in a value, so there's no need to coerce either.
                return Ok(());
            }
            if matches!(target.as_ref(), Value::List(_)) {
                let Value::List(target) = target.make_mut() else {
                    // An internal error, as we just checked above that this is a list.
                    return Err(());
                };
                for element in target {
                    coerce_value(types, element, ty, options)?;
                }
            } else {
                coerce_value(types, target, ty, options)?;
                if options.coerce_non_list_values_for_list_types {
                    *target.make_mut() = Value::List(vec![target.clone()]);
                }
            }
            Ok(())
        }
        Type::Named(ty) => {
            if target.is_null() {
                // Null is always valid for a nullable type.
                return Ok(());
            }
            if matches!(target.as_ref(), Value::Variable(_)) {
                // As mentioned above, we don't validate variables, since this validation is only
                // used for schema default values (which don't contain variables). We also don't
                // need to coerce variables.
                return Ok(());
            }
            let Some(ty) = types.get(ty) else {
                // An internal error, as valid schemas shouldn't have unknown type references.
                return Err(());
            };
            match ty {
                ExtendedType::InputObject(ty) => {
                    let Value::Object(target) = target.make_mut() else {
                        // A non-object value is invalid for an input object type.
                        return Err(());
                    };
                    // In the event one of the fields is invalid, we want to keep coercing the
                    // remaining ones.
                    let mut is_error = false;
                    for (field, target) in target.iter_mut() {
                        let Some(definition) = ty.fields.get(field) else {
                            // Unknown object fields are invalid for an input object type.
                            is_error = true;
                            continue;
                        };
                        if coerce_value(types, target, &definition.ty, options).is_err() {
                            is_error = true;
                        }
                    }
                    if is_error {
                        return Err(());
                    }
                    Ok(())
                }
                ExtendedType::Scalar(ty) => {
                    if ty.is_built_in() {
                        match ty.name.as_str() {
                            "Int" => {
                                if !matches!(target.as_ref(), Value::Int(_)) {
                                    // A non-integer value is invalid for the Int type.
                                    return Err(());
                                }
                                // As mentioned above, we don't validate the integer value is within
                                // the range of a 32-bit signed integer for backward compatibility
                                // with JS logic.
                            }
                            "Float" => {
                                if !matches!(target.as_ref(), Value::Int(_) | Value::Float(_)) {
                                    // A non-integer, non-float value is invalid for the Float type.
                                    return Err(());
                                }
                                // As mentioned above, we don't validate the float value is a finite
                                // IEEE 754 value for backward compatibility with JS logic.
                            }
                            "String" => {
                                // As mentioned above, we coerce enum values for the String type for
                                // backward compatibility with JS logic.
                                if let Value::Enum(name) = target.as_ref() {
                                    *target.make_mut() = Value::String(name.to_string());
                                }
                                if !matches!(target.as_ref(), Value::String(_)) {
                                    // A non-string value is invalid for the String type (but note
                                    // the exception above).
                                    return Err(());
                                }
                            }
                            "Boolean" => {
                                if !matches!(target.as_ref(), Value::Boolean(_)) {
                                    // A non-boolean value is invalid for the Boolean type.
                                    return Err(());
                                }
                            }
                            "ID" => {
                                if !matches!(target.as_ref(), Value::Int(_) | Value::String(_)) {
                                    // A non-integer, non-string value is invalid for the ID type.
                                    return Err(());
                                }
                            }
                            _ => {
                                // An internal error, as GraphQL only has the above five built-in
                                // scalar types.
                                return Err(());
                            }
                        }
                    }
                    // As mentioned above, we don't validate custom scalars, since we can't know the
                    // validation rules for them.
                    Ok(())
                }
                ExtendedType::Enum(ty) => {
                    // As mentioned above, we coerce string values for enum types for backward
                    // compatibility with JS logic.
                    if let Value::String(name) = target.as_ref() {
                        let Ok(name) = Name::new(name) else {
                            // A non-GraphQL-name value is invalid for enum types.
                            return Err(());
                        };
                        *target.make_mut() = Value::Enum(name);
                    }
                    let Value::Enum(name) = target.as_ref() else {
                        // A non-enum value is invalid for enum types (but note the exception
                        // above).
                        return Err(());
                    };
                    if !ty.values.contains_key(name) {
                        // An unknown enum value is invalid for enum types.
                        return Err(());
                    }
                    Ok(())
                }
                ExtendedType::Object(_) | ExtendedType::Interface(_) | ExtendedType::Union(_) => {
                    // An internal error, as valid schemas shouldn't use non-input types for values.
                    Err(())
                }
            }
        }
    }
}

/// Coerce default values in all the given arguments, mutating the arguments. Invalid default values
/// generate an error (but note that `coerce_value()` only does partial validation).
fn coerce_and_validate_arguments_default_values(
    types: &IndexMap<Name, ExtendedType>,
    arguments: &mut Vec<Node<InputValueDefinition>>,
    position: &Either<ObjectOrInterfaceFieldDefinitionPosition, DirectiveDefinitionPosition>,
    errors: &mut Vec<SingleFederationError>,
) {
    for arg in arguments {
        let name = arg.name.clone();
        let coordinate = match &position {
            Either::Left(ObjectOrInterfaceFieldDefinitionPosition::Object(position)) => {
                position.argument(name).to_string()
            }
            Either::Left(ObjectOrInterfaceFieldDefinitionPosition::Interface(position)) => {
                position.argument(name).to_string()
            }
            Either::Right(position) => position.argument(name).to_string(),
        };
        let arg = arg.make_mut();
        if arg.is_required() && arg.directives.has("deprecated") {
            errors.push(SingleFederationError::InvalidGraphQL {
                message: format!("Required argument {} cannot be deprecated.", coordinate,),
            })
        }
        let Some(default_value) = &mut arg.default_value else {
            continue;
        };
        if coerce_value(types, default_value, &arg.ty, &Default::default()).is_err() {
            errors.push(SingleFederationError::InvalidGraphQL {
                message: format!(
                    "Invalid default value (got: {}) provided for argument {} of type {}.",
                    default_value.serialize().no_indent(),
                    coordinate,
                    arg.ty
                ),
            })
        }
    }
}

/// This function, for a schema, will do the following for any GraphQL values in that schema:
/// 1. Coerce enum values for the String type and string values for enum types. This is not a
///    GraphQL spec coercion rule, but instead for backwards compatibility with JS logic, which has
///    schema and operation representations that effectively did the same coercions.
/// 2. For default values specifically, partially validate that the value is correct for its type.
///    We purposefully do not validate custom scalars (since router can't know the validation rules
///    for those) and variables (since schemas can't contain variables). However, for backward
///    compatibility with JS logic, we additionally omit checking that:
///    - object values have required fields for input object types
///    - integer values are within the range of a 32-bit signed integer for the Int type
///    - float values are a finite IEEE 754 value for the Float type
///    - default values don't contain cycles (this is distinct from input object cycle checks)
/// 3. Validate that required arguments/input fields are not marked @deprecated (apollo-rs does not
///    do this, but it's a GraphQL spec validation rule and federation JS logic does this).
///
/// Note that coercion still proceeds if a validation error was found. This is to mimic JS logic,
/// which coerced before validating in separate passes.
pub fn coerce_and_validate_schema_values(schema: &mut Schema) -> Result<(), FederationError> {
    let mut errors: Vec<SingleFederationError> = Default::default();

    // Keep a copy of the schema types/directives so we can mutate the schema while walking it.
    let types = schema.types.clone();
    let directive_definitions = schema.directive_definitions.clone();

    for ty in schema.types.values_mut() {
        match ty {
            ExtendedType::Object(object) => {
                let object = object.make_mut();
                coerce_directive_application_values_schema(
                    &directive_definitions,
                    &types,
                    &mut object.directives,
                );
                for field in object.fields.values_mut() {
                    let field = field.make_mut();
                    coerce_and_validate_arguments_default_values(
                        &types,
                        &mut field.arguments,
                        &Either::Left(
                            ObjectFieldDefinitionPosition {
                                type_name: object.name.clone(),
                                field_name: field.name.clone(),
                            }
                            .into(),
                        ),
                        &mut errors,
                    );
                    coerce_directive_application_values_ast(
                        &directive_definitions,
                        &types,
                        &mut field.directives,
                    );
                    coerce_argument_directive_application_values(
                        &directive_definitions,
                        &types,
                        &mut field.arguments,
                    );
                }
            }
            ExtendedType::Interface(interface) => {
                let interface = interface.make_mut();
                coerce_directive_application_values_schema(
                    &directive_definitions,
                    &types,
                    &mut interface.directives,
                );
                for field in interface.fields.values_mut() {
                    let field = field.make_mut();
                    coerce_and_validate_arguments_default_values(
                        &types,
                        &mut field.arguments,
                        &Either::Left(
                            InterfaceFieldDefinitionPosition {
                                type_name: interface.name.clone(),
                                field_name: field.name.clone(),
                            }
                            .into(),
                        ),
                        &mut errors,
                    );
                    coerce_directive_application_values_ast(
                        &directive_definitions,
                        &types,
                        &mut field.directives,
                    );
                    coerce_argument_directive_application_values(
                        &directive_definitions,
                        &types,
                        &mut field.arguments,
                    );
                }
            }
            ExtendedType::InputObject(input_object) => {
                let input_object = input_object.make_mut();
                coerce_directive_application_values_schema(
                    &directive_definitions,
                    &types,
                    &mut input_object.directives,
                );
                for field in input_object.fields.values_mut() {
                    let field = field.make_mut();
                    let coordinate = InputObjectFieldDefinitionPosition {
                        type_name: input_object.name.clone(),
                        field_name: field.name.clone(),
                    }
                    .to_string();
                    coerce_directive_application_values_ast(
                        &directive_definitions,
                        &types,
                        &mut field.directives,
                    );
                    if field.is_required() && field.directives.has("deprecated") {
                        errors.push(SingleFederationError::InvalidGraphQL {
                            message: format!(
                                "Required argument {} cannot be deprecated.",
                                coordinate,
                            ),
                        })
                    }
                    let Some(default_value) = &mut field.default_value else {
                        continue;
                    };
                    if coerce_value(&types, default_value, &field.ty, &Default::default()).is_err()
                    {
                        errors.push(SingleFederationError::InvalidGraphQL {
                            message: format!(
                                "Invalid default value (got: {}) provided for input field {} of type {}.",
                                default_value.serialize().no_indent(),
                                coordinate,
                                field.ty
                            )
                        })
                    }
                }
            }
            ExtendedType::Union(union_) => {
                let union_ = union_.make_mut();
                coerce_directive_application_values_schema(
                    &directive_definitions,
                    &types,
                    &mut union_.directives,
                );
            }
            ExtendedType::Scalar(scalar) => {
                let scalar = scalar.make_mut();
                coerce_directive_application_values_schema(
                    &directive_definitions,
                    &types,
                    &mut scalar.directives,
                );
            }
            ExtendedType::Enum(enum_) => {
                let enum_ = enum_.make_mut();
                coerce_directive_application_values_schema(
                    &directive_definitions,
                    &types,
                    &mut enum_.directives,
                );
                for value in enum_.values.values_mut() {
                    let value = value.make_mut();
                    coerce_directive_application_values_ast(
                        &directive_definitions,
                        &types,
                        &mut value.directives,
                    );
                }
            }
        }
    }

    for directive in schema.directive_definitions.values_mut() {
        let directive = directive.make_mut();
        coerce_and_validate_arguments_default_values(
            &types,
            &mut directive.arguments,
            &Either::Right(DirectiveDefinitionPosition {
                directive_name: directive.name.clone(),
            }),
            &mut errors,
        );
        coerce_argument_directive_application_values(
            &directive_definitions,
            &types,
            &mut directive.arguments,
        );
    }

    if !errors.is_empty() {
        return Err(MultipleFederationErrors { errors }.into());
    }

    Ok(())
}

fn coerce_directive_application_values(
    schema: &Valid<Schema>,
    directives: &mut executable::DirectiveList,
) {
    for directive in directives {
        let Some(definition) = schema.directive_definitions.get(&directive.name) else {
            continue;
        };
        let directive = directive.make_mut();
        for arg in &mut directive.arguments {
            let Some(definition) = definition.argument_by_name(&arg.name) else {
                continue;
            };
            let arg = arg.make_mut();
            // Note that GraphQL spec validation will catch invalidities in directive application
            // argument values but with nicer error messaging, so if coerce_value() fails validation
            // here we just ignore it.
            _ = coerce_value(
                &schema.types,
                &mut arg.value,
                &definition.ty,
                &CoerceOptions {
                    coerce_non_list_values_for_list_types: true,
                },
            );
        }
    }
}

fn coerce_directive_application_values_schema(
    directive_definitions: &IndexMap<Name, Node<DirectiveDefinition>>,
    type_definitions: &IndexMap<Name, ExtendedType>,
    directives: &mut schema::DirectiveList,
) {
    for directive in directives {
        let Some(definition) = directive_definitions.get(&directive.name) else {
            continue;
        };
        let directive = directive.make_mut();
        for arg in &mut directive.arguments {
            let Some(definition) = definition.argument_by_name(&arg.name) else {
                continue;
            };
            let arg = arg.make_mut();
            // Note that GraphQL spec validation will catch invalidities in directive application
            // argument values but with nicer error messaging, so if coerce_value() fails validation
            // here we just ignore it.
            _ = coerce_value(
                type_definitions,
                &mut arg.value,
                &definition.ty,
                &Default::default(),
            );
        }
    }
}

fn coerce_directive_application_values_ast(
    directive_definitions: &IndexMap<Name, Node<DirectiveDefinition>>,
    type_definitions: &IndexMap<Name, ExtendedType>,
    directives: &mut apollo_compiler::ast::DirectiveList,
) {
    for directive in directives {
        let Some(definition) = directive_definitions.get(&directive.name) else {
            continue;
        };
        let directive = directive.make_mut();
        for arg in &mut directive.arguments {
            let Some(definition) = definition.argument_by_name(&arg.name) else {
                continue;
            };
            let arg = arg.make_mut();
            // Note that GraphQL spec validation will catch invalidities in directive application
            // argument values but with nicer error messaging, so if coerce_value() fails validation
            // here we just ignore it.
            _ = coerce_value(
                type_definitions,
                &mut arg.value,
                &definition.ty,
                &Default::default(),
            );
        }
    }
}

/// Coerce the values of directives applied to field arguments (the `ARGUMENT_DEFINITION` location).
/// `coerce_schema_values` already handles directives on types, fields, input fields and enum
/// values; arguments are handled here so e.g. an enum-typed directive argument given as a string
/// literal is coerced to the enum value (matching graphql-js leniency) instead of failing later
/// schema validation.
fn coerce_argument_directive_application_values(
    directive_definitions: &IndexMap<Name, Node<DirectiveDefinition>>,
    type_definitions: &IndexMap<Name, ExtendedType>,
    arguments: &mut [Node<InputValueDefinition>],
) {
    for arg in arguments {
        let arg = arg.make_mut();
        coerce_directive_application_values_ast(
            directive_definitions,
            type_definitions,
            &mut arg.directives,
        );
    }
}

fn coerce_selection_set_values(
    schema: &Valid<Schema>,
    selection_set: &mut executable::SelectionSet,
) {
    for selection in &mut selection_set.selections {
        match selection {
            executable::Selection::Field(field) => {
                let definition = field.definition.clone(); // Clone so we can mutate `field`.
                let field = field.make_mut();
                for arg in &mut field.arguments {
                    let Some(definition) = definition.argument_by_name(&arg.name) else {
                        continue;
                    };
                    // Note that GraphQL spec validation will catch invalidities in field argument
                    // values but with nicer error messaging, so if coerce_value() fails validation
                    // here we just ignore it.
                    let arg = arg.make_mut();
                    _ = coerce_value(
                        &schema.types,
                        &mut arg.value,
                        &definition.ty,
                        &CoerceOptions {
                            coerce_non_list_values_for_list_types: true,
                        },
                    );
                }
                coerce_directive_application_values(schema, &mut field.directives);
                coerce_selection_set_values(schema, &mut field.selection_set);
            }
            executable::Selection::FragmentSpread(frag) => {
                let frag = frag.make_mut();
                coerce_directive_application_values(schema, &mut frag.directives);
            }
            executable::Selection::InlineFragment(frag) => {
                let frag = frag.make_mut();
                coerce_directive_application_values(schema, &mut frag.directives);
                coerce_selection_set_values(schema, &mut frag.selection_set);
            }
        }
    }
}

fn coerce_operation_values(schema: &Valid<Schema>, operation: &mut Node<executable::Operation>) {
    let operation = operation.make_mut();

    for variable in &mut operation.variables {
        let variable = variable.make_mut();
        let Some(default_value) = &mut variable.default_value else {
            continue;
        };

        // Note that GraphQL spec validation will catch invalidities in variable definition default
        // values but with nicer error messaging, so if coerce_value() fails validation here we just
        // ignore it.
        _ = coerce_value(
            &schema.types,
            default_value,
            &variable.ty,
            &CoerceOptions {
                coerce_non_list_values_for_list_types: true,
            },
        );
    }

    coerce_selection_set_values(schema, &mut operation.selection_set);
}

pub fn coerce_executable_values(schema: &Valid<Schema>, document: &mut ExecutableDocument) {
    if let Some(operation) = &mut document.operations.anonymous {
        coerce_operation_values(schema, operation);
    }
    for operation in document.operations.named.values_mut() {
        coerce_operation_values(schema, operation);
    }
    for fragment in document.fragments.values_mut() {
        let fragment = fragment.make_mut();
        coerce_directive_application_values(schema, &mut fragment.directives);
        coerce_selection_set_values(schema, &mut fragment.selection_set);
    }
}

/// Removes non-semantic directives so that the apollo-rs serialized output of the schema matches
/// the result of JS logic's `printSchema()`.
///
/// Note this has different behavior than graphql-js's `printSchema()`, as `buildSchema()` in
/// graphql-js will coerce default values (since the purpose of that schema representation is to
/// execute operations, so it pre-coerces them). Similarly, introspection in graphql-js will return
/// the coerced default values from `buildSchema()`, but the GraphQL spec doesn't say to coerce (see
/// https://spec.graphql.org/September2025/#sec-The-__InputValue-Type for more information).
pub(crate) fn make_print_schema_compatible(schema: &mut Schema) {
    remove_non_semantic_directives(schema);
}

#[cfg(test)]
mod tests {
    use apollo_compiler::ExecutableDocument;
    use apollo_compiler::Schema;
    use apollo_compiler::validation::Valid;

    use super::coerce_executable_values;

    fn parse_and_coerce(schema: &Valid<Schema>, input: &str) -> String {
        let mut document = ExecutableDocument::parse(schema, input, "test.graphql").unwrap();
        coerce_executable_values(schema, &mut document);
        document.to_string()
    }

    #[test]
    fn coerces_list_values() {
        let schema = Schema::parse_and_validate(
            r#"
        type Query {
          test(
            bools: [Boolean],
            ints: [Int],
            strings: [String],
            floats: [Float],
          ): Int
        }
        "#,
            "schema.graphql",
        )
        .unwrap();

        insta::assert_snapshot!(parse_and_coerce(&schema, r#"
        {
          test(bools: true, ints: 1, strings: "string", floats: 2.0)
        }
        "#), @r#"
        {
          test(bools: [true], ints: [1], strings: ["string"], floats: [2.0])
        }
        "#);
    }

    #[test]
    fn coerces_enum_values() {
        let schema = Schema::parse_and_validate(
            r#"
        scalar CustomScalar
        type Query {
          test(
            string: String!,
            strings: [String!]!,
            custom: CustomScalar!,
            customList: [CustomScalar!]!,
          ): Int
        }
        "#,
            "schema.graphql",
        )
        .unwrap();

        // Enum literals are only coerced into lists if the item type is a custom scalar type.
        insta::assert_snapshot!(parse_and_coerce(&schema, r#"
        {
          test(string: enumVal1, strings: enumVal2, custom: enumVal1, customList: enumVal2)
        }
        "#), @r#"
        {
          test(string: "enumVal1", strings: ["enumVal2"], custom: enumVal1, customList: [enumVal2])
        }
        "#);
    }

    #[test]
    fn coerces_enum_string_in_field_argument_directive() {
        // Regression: a directive applied to a *field argument* (ARGUMENT_DEFINITION) whose
        // argument is enum-typed but given a string literal must be coerced to the enum value,
        // just like directives on fields/enum-values. Previously `coerce_schema_values` skipped
        // field-argument directives, so such a string survived to fail later schema validation.
        let mut schema = Schema::parse(
            r#"
            directive @d(x: E!) on ARGUMENT_DEFINITION
            enum E { A B }
            type Query { f(a: Int @d(x: "A")): Int }
            "#,
            "schema.graphql",
        )
        .expect("valid subgraph schema");

        super::coerce_and_validate_schema_values(&mut schema).unwrap();

        let sdl = schema.to_string();
        insta::assert_snapshot!(sdl, @"
        directive @d(x: E!) on ARGUMENT_DEFINITION

        enum E {
          A
          B
        }

        type Query {
          f(
            a: Int @d(x: A),
          ): Int
        }
        ");
    }

    #[test]
    fn coerces_in_fragment_definitions() {
        let schema = Schema::parse_and_validate(
            r#"
        type T {
            get(bools: [Boolean!]!): Int
        }
        type Query {
          test: T
        }
        "#,
            "schema.graphql",
        )
        .unwrap();

        insta::assert_snapshot!(parse_and_coerce(&schema, r#"
        {
          test {
            ...f
          }
        }

        fragment f on T {
            get(bools: true)
        }
        "#), @"
        {
          test {
            ...f
          }
        }

        fragment f on T {
          get(bools: [true])
        }
        ");
    }
}
