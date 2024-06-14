//! Functions for output compatibility between graphql-js and apollo-rs
//!
//! apollo-rs produces different SDL than graphql-js based tools. For example, it chooses to
//! include directive applications by default where graphql-js does not support doing that
//! at all.
//!
//! This module contains functions that modify an apollo-rs schema to produce the same output as a
//! graphql-js schema would.

use apollo_compiler::ast::Value;
use apollo_compiler::schema::Directive;
use apollo_compiler::schema::ExtendedType;
use apollo_compiler::schema::InputValueDefinition;
use apollo_compiler::schema::Name;
use apollo_compiler::schema::Type;
use apollo_compiler::Node;
use apollo_compiler::Schema;
use indexmap::IndexMap;

/// Return true if a directive application is "semantic", meaning it's observable in introspection.
fn is_semantic_directive_application(directive: &Directive) -> bool {
    match directive.name.as_str() {
        "specifiedBy" => true,
        // For @deprecated, explicitly writing `reason: null` disables the directive,
        // as `null` overrides the default string value.
        "deprecated"
            if directive
                .argument_by_name("reason")
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
            .argument_by_name("reason")
            .and_then(|value| value.as_str())
            .is_some_and(|reason| reason == "No longer supported")
    {
        directive.arguments.clear();
    }
}

/// Retain only semantic directives in a directive list from the high-level schema representation.
fn retain_semantic_directives(directives: &mut apollo_compiler::schema::DirectiveList) {
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
pub fn remove_non_semantic_directives(schema: &mut Schema) {
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

/// Recursively assign default values in input object values, mutating the value.
/// If the default value is invalid, returns `Err(())`.
fn coerce_value(
    types: &IndexMap<Name, ExtendedType>,
    target: &mut Node<Value>,
    ty: &Type,
) -> CoerceResult {
    match (target.make_mut(), types.get(ty.inner_named_type())) {
        (Value::Object(object), Some(ExtendedType::InputObject(definition))) if ty.is_named() => {
            for (field_name, field_definition) in definition.fields.iter() {
                match object.iter_mut().find(|(key, _value)| key == field_name) {
                    Some((_name, value)) => {
                        coerce_value(types, value, &field_definition.ty)?;
                    }
                    None => {
                        if let Some(default_value) = &field_definition.default_value {
                            let mut value = default_value.clone();
                            // If the default value is an input object we may need to fill in
                            // its defaulted fields recursively.
                            coerce_value(types, &mut value, &field_definition.ty)?;
                            object.push((field_name.clone(), value));
                        } else if field_definition.is_required() {
                            return Err(());
                        }
                    }
                }
            }
        }
        (Value::List(list), Some(_)) if ty.is_list() => {
            for element in list {
                coerce_value(types, element, ty.item_type())?;
            }
        }
        // Coerce single values (except null) to a list.
        (
            Value::Object(_)
            | Value::String(_)
            | Value::Enum(_)
            | Value::Int(_)
            | Value::Float(_)
            | Value::Boolean(_),
            Some(_),
        ) if ty.is_list() => {
            coerce_value(types, target, ty.item_type())?;
            *target.make_mut() = Value::List(vec![target.clone()]);
        }

        // Accept null for any nullable type.
        (Value::Null, _) if !ty.is_non_null() => {}

        // Accept non-composite values if they match the type.
        (Value::String(_), Some(ExtendedType::Scalar(scalar)))
            if !scalar.is_built_in() || matches!(scalar.name.as_str(), "ID" | "String") => {}
        (Value::Boolean(_), Some(ExtendedType::Scalar(scalar)))
            if !scalar.is_built_in() || scalar.name == "Boolean" => {}
        (Value::Int(_), Some(ExtendedType::Scalar(scalar)))
            if !scalar.is_built_in() || matches!(scalar.name.as_str(), "ID" | "Int" | "Float") => {}
        (Value::Float(_), Some(ExtendedType::Scalar(scalar)))
            if !scalar.is_built_in() || scalar.name == "Float" => {}
        // Custom scalars accept any value, even objects and lists.
        (Value::Object(_), Some(ExtendedType::Scalar(scalar))) if !scalar.is_built_in() => {}
        (Value::List(_), Some(ExtendedType::Scalar(scalar))) if !scalar.is_built_in() => {}
        // Enums must match the type.
        (Value::Enum(value), Some(ExtendedType::Enum(enum_)))
            if enum_.values.contains_key(value) => {}

        // Other types are totally invalid (and should ideally be rejected by validation).
        _ => return Err(()),
    }
    Ok(())
}

/// Coerce default values in all the given arguments, mutating the arguments.
/// If a default value is invalid, the whole default value is removed silently.
fn coerce_arguments_default_values(
    types: &IndexMap<Name, ExtendedType>,
    arguments: &mut Vec<Node<InputValueDefinition>>,
) {
    for arg in arguments {
        let arg = arg.make_mut();
        let Some(default_value) = &mut arg.default_value else {
            continue;
        };

        if coerce_value(types, default_value, &arg.ty).is_err() {
            arg.default_value = None;
        }
    }
}

/// Do graphql-js-style input coercion on default values. Invalid default values are silently
/// removed from the schema.
///
/// This is not what we would want to do for coercion in a real execution scenario, but it matches
/// a behaviour in graphql-js so we can compare API schema results between federation-next and JS
/// federation. We can consider removing this when we no longer rely on JS federation.
pub fn coerce_schema_default_values(schema: &mut Schema) {
    // Keep a copy of the types in the schema so we can mutate the schema while walking it.
    let types = schema.types.clone();

    for ty in schema.types.values_mut() {
        match ty {
            ExtendedType::Object(object) => {
                let object = object.make_mut();
                for field in object.fields.values_mut() {
                    let field = field.make_mut();
                    coerce_arguments_default_values(&types, &mut field.arguments);
                }
            }
            ExtendedType::Interface(interface) => {
                let interface = interface.make_mut();
                for field in interface.fields.values_mut() {
                    let field = field.make_mut();
                    coerce_arguments_default_values(&types, &mut field.arguments);
                }
            }
            ExtendedType::InputObject(input_object) => {
                let input_object = input_object.make_mut();
                for field in input_object.fields.values_mut() {
                    let field = field.make_mut();
                    let Some(default_value) = &mut field.default_value else {
                        continue;
                    };

                    if coerce_value(&types, default_value, &field.ty).is_err() {
                        field.default_value = None;
                    }
                }
            }
            ExtendedType::Union(_) | ExtendedType::Scalar(_) | ExtendedType::Enum(_) => {
                // Nothing to do
            }
        }
    }

    for directive in schema.directive_definitions.values_mut() {
        let directive = directive.make_mut();
        coerce_arguments_default_values(&types, &mut directive.arguments);
    }
}

/// Applies default value coercion and removes non-semantic directives so that
/// the apollo-rs serialized output of the schema matches the result of
/// `printSchema(buildSchema()` in graphql-js.
pub fn make_print_schema_compatible(schema: &mut Schema) {
    remove_non_semantic_directives(schema);
    coerce_schema_default_values(schema);
}
