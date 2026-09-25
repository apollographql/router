//! Semantic validation of a parsed `FieldSelectionMap` (Appendix A, "Validation").
//!
//! A selection map relates two sides that may live in different schemas: the *output* side (the
//! type fields are selected from — for `@is` the lookup's return type, for `@require` the type
//! declaring the field) and the *input* side (the argument's type, from the source schema that
//! declares the argument). Post-merge rules validate the output side against the merged schema
//! while the input side always comes from the declaring source schema, so both are parameters.

use apollo_compiler::Name;
use apollo_compiler::Node;
use apollo_compiler::Schema;
use apollo_compiler::ast;
use apollo_compiler::collections::HashSet;
use apollo_compiler::collections::IndexSet;
use apollo_compiler::schema::ExtendedType;
use apollo_compiler::schema::FieldDefinition;

use super::Path;
use super::SelectedListValue;
use super::SelectedObjectField;
use super::SelectedObjectValue;
use super::SelectedValue;
use super::SelectedValueEntry;

/// The output side of a selection map: where selected fields are looked up. It may span several
/// schemas (e.g. every source schema other than the one declaring a `@require`).
pub(crate) trait OutputSchema {
    /// The definition of `type_name.field_name`, if selectable, with the schema defining it (used
    /// to coerce literal arguments).
    fn field(
        &self,
        type_name: &Name,
        field_name: &Name,
    ) -> Option<(&Node<FieldDefinition>, &Schema)>;
    /// Whether `type_name` is an object, interface or union type.
    fn is_composite(&self, type_name: &Name) -> bool;
    /// Whether `type_name` is a scalar or enum type.
    fn is_leaf(&self, type_name: &Name) -> bool;
    /// The possible object types of a composite type.
    fn possible_types(&self, type_name: &Name) -> IndexSet<Name>;
}

/// A single schema as the output side.
#[cfg(test)]
pub(crate) struct SingleSchemaOutput<'a>(pub(crate) &'a Schema);

#[cfg(test)]
impl OutputSchema for SingleSchemaOutput<'_> {
    fn field(
        &self,
        type_name: &Name,
        field_name: &Name,
    ) -> Option<(&Node<FieldDefinition>, &Schema)> {
        field_in_schema(self.0, type_name, field_name).map(|f| (f, self.0))
    }

    fn is_composite(&self, type_name: &Name) -> bool {
        is_composite_in_schema(self.0, type_name)
    }

    fn is_leaf(&self, type_name: &Name) -> bool {
        is_leaf_in_schema(self.0, type_name)
    }

    fn possible_types(&self, type_name: &Name) -> IndexSet<Name> {
        possible_types(self.0, type_name)
    }
}

pub(crate) fn field_in_schema<'s>(
    schema: &'s Schema,
    type_name: &Name,
    field_name: &Name,
) -> Option<&'s Node<FieldDefinition>> {
    match schema.types.get(type_name)? {
        ExtendedType::Object(object) => object.fields.get(field_name).map(|f| &f.node),
        ExtendedType::Interface(interface) => interface.fields.get(field_name).map(|f| &f.node),
        _ => None,
    }
}

pub(crate) fn is_composite_in_schema(schema: &Schema, type_name: &Name) -> bool {
    matches!(
        schema.types.get(type_name),
        Some(ExtendedType::Object(_) | ExtendedType::Interface(_) | ExtendedType::Union(_))
    )
}

pub(crate) fn is_leaf_in_schema(schema: &Schema, type_name: &Name) -> bool {
    matches!(
        schema.types.get(type_name),
        Some(ExtendedType::Scalar(_) | ExtendedType::Enum(_))
    )
}

/// A validation failure, with a human-readable message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FieldSelectionMapValidationError {
    pub(crate) message: String,
}

/// The possible object types of a composite type: itself for an object, its implementations for
/// an interface, its members for a union.
pub(crate) fn possible_types(schema: &Schema, type_name: &Name) -> IndexSet<Name> {
    match schema.types.get(type_name) {
        Some(ExtendedType::Object(_)) => IndexSet::from_iter([type_name.clone()]),
        Some(ExtendedType::Interface(_)) => schema
            .types
            .iter()
            .filter_map(|(name, ty)| match ty {
                ExtendedType::Object(object)
                    if object.implements_interfaces.contains(type_name) =>
                {
                    Some(name.clone())
                }
                _ => None,
            })
            .collect(),
        Some(ExtendedType::Union(union_)) => {
            union_.members.iter().map(|m| m.name.clone()).collect()
        }
        _ => IndexSet::default(),
    }
}

