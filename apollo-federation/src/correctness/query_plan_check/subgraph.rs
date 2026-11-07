//! The federation metadata a subgraph schema carries: `@key`, `@requires`, `@external` and
//! `@provides`.
//!
//! The model keeps these beside the schema (`Subgraph.keys`, `Subgraph.requires`,
//! `Subgraph.resolves`, `Subgraph.provides`) because its `Schema` does not model directives. Here
//! they are read from the subgraph schema itself, and what the subgraph can resolve from the
//! metadata the query graph is built from.
//!
//! Field sets are written as strings in the directives. A `@key` or `@requires` field set is parsed
//! against the **supergraph** schema, not the subgraph's: it names the requirement the plan must
//! already have satisfied, and that requirement is compared against selections written in
//! supergraph terms. A `@provides` field set is parsed against the subgraph's own schema: what it
//! grants is read against a fetch's operation, which is written in the subgraph's terms.

use apollo_compiler::Name;
use apollo_compiler::Node;
use apollo_compiler::ast;
use apollo_compiler::executable::FieldSet;
use apollo_compiler::executable::Selection;
use apollo_compiler::schema::Directive;
use apollo_compiler::schema::DirectiveDefinition;
use apollo_compiler::schema::ExtendedType;

use super::ComparisonError;
use crate::error::FederationError;
use crate::link::federation_spec_definition::FederationSpecDefinition;
use crate::link::federation_spec_definition::get_federation_spec_definition_from_subgraph;
use crate::schema::ValidFederationSchema;
use crate::schema::position::FieldDefinitionPosition;
use crate::schema::position::ObjectFieldDefinitionPosition;
use crate::schema::subgraph_metadata::SubgraphMetadata;

/// One `@key` application on an entity type in a subgraph.
pub(super) struct KeyDirective {
    /// The key's field set, as written.
    pub(super) fields: String,
    /// Whether the subgraph will resolve the entity from this key. An entity fetch may only rely
    /// on a resolvable key.
    pub(super) resolvable: bool,
}

/// A subgraph's federation metadata.
pub(super) struct Subgraph<'a> {
    schema: &'a ValidFederationSchema,
    federation: &'a FederationSpecDefinition,
    /// The reading of `@external` the query graph is built from.
    metadata: &'a SubgraphMetadata,
}

impl<'a> Subgraph<'a> {
    pub(super) fn new(schema: &'a ValidFederationSchema) -> Result<Self, ComparisonError> {
        let federation = get_federation_spec_definition_from_subgraph(schema).map_err(|e| {
            ComparisonError::new(format!("subgraph has no federation spec definition: {e}"))
        })?;
        let metadata = schema.subgraph_metadata().ok_or_else(|| {
            ComparisonError::new("subgraph schema carries no federation metadata".to_string())
        })?;
        Ok(Subgraph {
            schema,
            federation,
            metadata,
        })
    }

