//! Removing `@internal` elements and `@require` arguments before merging.
//!
//! An internal field or type is local to its source schema: it is never part of the composite
//! schema, so it does not take part in merging (and cannot conflict with a public field or type of
//! the same name in another source schema, spec `INVALID_FIELD_SHARING` and "Merge Input Types").
//! Arguments annotated `@require` are supplied by the executor and "do not participate in the
//! merge process" either. Validation, including the cross-schema `@is`/`@require` rules, runs on
//! the unmodified subgraphs before this.
//!
//! The supergraph does not record lookups or requirements yet, so the planner cannot use internal
//! lookups or build fetches that supply `@require` arguments. Recording them is left to the change
//! that adds that join metadata (spec: "Internal lookup fields remain usable for transitions").

use apollo_compiler::Name;
use apollo_compiler::Schema;
use apollo_compiler::collections::IndexSet;
use apollo_compiler::schema::ExtendedType;

use super::validation::fields_of;
use super::validation::is_federation_root_field;

/// Whether a type is internal: annotated with `@internal`, or an object or interface type whose
/// fields are all `@internal`. Validation and removal both use this, so they cannot disagree on
/// which types are not merged. Root operation types are never removed, which callers handle.
pub(crate) fn is_internal_type(ty: &ExtendedType, internal: &Name) -> bool {
    ty.directives().has(internal)
        || fields_of(ty).is_some_and(|fields| fields.values().all(|f| f.directives.has(internal)))
}

/// Remove from a subgraph schema (directive names as in `schema`):
/// - fields annotated `@internal`;
/// - object and interface types annotated `@internal`, or whose fields are all `@internal`, other
///   than root operation types (only their internal fields are removed, which is every field
///   but `_service`/`_entities` for a root type annotated `@internal`);
/// - arguments annotated `@require`;
/// - input object types annotated `@internal`. Source-schema validation
///   (`INTERNAL_INPUT_TYPE_REFERENCED_PUBLICLY`) guarantees that only the elements removed above
///   reference them.
///
/// Returns whether anything was removed.
pub(crate) fn remove_internal_elements(
    schema: &mut Schema,
    internal: &Name,
    require: &Name,
) -> bool {
    let root_types: IndexSet<Name> = schema
        .schema_definition
        .iter_root_operations()
        .map(|(_, name)| Name::clone(name))
        .collect();
    let internal_types: IndexSet<Name> = schema
        .types
        .iter()
        .filter(|(name, ty)| {
            !root_types.contains(*name)
                && matches!(ty, ExtendedType::Object(_) | ExtendedType::Interface(_))
                && is_internal_type(ty, internal)
        })
        .map(|(name, _)| name.clone())
        .collect();
    let mut changed = false;

    for name in &internal_types {
        schema.types.shift_remove(name);
        changed = true;
    }
    for ty in schema.types.values_mut() {
        match ty {
            ExtendedType::Object(object) => {
                let before = object.fields.len() + object.implements_interfaces.len();
                // Only a root type can still be annotated `@internal` here: the annotation is
                // shorthand for `@internal` on each of its fields, but the type itself stays.
                let type_is_internal = object.directives.has(internal);
                let object = object.make_mut();
                object.fields.retain(|name, f| {
                    !f.directives.has(internal)
                        && !(type_is_internal && !is_federation_root_field(name))
                });
                for field in object.fields.values_mut() {
                    if field.arguments.iter().any(|a| a.directives.has(require)) {
                        field
                            .make_mut()
                            .arguments
                            .retain(|a| !a.directives.has(require));
                        changed = true;
                    }
                }
                object
                    .implements_interfaces
                    .retain(|i| !internal_types.contains(&**i));
                changed |= before != object.fields.len() + object.implements_interfaces.len();
            }
            ExtendedType::Interface(interface) => {
                let before = interface.fields.len() + interface.implements_interfaces.len();
                let interface = interface.make_mut();
                interface.fields.retain(|_, f| !f.directives.has(internal));
                for field in interface.fields.values_mut() {
                    if field.arguments.iter().any(|a| a.directives.has(require)) {
                        field
                            .make_mut()
                            .arguments
                            .retain(|a| !a.directives.has(require));
                        changed = true;
                    }
                }
                interface
                    .implements_interfaces
                    .retain(|i| !internal_types.contains(&**i));
                changed |= before != interface.fields.len() + interface.implements_interfaces.len();
            }
            _ => {}
        }
    }

    let before = schema.types.len();
    schema.types.retain(|_, ty| {
        !(matches!(ty, ExtendedType::InputObject(_)) && ty.directives().has(internal))
    });
    changed |= before != schema.types.len();
    changed
}