struct Validator<'a> {
    output: &'a dyn OutputSchema,
    input: &'a Schema,
    errors: Vec<FieldSelectionMapValidationError>,
}

/// Validate `value` against an output root type and the expected input (argument) type.
pub(crate) fn validate(
    value: &SelectedValue,
    output: &dyn OutputSchema,
    output_root: &Name,
    input: &Schema,
    expected: &ast::Type,
) -> Vec<FieldSelectionMapValidationError> {
    let mut validator = Validator {
        output,
        input,
        errors: Vec::new(),
    };
    validator.value(value, output_root, expected);
    validator.errors
}

/// Whether the map supplies any field argument, anywhere (`IS_FIELDS_HAS_ARGUMENTS`).
pub(crate) fn has_arguments(value: &SelectedValue) -> bool {
    fn path_has_arguments(path: &Path) -> bool {
        path.segments.iter().any(|s| !s.arguments.is_empty())
    }
    fn object_has_arguments(object: &SelectedObjectValue) -> bool {
        object.fields.iter().any(|field| match field {
            SelectedObjectField::Labeled(_, value) => has_arguments(value),
            SelectedObjectField::Shorthand(_, arguments) => !arguments.is_empty(),
        })
    }
    fn list_has_arguments(list: &SelectedListValue) -> bool {
        match list {
            SelectedListValue::Value(value) => has_arguments(value),
            SelectedListValue::List(list) => list_has_arguments(list),
        }
    }
    value.alternatives.iter().any(|entry| match entry {
        SelectedValueEntry::Path(path) => path_has_arguments(path),
        SelectedValueEntry::PathObject(path, object) => {
            path_has_arguments(path) || object_has_arguments(object)
        }
        SelectedValueEntry::PathList(path, list) => {
            path_has_arguments(path) || list_has_arguments(list)
        }
        SelectedValueEntry::Object(object) => object_has_arguments(object),
    })
}

