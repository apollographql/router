//! Analysis of a `FieldSelectionMap` against one concrete root type.
//!
//! Used by `LOOKUP_KEY_MISSING_FOR_TYPE` ([`is_mappable`]) to check that a lookup argument maps to
//! fields of every possible type.

use apollo_compiler::Name;
use apollo_compiler::Schema;
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
    ) -> Result<&apollo_compiler::Node<apollo_compiler::schema::FieldDefinition>, Unmappable> {
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
