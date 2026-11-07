//! The federation metadata a subgraph schema carries: `@key`, `@requires`, `@external`,
//! `@provides`, `@context` and `@fromContext`.
//!
//! The model keeps these beside the schema (`Subgraph.keys`, `Subgraph.requires`) because its
//! `Schema` does not model directives. Here they are read from the subgraph schema itself, which
//! is what the legacy checker does.
//!
//! Field sets are written as strings in the directives and parsed against the **supergraph**
//! schema, not the subgraph's: they name the requirement the plan must already have satisfied, and
//! that requirement is compared against selections written in supergraph terms.

use apollo_compiler::Name;
use apollo_compiler::executable::FieldSet;
use apollo_compiler::executable::Selection;
use apollo_compiler::schema::ExtendedType;

use super::ComparisonError;
use super::selections::type_includes_object;
use crate::link::federation_spec_definition::FederationSpecDefinition;
use crate::link::federation_spec_definition::get_federation_spec_definition_from_subgraph;
use crate::schema::ValidFederationSchema;
use crate::schema::position::FieldDefinitionPosition;
use crate::schema::position::ObjectFieldDefinitionPosition;
use crate::schema::validators::from_context::parse_context;

/// One `@key` application on an entity type in a subgraph.
pub(super) struct KeyDirective {
    /// The key's field set, as written.
    pub(super) fields: String,
    /// Whether the subgraph will resolve the entity from this key. An entity fetch may only rely
    /// on a resolvable key.
    pub(super) resolvable: bool,
}

/// One `@fromContext` argument of a field: the argument's name, and the context and selection
/// `parse_context` splits the directive's `field` into (the model's `ContextArgument`). The
/// selection is kept as written; [`super::context`] reads it.
#[derive(Clone, PartialEq, Eq)]
pub(super) struct ContextArgument {
    pub(super) argument_name: Name,
    pub(super) context: String,
    pub(super) selection: String,
}

/// A subgraph's federation metadata.
pub(super) struct Subgraph<'a> {
    schema: &'a ValidFederationSchema,
    federation: &'a FederationSpecDefinition,
}

impl<'a> Subgraph<'a> {
    pub(super) fn new(schema: &'a ValidFederationSchema) -> Result<Self, ComparisonError> {
        let federation = get_federation_spec_definition_from_subgraph(schema).map_err(|e| {
            ComparisonError::new(format!("subgraph has no federation spec definition: {e}"))
        })?;
        Ok(Subgraph { schema, federation })
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

    /// Whether an object of type `object_type`, sent with `__typename` `sent`, resolves to an
    /// object entity case `entity_type` applies to (the model's `SentTypeResolvesToCase`): `sent`
    /// is the case's type, or `sent` and the case both include `object_type` in this subgraph, as
    /// the object's own type or an entity interface over it does. An interface object is an object
    /// type here, so it includes no implementation and is met only by the first disjunct.
    pub(super) fn sent_type_resolves_to_case(
        &self,
        sent: &Name,
        object_type: &Name,
        entity_type: &Name,
    ) -> bool {
        sent == entity_type
            || (type_includes_object(self.schema, sent, object_type)
                && type_includes_object(self.schema, entity_type, object_type))
    }

    /// The `@requires` field set of a field, by parent type and field name.
    pub(super) fn requires(
        &self,
        type_name: &Name,
        field_name: &Name,
    ) -> Result<Option<String>, ComparisonError> {
        let Ok(definition) = self.federation.requires_directive_definition(self.schema) else {
            return Ok(None);
        };
        let field = match self.schema.schema().types.get(type_name) {
            Some(ExtendedType::Object(ty)) => ty.fields.get(field_name),
            Some(ExtendedType::Interface(ty)) => ty.fields.get(field_name),
            _ => None,
        };
        let Some(field) = field else {
            return Ok(None);
        };
        let Some(directive) = field.directives.get(&definition.name) else {
            return Ok(None);
        };
        let arguments = self
            .federation
            .requires_directive_arguments(directive)
            .map_err(|e| {
                ComparisonError::new(format!(
                    "malformed @requires on {type_name}.{field_name}: {e}"
                ))
            })?;
        Ok(Some(arguments.fields.to_string()))
    }

    /// Whether a field of an object type is `@external`, as `ExternalMetadata::is_external` reads
    /// it: a fake external, a key field Fed 1 marks external on an extension, is not.
    pub(super) fn is_external(&self, type_name: &Name, field_name: &Name) -> bool {
        self.schema.subgraph_metadata().is_some_and(|metadata| {
            metadata.is_field_external(&FieldDefinitionPosition::Object(
                ObjectFieldDefinitionPosition {
                    type_name: type_name.clone(),
                    field_name: field_name.clone(),
                },
            ))
        })
    }

    /// The `@provides` field set of a field of an object type, as written.
    pub(super) fn provides(
        &self,
        type_name: &Name,
        field_name: &Name,
    ) -> Result<Option<String>, ComparisonError> {
        let Ok(definition) = self.federation.provides_directive_definition(self.schema) else {
            return Ok(None);
        };
        let Some(ExtendedType::Object(ty)) = self.schema.schema().types.get(type_name) else {
            return Ok(None);
        };
        let Some(directive) = ty
            .fields
            .get(field_name)
            .and_then(|field| field.directives.get(&definition.name))
        else {
            return Ok(None);
        };
        let arguments = self
            .federation
            .provides_directive_arguments(directive)
            .map_err(|e| {
                ComparisonError::new(format!(
                    "malformed @provides on {type_name}.{field_name}: {e}"
                ))
            })?;
        Ok(Some(arguments.fields.to_string()))
    }
}

impl Subgraph<'_> {
    /// Whether any field argument carries `@fromContext`, read off the schema's referencers. A
    /// subgraph with none has no contextual use to walk an operation for.
    pub(super) fn declares_context_arguments(&self) -> bool {
        let Ok(definition) = self
            .federation
            .from_context_directive_definition(self.schema)
        else {
            return false;
        };
        let applied = self.schema.referencers().get_directive(&definition.name);
        !applied.object_field_arguments.is_empty() || !applied.interface_field_arguments.is_empty()
    }

