//! Analysis of a `FieldSelectionMap` against one concrete root type.
//!
//! Used by `LOOKUP_KEY_MISSING_FOR_TYPE` ([`is_mappable`]) to check that a lookup argument maps to
//! fields of every possible type, and by composition ([`selections_for_type`]) to turn a lookup's
//! arguments into the _stable key_ it recalls the entity by.

use apollo_compiler::Name;
use apollo_compiler::Node;
use apollo_compiler::Schema;
use apollo_compiler::ast;
use apollo_compiler::schema::ExtendedType;

use super::Path;
use super::SelectedListValue;
use super::SelectedObjectField;
use super::SelectedObjectValue;
use super::SelectedValue;
use super::SelectedValueEntry;
use super::validate::field_in_schema;
use super::validate::possible_types;

/// Why a selection map cannot be mapped for a concrete type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Unmappable {
    /// No top-level alternative's type condition admits the concrete type.
    NotAdmitted,
    /// An alternative that applies selects `type_name.field`, which is not defined.
    MissingField { type_name: Name, field: Name },
}

/// `IsArgumentMappable` (spec §4 "Lookup Key Missing For Type"): some top-level alternative of
/// `value` admits `concrete`, and every field it references, at every level, is defined for
/// `concrete` in `schema`.
#[cfg(test)]
pub(crate) fn is_mappable(value: &SelectedValue, schema: &Schema, concrete: &Name) -> bool {
    check_mappable(value, schema, concrete).is_ok()
}

/// [`is_mappable`], with the reason when it is not: the first undefined field of an applicable
/// alternative, or [`Unmappable::NotAdmitted`] when no alternative applies.
pub(crate) fn check_mappable(
    value: &SelectedValue,
    schema: &Schema,
    concrete: &Name,
) -> Result<(), Unmappable> {
    if !matches!(schema.types.get(concrete), Some(ExtendedType::Object(_))) {
        return Err(Unmappable::NotAdmitted);
    }
    Mappability { schema, concrete }.value(value, concrete, true)
}

struct Mappability<'a> {
    schema: &'a Schema,
    concrete: &'a Name,
}

impl Mappability<'_> {
    /// At the root, type conditions select among alternatives, so one applicable, defined
    /// alternative is enough. Below a path the runtime type is unknown statically, so every
    /// alternative must be defined.
    fn value(&self, value: &SelectedValue, ty: &Name, at_root: bool) -> Result<(), Unmappable> {
        if !at_root {
            return value
                .alternatives
                .iter()
                .try_for_each(|entry| self.entry(entry, ty, false));
        }
        // Prefer reporting a missing field of an applicable alternative over "nothing applies".
        let mut error = Unmappable::NotAdmitted;
        for entry in &value.alternatives {
            match self.entry(entry, ty, true) {
                Ok(()) => return Ok(()),
                Err(missing @ Unmappable::MissingField { .. }) => {
                    if error == Unmappable::NotAdmitted {
                        error = missing;
                    }
                }
                Err(Unmappable::NotAdmitted) => {}
            }
        }
        Err(error)
    }

    fn entry(
        &self,
        entry: &SelectedValueEntry,
        ty: &Name,
        at_root: bool,
    ) -> Result<(), Unmappable> {
        match entry {
            SelectedValueEntry::Path(path) => self.path(path, ty, at_root).map(|_| ()),
            SelectedValueEntry::PathObject(path, object) => {
                let selected = self.path(path, ty, at_root)?;
                self.object(object, &selected, false)
            }
            SelectedValueEntry::PathList(path, list) => {
                let selected = self.path(path, ty, at_root)?;
                self.list(list, &selected)
            }
            SelectedValueEntry::Object(object) => self.object(object, ty, at_root),
        }
    }

    /// The named type a path selects from `ty`. Fails if a field along it is not defined or a
    /// root type condition excludes the concrete type. A nested type condition switches to its
    /// type.
    fn path(&self, path: &Path, ty: &Name, at_root: bool) -> Result<Name, Unmappable> {
        let mut current = ty.clone();
        if let Some(type_condition) = &path.type_condition {
            if at_root {
                if !possible_types(self.schema, type_condition).contains(self.concrete) {
                    return Err(Unmappable::NotAdmitted);
                }
            } else {
                current = type_condition.clone();
            }
        }
        for segment in &path.segments {
            let field = self.field(&current, &segment.field)?;
            current = segment
                .type_condition
                .clone()
                .unwrap_or_else(|| field.ty.inner_named_type().clone());
        }
        Ok(current)
    }

    fn field(
        &self,
        type_name: &Name,
        field: &Name,
    ) -> Result<&Node<apollo_compiler::schema::FieldDefinition>, Unmappable> {
        field_in_schema(self.schema, type_name, field).ok_or_else(|| Unmappable::MissingField {
            type_name: type_name.clone(),
            field: field.clone(),
        })
    }

    fn object(
        &self,
        object: &SelectedObjectValue,
        ty: &Name,
        at_root: bool,
    ) -> Result<(), Unmappable> {
        object.fields.iter().try_for_each(|field| match field {
            SelectedObjectField::Shorthand(name, _) => self.field(ty, name).map(|_| ()),
            SelectedObjectField::Labeled(_, value) => self.value(value, ty, at_root),
        })
    }

    fn list(&self, list: &SelectedListValue, ty: &Name) -> Result<(), Unmappable> {
        match list {
            SelectedListValue::Value(value) => self.value(value, ty, false),
            SelectedListValue::List(inner) => self.list(inner, ty),
        }
    }
}