    /// The subgraph's own schema, which a fetch's operation is written against.
    pub(super) fn schema(&self) -> &'a ValidFederationSchema {
        self.schema
    }

    /// A field's definition on an object or interface type, by type and field name.
    pub(super) fn field(
        &self,
        type_name: &Name,
        field_name: &Name,
    ) -> Option<&'a Node<ast::FieldDefinition>> {
        match self.schema.schema().types.get(type_name) {
            Some(ExtendedType::Object(ty)) => ty.fields.get(field_name),
            Some(ExtendedType::Interface(ty)) => ty.fields.get(field_name),
            _ => None,
        }
    }

    /// Whether the subgraph can resolve a field of an object type outright, outside any
    /// `@provides` window: the model's `Subgraph.resolves`.
    ///
    /// It can unless the metadata reads the field as external: marked `@external`, or a field of a
    /// type marked `@external`. A field marked `@external` that a key of a type extension names is
    /// a *fake* external, which the metadata reads as one the subgraph resolves.
    pub(super) fn resolves(&self, object_type: &Name, field_name: &Name) -> bool {
        let position = FieldDefinitionPosition::Object(ObjectFieldDefinitionPosition {
            type_name: object_type.clone(),
            field_name: field_name.clone(),
        });
        !self.metadata.is_field_external(&position)
    }

    /// The `@key` applications on a type. A type may carry several.
    pub(super) fn keys(&self, type_name: &Name) -> Result<Vec<KeyDirective>, ComparisonError> {
        let Ok(definition) = self.federation.key_directive_definition(self.schema) else {
            return Ok(Vec::new());
        };
        let Some(position) = self.schema.schema().types.get(type_name) else {
            return Ok(Vec::new());
        };
        position
            .directives()
            .get_all(&definition.name)
            .map(|directive| {
                let arguments =
                    self.federation
                        .key_directive_arguments(directive)
                        .map_err(|e| {
                            ComparisonError::new(format!("malformed @key on {type_name}: {e}"))
                        })?;
                Ok(KeyDirective {
                    fields: arguments.fields.to_string(),
                    resolvable: arguments.resolvable,
                })
            })
            .collect()
    }

    /// The `@requires` field set of a field, by parent type and field name.
    pub(super) fn requires(
        &self,
        type_name: &Name,
        field_name: &Name,
    ) -> Result<Option<String>, ComparisonError> {
        self.field_directive(
            self.federation.requires_directive_definition(self.schema),
            type_name,
            field_name,
            |directive| {
                self.federation
                    .requires_directive_arguments(directive)
                    .map(|arguments| arguments.fields.to_string())
            },
        )
    }

    /// The `@provides` field set of a field, by parent type and field name, parsed against this
    /// subgraph's schema at the field's own type: the model's `Subgraph.provides`.
    pub(super) fn provides(
        &self,
        type_name: &Name,
        field_name: &Name,
    ) -> Result<Option<Vec<Selection>>, ComparisonError> {
        let fields = self.field_directive(
            self.federation.provides_directive_definition(self.schema),
            type_name,
            field_name,
            |directive| {
                self.federation
                    .provides_directive_arguments(directive)
                    .map(|arguments| arguments.fields.to_string())
            },
        )?;
        let (Some(fields), Some(field)) = (fields, self.field(type_name, field_name)) else {
            return Ok(None);
        };
        parse_field_set(self.schema, field.ty.inner_named_type(), &fields).map(Some)
    }

    /// The arguments of a federation directive applied to a field, read by `arguments`: `None`
    /// when the subgraph does not define the directive, does not declare the field, or does not
    /// apply the directive to it.
    fn field_directive<T>(
        &self,
        definition: Result<&Node<DirectiveDefinition>, FederationError>,
        type_name: &Name,
        field_name: &Name,
        arguments: impl FnOnce(&Node<Directive>) -> Result<T, FederationError>,
    ) -> Result<Option<T>, ComparisonError> {
        let Ok(definition) = definition else {
            return Ok(None);
        };
        let Some(field) = self.field(type_name, field_name) else {
            return Ok(None);
        };
        let Some(directive) = field.directives.get(&definition.name) else {
            return Ok(None);
        };
        arguments(directive).map(Some).map_err(|e| {
            ComparisonError::new(format!(
                "malformed @{} on {type_name}.{field_name}: {e}",
                definition.name
            ))
        })
    }
}

/// Parses a field set written in a `@key`, `@requires` or `@provides` argument, against the schema
/// it is read in: the supergraph for a `@key` or `@requires`, the subgraph for a `@provides`.
pub(super) fn parse_field_set(
    schema: &ValidFederationSchema,
    parent_type: &Name,
    field_set: &str,
) -> Result<Vec<Selection>, ComparisonError> {
    FieldSet::parse_and_validate(
        schema.schema(),
        parent_type.clone(),
        field_set,
        "field_set.graphql",
    )
    .map(|parsed| parsed.into_inner().selection_set.selections)
    .map_err(|e| {
        ComparisonError::new(format!(
            "field set `{field_set}` is not valid on {parent_type}:\n{e}"
        ))
    })
}