    /// The `@fromContext` arguments of a field, by parent type and field name.
    pub(super) fn context_arguments(
        &self,
        type_name: &Name,
        field_name: &Name,
    ) -> Result<Vec<ContextArgument>, ComparisonError> {
        let Ok(definition) = self
            .federation
            .from_context_directive_definition(self.schema)
        else {
            return Ok(Vec::new());
        };
        let field = match self.schema.schema().types.get(type_name) {
            Some(ExtendedType::Object(ty)) => ty.fields.get(field_name),
            Some(ExtendedType::Interface(ty)) => ty.fields.get(field_name),
            _ => None,
        };
        let Some(field) = field else {
            return Ok(Vec::new());
        };
        let mut declared = Vec::new();
        for argument in &field.arguments {
            let Some(directive) = argument.directives.get(&definition.name) else {
                continue;
            };
            let malformed = |reason: String| {
                ComparisonError::new(format!(
                    "malformed @fromContext on {type_name}.{field_name}({}:): {reason}",
                    argument.name
                ))
            };
            let arguments = self
                .federation
                .from_context_directive_arguments(directive)
                .map_err(|e| malformed(e.to_string()))?;
            let (Some(context), Some(selection)) = parse_context(arguments.field) else {
                return Err(malformed(format!(
                    "`{}` names no context and selection",
                    arguments.field
                )));
            };
            declared.push(ContextArgument {
                argument_name: argument.name.clone(),
                context,
                selection,
            });
        }
        Ok(declared)
    }

    /// The types setting a context, `@context(name:)` on an object, an interface or a union, in
    /// declaration order.
    pub(super) fn context_types(&self, context: &str) -> Vec<Name> {
        let Ok(definition) = self.federation.context_directive_definition(self.schema) else {
            return Vec::new();
        };
        self.schema
            .schema()
            .types
            .iter()
            .filter(|(_, ty)| {
                matches!(
                    ty,
                    ExtendedType::Object(_) | ExtendedType::Interface(_) | ExtendedType::Union(_)
                ) && ty.directives().get_all(&definition.name).any(|directive| {
                    self.federation
                        .context_directive_arguments(directive)
                        .is_ok_and(|arguments| arguments.name == context)
                })
            })
            .map(|(name, _)| name.clone())
            .collect()
    }
}

/// Parses a field set written in a `@key` or `@requires` argument, against the supergraph schema.
pub(super) fn parse_field_set(
    supergraph_schema: &ValidFederationSchema,
    parent_type: &Name,
    field_set: &str,
) -> Result<Vec<Selection>, ComparisonError> {
    FieldSet::parse_and_validate(
        supergraph_schema.schema(),
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
