//! Support for the GraphQL Federation specification (formerly "Composite Schemas").
//!
//! A GraphQL Federation *source schema* is treated internally as a federation v3.0 subgraph: its
//! directives (`@lookup`, `@internal`, `@is`, `@require`) are federation 3.0 directives, and
//! detection stamps an implicit federation `@link` onto schemas that carry none. The rest of this
//! module is the composition behaviour those directives need.

pub(crate) mod cross_schema;
pub(crate) mod detection;
#[cfg(test)]
mod tests;
pub(crate) mod validation;

/// Whether composition may merge subgraphs that apply the GraphQL Federation directives.
///
/// Enabled by the `graphql_federation` crate feature, which only this crate's own tests turn on
/// (see the `dev-dependencies` in `Cargo.toml`). Until the router can plan lookups and
/// requirements, merging such subgraphs fails with `GRAPHQL_FEDERATION_NOT_ENABLED` everywhere
/// else. Subgraph validation runs regardless.
pub(crate) fn graphql_federation_enabled() -> bool {
    #[cfg(test)]
    if let Some(enabled) = GRAPHQL_FEDERATION_OVERRIDE.get() {
        return enabled;
    }
    cfg!(feature = "graphql_federation")
}

#[cfg(test)]
thread_local! {
    /// Per-thread override of [`graphql_federation_enabled`], so unit tests can check the closed
    /// gate even though the crate's tests build with the feature on.
    pub(crate) static GRAPHQL_FEDERATION_OVERRIDE: std::cell::Cell<Option<bool>> =
        const { std::cell::Cell::new(None) };
}

use apollo_compiler::Name;

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
/// federation version defining the dialect's directives and applies `@lookup`.
///
/// A source schema resolves entities through its lookups, so it cannot use `@requires`,
/// `@key(resolvable:)` or `@context`/`@fromContext`. A subgraph that links federation v3.0
/// without applying `@lookup` is not a source schema.
pub(crate) fn is_composite_schema(
    schema: &FederationSchema,
    federation_spec: &FederationSpecDefinition,
) -> bool {
    if !federation_spec.supports_composite_schemas() {
        return false;
    }
    let Some(link) = federation_spec.link_in_schema(schema) else {
        return false;
    };
    let lookup = link.directive_name_in_schema(&FEDERATION_LOOKUP_DIRECTIVE_NAME_IN_SPEC);
    applies_any(schema, std::slice::from_ref(&lookup))
}

/// Whether an (expanded) subgraph schema applies any of the GraphQL Federation directives
/// (`@lookup`, `@internal`, `@is`, `@require`). Their validation rules run whenever they are
/// applied, and merging such a subgraph requires the `graphql_federation` crate feature (see
/// [`graphql_federation_enabled`]).
pub(crate) fn uses_composite_directives(
    schema: &FederationSchema,
    federation_spec: &FederationSpecDefinition,
) -> bool {
    composite_directive_names(schema, federation_spec)
        .is_some_and(|names| applies_any(schema, &names))
}

/// Whether `schema` applies any of `names`.
fn applies_any(schema: &FederationSchema, names: &[Name]) -> bool {
    detection::applied_directive_names(schema.schema()).any(|name| names.contains(name))
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