impl Validator<'_> {
    fn error(&mut self, message: impl Into<String>) {
        self.errors.push(FieldSelectionMapValidationError {
            message: message.into(),
        });
    }

    fn value(&mut self, value: &SelectedValue, output_type: &Name, expected: &ast::Type) {
        for entry in &value.alternatives {
            self.entry(entry, output_type, expected);
        }
    }

    fn entry(&mut self, entry: &SelectedValueEntry, output_type: &Name, expected: &ast::Type) {
        match entry {
            SelectedValueEntry::Path(path) => {
                let Some(selected) = self.path(path, output_type) else {
                    return;
                };
                if !self.output.is_leaf(selected.inner_named_type()) {
                    self.error(format!(
                        "path \"{path}\" must end at a scalar or enum field, but selects a value \
                         of type \"{selected}\""
                    ));
                    return;
                }
                if !self.coercible(&selected, expected) {
                    self.error(format!(
                        "path \"{path}\" selects a value of type \"{selected}\", which is not \
                         coercible to the expected type \"{expected}\""
                    ));
                }
            }
            SelectedValueEntry::PathObject(path, object) => {
                let Some(selected) = self.path(path, output_type) else {
                    return;
                };
                if selected.is_list() {
                    self.error(format!(
                        "path \"{path}\" selects a list; use \"{path}[...]\" to select from its \
                         elements"
                    ));
                    return;
                }
                let named = selected.inner_named_type().clone();
                if !self.output.is_composite(&named) {
                    self.error(format!(
                        "path \"{path}\" selects the leaf type \"{named}\", which has no fields \
                         to select"
                    ));
                    return;
                }
                self.object(object, &named, expected);
            }
            SelectedValueEntry::PathList(path, list) => {
                let Some(selected) = self.path(path, output_type) else {
                    return;
                };
                if !selected.is_list() {
                    self.error(format!(
                        "path \"{path}\" is followed by a list selection but selects the \
                         non-list type \"{selected}\""
                    ));
                    return;
                }
                self.list(list, &selected, expected);
            }
            SelectedValueEntry::Object(object) => self.object(object, output_type, expected),
        }
    }

    /// Resolve a path from `output_type`, returning the (wrapped) type of its last segment. Type
    /// conditions narrow the type they apply to.
    fn path(&mut self, path: &Path, output_type: &Name) -> Option<ast::Type> {
        let mut current = output_type.clone();
        if let Some(type_condition) = &path.type_condition {
            current = self.narrow(&current, type_condition)?;
        }
        let last = path.segments.len().saturating_sub(1);
        let mut selected = None;
        for (i, segment) in path.segments.iter().enumerate() {
            let Some((field, field_schema)) = self.output.field(&current, &segment.field) else {
                self.error(format!(
                    "field \"{}\" is not defined on type \"{current}\"",
                    segment.field
                ));
                return None;
            };
            self.arguments(field_schema, &current, field, &segment.arguments);
            let ty = field.ty.clone();
            let mut named = ty.inner_named_type().clone();
            if let Some(type_condition) = &segment.type_condition {
                named = self.narrow(&named, type_condition)?;
            }
            if i < last {
                if ty.is_list() {
                    self.error(format!(
                        "field \"{current}.{}\" is a list; a path cannot traverse a list without \
                         a list selection (\"{}[...]\")",
                        segment.field, segment.field
                    ));
                    return None;
                }
                if !self.output.is_composite(&named) {
                    self.error(format!(
                        "field \"{current}.{}\" has the leaf type \"{named}\" and cannot be \
                         followed by further path segments",
                        segment.field
                    ));
                    return None;
                }
                current = named;
            } else {
                selected = Some(ty);
            }
        }
        selected
    }

    fn narrow(&mut self, parent: &Name, type_condition: &Name) -> Option<Name> {
        if !self.output.is_composite(type_condition) {
            self.error(format!(
                "type condition \"{type_condition}\" does not name a composite type"
            ));
            return None;
        }
        let parent_types = self.output.possible_types(parent);
        let condition_types = self.output.possible_types(type_condition);
        if parent_types.intersection(&condition_types).next().is_none() {
            self.error(format!(
                "type condition \"{type_condition}\" can never apply to type \"{parent}\""
            ));
            return None;
        }
        Some(type_condition.clone())
    }

    fn arguments(
        &mut self,
        field_schema: &Schema,
        parent: &Name,
        field: &FieldDefinition,
        arguments: &[Node<ast::Argument>],
    ) {
        let mut seen = HashSet::default();
        for argument in arguments {
            if !seen.insert(argument.name.clone()) {
                self.error(format!(
                    "argument \"{}\" is supplied more than once to \"{parent}.{}\"",
                    argument.name, field.name
                ));
                continue;
            }
            let Some(definition) = field.arguments.iter().find(|a| a.name == argument.name) else {
                self.error(format!(
                    "argument \"{}\" is not defined on \"{parent}.{}\"",
                    argument.name, field.name
                ));
                continue;
            };
            if !const_value_coercible(field_schema, &argument.value, &definition.ty) {
                self.error(format!(
                    "value {} is not coercible to the type \"{}\" of argument \"{parent}.{}({}:)\"",
                    argument.value, definition.ty, field.name, argument.name
                ));
            }
        }
        for definition in &field.arguments {
            if definition.ty.is_non_null()
                && definition.default_value.is_none()
                && !seen.contains(&definition.name)
            {
                self.error(format!(
                    "required argument \"{parent}.{}({}:)\" is not supplied",
                    field.name, definition.name
                ));
            }
        }
    }

    fn object(&mut self, object: &SelectedObjectValue, output_type: &Name, expected: &ast::Type) {
        if expected.is_list() {
            self.error(format!(
                "a selected object value cannot produce the list type \"{expected}\""
            ));
            return;
        }
        let input_type_name = expected.inner_named_type();
        let Some(ExtendedType::InputObject(input_object)) = self.input.types.get(input_type_name)
        else {
            self.error(format!(
                "a selected object value requires an input object type, but the expected type is \
                 \"{expected}\""
            ));
            return;
        };
        let is_one_of = input_object.directives.has("oneOf");
        let mut seen = HashSet::default();
        for field in &object.fields {
            let name = field.name();
            if !seen.insert(name.clone()) {
                self.error(format!(
                    "field \"{name}\" is selected more than once in a selected object value"
                ));
                continue;
            }
            let Some(input_field) = input_object.fields.get(name) else {
                self.error(format!(
                    "field \"{name}\" is not defined on input type \"{input_type_name}\""
                ));
                continue;
            };
            let input_field_type = input_field.ty.as_ref().clone();
            match field {
                SelectedObjectField::Labeled(_, value) => {
                    self.value(value, output_type, &input_field_type)
                }
                SelectedObjectField::Shorthand(name, arguments) => {
                    let path = Path {
                        type_condition: None,
                        segments: vec![super::PathSegment {
                            field: name.clone(),
                            arguments: arguments.clone(),
                            type_condition: None,
                        }],
                    };
                    self.entry(
                        &SelectedValueEntry::Path(path),
                        output_type,
                        &input_field_type,
                    );
                }
            }
        }
        if is_one_of {
            if object.fields.len() != 1 {
                self.error(format!(
                    "a selected object value for the @oneOf input type \"{input_type_name}\" must \
                     select exactly one field"
                ));
            }
        } else {
            for (name, input_field) in &input_object.fields {
                if input_field.ty.is_non_null()
                    && input_field.default_value.is_none()
                    && !seen.contains(name)
                {
                    self.error(format!(
                        "required input field \"{input_type_name}.{name}\" is not selected"
                    ));
                }
            }
        }
    }

    fn list(&mut self, list: &SelectedListValue, selected: &ast::Type, expected: &ast::Type) {
        let (ast::Type::List(output_item) | ast::Type::NonNullList(output_item)) = selected else {
            self.error(format!(
                "a list selection requires a list, but the selected type is \"{selected}\""
            ));
            return;
        };
        let (ast::Type::List(expected_item) | ast::Type::NonNullList(expected_item)) = expected
        else {
            self.error(format!(
                "a list selection produces a list, but the expected type is \"{expected}\""
            ));
            return;
        };
        match list {
            SelectedListValue::List(inner) => self.list(inner, output_item, expected_item),
            SelectedListValue::Value(value) => {
                if output_item.is_list() {
                    self.error(format!(
                        "the selected list has nested list elements of type \"{output_item}\"; \
                         use a nested list selection (\"[[...]]\")"
                    ));
                    return;
                }
                let named = output_item.inner_named_type();
                if !self.output.is_composite(named) {
                    self.error(format!(
                        "a list selection requires a list of composite types, but the elements \
                         have type \"{named}\""
                    ));
                    return;
                }
                self.value(value, named, expected_item);
            }
        }
    }

    /// Whether a selected output value of type `output` can be used as an input value of type
    /// `expected`. Nullability is not enforced: a nullable output may feed a non-null input (the
    /// executor then skips the value), matching the specification's examples.
    fn coercible(&self, output: &ast::Type, expected: &ast::Type) -> bool {
        match (output, expected) {
            (
                ast::Type::List(o) | ast::Type::NonNullList(o),
                ast::Type::List(e) | ast::Type::NonNullList(e),
            ) => self.coercible(o, e),
            (
                ast::Type::Named(o) | ast::Type::NonNullNamed(o),
                ast::Type::Named(e) | ast::Type::NonNullNamed(e),
            ) => o == e || (o == "Int" && e == "Float"),
            _ => false,
        }
    }
}

