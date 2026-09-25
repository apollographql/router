//! Support for the GraphQL Federation specification (formerly "Composite Schemas").
//!
//! A GraphQL Federation *source schema* is treated internally as a federation v2.16 subgraph: its
//! directives (`@lookup`, `@internal`, `@is`, `@require`) are federation 2.16 directives, and
//! detection stamps an implicit federation `@link` onto schemas that carry none. The rest of this
//! module is the composition behaviour those directives need.

pub(crate) mod cross_schema;
pub(crate) mod detection;
pub(crate) mod lookups;
pub(crate) mod normalize;
pub(crate) mod validation;

use apollo_compiler::Name;
use apollo_compiler::schema::ExtendedType;

use crate::link::federation_spec_definition::FEDERATION_CONTEXT_DIRECTIVE_NAME_IN_SPEC;
use crate::link::federation_spec_definition::FEDERATION_FROM_CONTEXT_DIRECTIVE_NAME_IN_SPEC;
use crate::link::federation_spec_definition::FEDERATION_INTERNAL_DIRECTIVE_NAME_IN_SPEC;
use crate::link::federation_spec_definition::FEDERATION_IS_DIRECTIVE_NAME_IN_SPEC;
use crate::link::federation_spec_definition::FEDERATION_KEY_DIRECTIVE_NAME_IN_SPEC;
use crate::link::federation_spec_definition::FEDERATION_LOOKUP_DIRECTIVE_NAME_IN_SPEC;
use crate::link::federation_spec_definition::FEDERATION_REQUIRE_DIRECTIVE_NAME_IN_SPEC;
use crate::link::federation_spec_definition::FEDERATION_REQUIRES_DIRECTIVE_NAME_IN_SPEC;
use crate::link::federation_spec_definition::FederationSpecDefinition;
use crate::link::spec_definition::SpecDefinition;
use crate::schema::FederationSchema;

/// The names, in `schema`, of the GraphQL Federation-only directives (`@lookup`, `@internal`,
/// `@is`, `@require`), or `None` when the linked federation version does not define them.
pub(crate) fn composite_directive_names(
    schema: &FederationSchema,
    federation_spec: &FederationSpecDefinition,
) -> Option<[Name; 4]> {
    if !federation_spec.supports_composite_schemas() {
        return None;
    }
    let link = federation_spec.link_in_schema(schema)?;
    Some(detection::COMPOSITE_ONLY_DIRECTIVES.map(|name| link.directive_name_in_schema(&name)))
}

/// Whether an (expanded) subgraph schema is a GraphQL Federation source schema: it links a
/// federation version defining the dialect's directives, and applies one of them or was detected
/// as a source schema (its implicit link imports `FieldSelectionMap`).
///
/// This is the one dialect signal composition consumes. It drives the dialect-only validation
/// rules and the derivation of `@key` resolvability from lookups; a federation subgraph that
/// merely links federation v2.16 is unaffected.
pub(crate) fn is_composite_schema(
    schema: &FederationSchema,
    federation_spec: &FederationSpecDefinition,
) -> bool {
    let Some(names) = composite_directive_names(schema, federation_spec) else {
        return false;
    };
    // The implicit link stamped onto detected source schemas imports the `FieldSelectionMap`
    // type, which marks the schema as a source schema even if it declares no lookup. (Importing
    // `@lookup` would not do: tooling commonly imports every federation directive.)
    if federation_spec.link_in_schema(schema).is_some_and(|link| {
        link.imports.iter().any(|import| {
            !import.is_directive
                && import.element
                    == crate::link::federation_spec_definition::FEDERATION_FIELD_SELECTION_MAP_TYPE_NAME_IN_SPEC
        })
    }) {
        return true;
    }
    let applied = |directives: &apollo_compiler::ast::DirectiveList| {
        directives.iter().any(|d| names.contains(&d.name))
    };
    schema.schema().types.values().any(|ty| {
        ty.directives().iter().any(|d| names.contains(&d.name))
            || match ty {
                ExtendedType::Object(object) => object.fields.values().any(|field| {
                    applied(&field.directives)
                        || field.arguments.iter().any(|a| applied(&a.directives))
                }),
                ExtendedType::Interface(interface) => interface.fields.values().any(|field| {
                    applied(&field.directives)
                        || field.arguments.iter().any(|a| applied(&a.directives))
                }),
                _ => false,
            }
    })
}

/// The names, in one subgraph schema, of the directives composite-schemas logic reads.
#[derive(Debug, Clone)]
pub(crate) struct CompositeNames {
    pub(crate) lookup: Name,
    pub(crate) internal: Name,
    pub(crate) is: Name,
    pub(crate) require: Name,
    pub(crate) key: Name,
    pub(crate) requires: Option<Name>,
    pub(crate) context: Option<Name>,
    pub(crate) from_context: Option<Name>,
}

impl CompositeNames {
    pub(crate) fn new(
        schema: &FederationSchema,
        federation_spec: &FederationSpecDefinition,
    ) -> Option<Self> {
        if !federation_spec.supports_composite_schemas() {
            return None;
        }
        let link = federation_spec.link_in_schema(schema)?;
        let name = |n: &Name| link.directive_name_in_schema(n);
        let defined = |n: Name| {
            schema
                .schema()
                .directive_definitions
                .contains_key(&n)
                .then_some(n)
        };
        Some(Self {
            lookup: name(&FEDERATION_LOOKUP_DIRECTIVE_NAME_IN_SPEC),
            internal: name(&FEDERATION_INTERNAL_DIRECTIVE_NAME_IN_SPEC),
            is: name(&FEDERATION_IS_DIRECTIVE_NAME_IN_SPEC),
            require: name(&FEDERATION_REQUIRE_DIRECTIVE_NAME_IN_SPEC),
            key: name(&FEDERATION_KEY_DIRECTIVE_NAME_IN_SPEC),
            requires: defined(name(&FEDERATION_REQUIRES_DIRECTIVE_NAME_IN_SPEC)),
            context: defined(name(&FEDERATION_CONTEXT_DIRECTIVE_NAME_IN_SPEC)),
            from_context: defined(name(&FEDERATION_FROM_CONTEXT_DIRECTIVE_NAME_IN_SPEC)),
        })
    }
}
