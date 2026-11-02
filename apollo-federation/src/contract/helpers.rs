use apollo_compiler::Name;
use apollo_compiler::Node;
use apollo_compiler::Schema;
use apollo_compiler::collections::IndexSet;
use apollo_compiler::schema::Directive;

use crate::error::FederationError;
use crate::link::metadata::LinksMetadata;
use crate::link::spec::Identity;
use crate::schema::FederationSchema;
use crate::schema::GRAPHQL_BUILT_IN_DIRECTIVES;
use crate::schema::position::DirectiveTargetPosition;
use crate::schema::position::HasAppliedDirectives;
use crate::schema::position::ObjectOrInterfaceTypeDefinitionPosition;
use crate::schema::position::TypeDefinitionPosition;
use crate::schema::referencer::DirectiveReferencers;

/// The schema's `@link` metadata, plus the `@tag`/`@inaccessible`/`@link`
/// directive names it resolves to.
///
/// Only the three names are cached, because they are read on essentially every
/// element the pipeline visits and `directive_name_in_schema` returns an owned
/// `Name`. Everything else is derived from `links` on demand.
///
/// Names come from `apollo-federation`, so `@link(as:)` spec aliases and
/// `@link(import: [{name: "@tag", as: "@myTag"}])` element aliases are both
/// honoured.
#[derive(Debug, Clone)]
pub(crate) struct FilterDirectiveMetadata {
    pub(crate) tag: Name,
    pub(crate) inaccessible: Name,
    pub(crate) link: Name,
    /// `None` when the schema has no bootstrap `@link`/`@core`, in which case
    /// the names above are the unprefixed defaults and nothing is core.
    links: Option<LinksMetadata>,
}

impl FilterDirectiveMetadata {
    /// Resolve the `@tag`, `@inaccessible` and `@link`/`@core` directive names
    /// for a schema.
    ///
    /// Reads the link metadata `FederationSchema` already computed, so `@link(as:)` spec
    /// aliases and `@link(import: [{name: "@tag", as: "@myTag"}])` element aliases are both
    /// honoured, and a malformed `@link` has already been reported by the time we get here.
    /// A schema with no bootstrap `@link`/`@core` falls back to the unprefixed defaults.
    pub(crate) fn from_schema(schema: &FederationSchema) -> Self {
        let Some(links) = schema.metadata() else {
            return Self {
                tag: Identity::TAG_NAME,
                inaccessible: Identity::INACCESSIBLE_NAME,
                link: Identity::LINK_NAME,
                links: None,
            };
        };

        let directive_name = |identity: &Identity, name_in_spec: Name| {
            links
                .for_identity(identity)
                .map(|link| link.directive_name_in_schema(&name_in_spec))
                .unwrap_or(name_in_spec)
        };

        Self {
            tag: directive_name(&Identity::tag_identity(), Identity::TAG_NAME),
            inaccessible: directive_name(
                &Identity::inaccessible_identity(),
                Identity::INACCESSIBLE_NAME,
            ),
            link: links
                .for_identity(&Identity::link_identity())
                .or_else(|| links.for_identity(&Identity::core_identity()))
                .map(|link| link.spec_name_in_schema())
                .unwrap_or(Identity::LINK_NAME),
            // Cloned so the steps can hold this while mutating the schema. The maps are
            // small and their entries are `Arc`s.
            links: Some(links.clone()),
        }
    }

    /// Returns true if the type belongs to a linked core spec (`join__FieldSet`,
    /// `link__Import`, ...), and so must never be filtered.
    ///
    /// Resolved from the schema's own `@link` applications, so specs renamed via
    /// `@link(as:)` and specs we do not know about are both handled.
    pub(crate) fn is_core_type(&self, name: &Name) -> bool {
        self.links
            .as_ref()
            .is_some_and(|links| links.source_link_of_type(name).is_some())
    }

    /// Returns true if the directive belongs to a linked core spec (including
    /// `@tag`, `@inaccessible` and `@link`/`@core` themselves).
    pub(crate) fn is_core_directive(&self, name: &Name) -> bool {
        *name == self.link
            || self
                .links
                .as_ref()
                .is_some_and(|links| links.source_link_of_directive(name).is_some())
    }
}

/// Returns true for built-in scalars (`String`, `Int`, ...) and introspection
/// types (`__Schema`, `__Type`, ...).
///
/// apollo-compiler sources these from `FileId::BUILT_IN` and never serializes
/// them, so they must never be filtered.
pub(crate) fn is_built_in_type(schema: &Schema, name: &Name) -> bool {
    schema.types.get(name).is_some_and(|ty| ty.is_built_in())
}