/// Whether a constant value literal coerces to `ty` in `schema` (GraphQL input coercion).
pub(crate) fn const_value_coercible(schema: &Schema, value: &ast::Value, ty: &ast::Type) -> bool {
    if let ast::Value::Null = value {
        return !ty.is_non_null();
    }
    match ty {
        ast::Type::List(item) | ast::Type::NonNullList(item) => match value {
            ast::Value::List(items) => items
                .iter()
                .all(|item_value| const_value_coercible(schema, item_value, item)),
            // A single value coerces to a list of one.
            other => const_value_coercible(schema, other, item),
        },
        ast::Type::Named(name) | ast::Type::NonNullNamed(name) => {
            match (schema.types.get(name), value) {
                (Some(ExtendedType::Enum(enum_)), ast::Value::Enum(value)) => {
                    enum_.values.contains_key(value)
                }
                (Some(ExtendedType::InputObject(input)), ast::Value::Object(fields)) => {
                    fields.iter().all(|(field_name, field_value)| {
                        input
                            .fields
                            .get(field_name)
                            .is_some_and(|f| const_value_coercible(schema, field_value, &f.ty))
                    }) && input.fields.iter().all(|(field_name, field)| {
                        !field.ty.is_non_null()
                            || field.default_value.is_some()
                            || fields.iter().any(|(n, _)| n == field_name)
                    })
                }
                (Some(ExtendedType::Scalar(_)), value) => match name.as_str() {
                    "Int" => matches!(value, ast::Value::Int(_)),
                    "Float" => matches!(value, ast::Value::Int(_) | ast::Value::Float(_)),
                    "String" => matches!(value, ast::Value::String(_)),
                    "Boolean" => matches!(value, ast::Value::Boolean(_)),
                    "ID" => matches!(value, ast::Value::String(_) | ast::Value::Int(_)),
                    // Custom scalars accept any literal.
                    _ => true,
                },
                _ => false,
            }
        }
    }
}