/// Collects the fields a selection map reads for one concrete root type, as field-set text.
struct Collector<'a> {
    schema: &'a Schema,
    concrete: &'a Name,
}

impl Collector<'_> {
    fn admits(&self, type_condition: &Name) -> bool {
        possible_types(self.schema, type_condition).contains(self.concrete)
    }

    /// At the root, type conditions select among alternatives, so only the first applicable one
    /// is read. Below a path the runtime type is unknown statically, so every alternative is read.
    /// `None` when no root alternative applies.
    fn value(&self, value: &SelectedValue, at_root: bool) -> Option<String> {
        if at_root {
            return value
                .alternatives
                .iter()
                .find_map(|entry| self.entry(entry, true));
        }
        let fields: Vec<String> = value
            .alternatives
            .iter()
            .filter_map(|entry| self.entry(entry, false))
            .collect();
        Some(fields.join(" "))
    }

    fn entry(&self, entry: &SelectedValueEntry, at_root: bool) -> Option<String> {
        match entry {
            SelectedValueEntry::Path(path) => self.path(path, at_root, None),
            SelectedValueEntry::PathObject(path, object) => {
                let object = self.object(object, false)?;
                self.path(path, at_root, Some(object))
            }
            SelectedValueEntry::PathList(path, list) => {
                self.path(path, at_root, Some(self.list(list)))
            }
            SelectedValueEntry::Object(object) => self.object(object, at_root),
        }
    }

    /// The fields of `path`, with `selection` selected under its last segment. `None` when a root
    /// type condition excludes the concrete type.
    fn path(&self, path: &Path, at_root: bool, selection: Option<String>) -> Option<String> {
        let mut text = selection;
        for segment in path.segments.iter().rev() {
            if let Some(type_condition) = &segment.type_condition {
                text = Some(inline_fragment(type_condition, text.as_deref()));
            }
            let field = field(&segment.field, &segment.arguments);
            text = Some(match text {
                Some(selection) => format!("{field} {{ {selection} }}"),
                None => field,
            });
        }
        match &path.type_condition {
            Some(type_condition) if at_root => self.admits(type_condition).then_some(text?),
            Some(type_condition) => Some(inline_fragment(type_condition, text.as_deref())),
            None => text,
        }
    }

    fn object(&self, object: &SelectedObjectValue, at_root: bool) -> Option<String> {
        let fields = object
            .fields
            .iter()
            .map(|selected| match selected {
                SelectedObjectField::Labeled(_, value) => self.value(value, at_root),
                SelectedObjectField::Shorthand(name, arguments) => Some(field(name, arguments)),
            })
            .collect::<Option<Vec<String>>>()?;
        Some(fields.join(" "))
    }

    fn list(&self, list: &SelectedListValue) -> String {
        match list {
            SelectedListValue::Value(value) => self.value(value, false).unwrap_or_default(),
            SelectedListValue::List(inner) => self.list(inner),
        }
    }
}

fn field(name: &Name, arguments: &[Node<ast::Argument>]) -> String {
    if arguments.is_empty() {
        return name.to_string();
    }
    let arguments: Vec<String> = arguments
        .iter()
        .map(|argument| {
            format!(
                "{}: {}",
                argument.name,
                argument.value.serialize().no_indent()
            )
        })
        .collect();
    format!("{name}({})", arguments.join(", "))
}

fn inline_fragment(type_condition: &Name, selection: Option<&str>) -> String {
    format!(
        "... on {type_condition} {{ {} }}",
        selection.unwrap_or_default()
    )
}

/// The output fields `value` reads when the root object has the concrete type `concrete`, as
/// field-set text, one per applicable top-level alternative. Empty when no alternative applies.
///
/// Each top-level alternative of an argument is a distinct way to recall the entity (for a
/// `@oneOf` input, a distinct _stable key_), hence one field set each.
pub(crate) fn selections_for_type(
    value: &SelectedValue,
    schema: &Schema,
    concrete: &Name,
) -> Vec<String> {
    let collector = Collector { schema, concrete };
    value
        .alternatives
        .iter()
        .filter_map(|entry| collector.entry(entry, true))
        .collect()
}