// ---- Directive helpers ----

/// A `@tag(name: "<tag_value>")` application, under the schema's name for `@tag`.
pub(crate) fn tag_directive(tag_directive_name: &Name, tag_value: &str) -> Directive {
    Directive {
        name: tag_directive_name.clone(),
        arguments: vec![Node::new(apollo_compiler::ast::Argument {
            // SAFETY: "name" is always a valid GraphQL name
            name: Name::new_unchecked("name"),
            value: Node::new(apollo_compiler::ast::Value::String(tag_value.to_string())),
        })],
    }
}

/// Collect the `name:` argument of each `@tag` application.
pub(crate) fn tag_values<'a>(directives: impl Iterator<Item = &'a Directive>) -> IndexSet<String> {
    directives
        .flat_map(|d| &d.arguments)
        .filter(|arg| arg.name == "name")
        .filter_map(|arg| match &*arg.value {
            apollo_compiler::ast::Value::String(s) => Some(s.clone()),
            _ => None,
        })
        .collect()
}

/// Returns true for the GraphQL built-in directives, which must never be filtered.
pub(crate) fn is_built_in_directive(schema: &Schema, name: &Name) -> bool {
    GRAPHQL_BUILT_IN_DIRECTIVES.contains(&name.as_str())
        || schema
            .directive_definitions
            .get(name)
            .is_some_and(|def| def.is_built_in())
}

// ---- Position helpers ----
//
// Every step reads and writes through the position API, so the schema's referencers stay
// in step with its edits. That is what lets a step start from the elements that carry
// `@tag` or `@inaccessible` instead of walking the whole schema.

/// The `name:` of each `@tag` applied at `position`.
pub(crate) fn applied_tags(
    position: &impl HasAppliedDirectives,
    schema: &FederationSchema,
    metadata: &FilterDirectiveMetadata,
) -> IndexSet<String> {
    tag_values(
        position
            .get_applied_directives(schema, &metadata.tag)
            .into_iter()
            .map(AsRef::as_ref),
    )
}

/// Whether `@inaccessible` is applied at `position`.
pub(crate) fn is_inaccessible(
    position: &impl HasAppliedDirectives,
    schema: &FederationSchema,
    metadata: &FilterDirectiveMetadata,
) -> bool {
    !position
        .get_applied_directives(schema, &metadata.inaccessible)
        .is_empty()
}

/// Apply `@inaccessible` at `position`, unless it is already there.
pub(crate) fn mark_inaccessible(
    schema: &mut FederationSchema,
    position: impl Into<DirectiveTargetPosition>,
    metadata: &FilterDirectiveMetadata,
) -> Result<(), FederationError> {
    let position = position.into();
    if is_inaccessible(&position, schema, metadata) {
        return Ok(());
    }
    position.insert_directive(
        schema,
        Directive {
            name: metadata.inaccessible.clone(),
            arguments: Vec::new(),
        },
    )
}

/// Every element `@inaccessible` is currently applied to.
pub(crate) fn inaccessible_elements<'schema>(
    schema: &'schema FederationSchema,
    metadata: &FilterDirectiveMetadata,
) -> &'schema DirectiveReferencers {
    schema.referencers().get_directive(&metadata.inaccessible)
}

/// Whether filtering may touch type `name`, or anything inside it.
///
/// Built-in types stay out even though an SDL extension (`extend scalar Int @tag(...)`) can
/// tag them, and so put them among the `@tag` referencers.
pub(crate) fn is_filterable_type(
    schema: &Schema,
    metadata: &FilterDirectiveMetadata,
    name: &Name,
) -> bool {
    !is_built_in_type(schema, name) && !metadata.is_core_type(name)
}

/// The types filtering may touch: everything except built-ins and the types of linked
/// core specs.
pub(crate) fn filterable_types<'a>(
    schema: &'a FederationSchema,
    metadata: &'a FilterDirectiveMetadata,
) -> impl Iterator<Item = TypeDefinitionPosition> + 'a {
    schema
        .get_types()
        .filter(|type_| !metadata.is_core_type(type_.type_name()))
}

/// The object and interface types filtering may touch.
pub(crate) fn composite_types<'a>(
    schema: &'a FederationSchema,
    metadata: &'a FilterDirectiveMetadata,
) -> impl Iterator<Item = ObjectOrInterfaceTypeDefinitionPosition> + 'a {
    filterable_types(schema, metadata).filter_map(|type_| type_.try_into().ok())
}
