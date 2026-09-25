// Path-specific constraints imposed by subgraph schemas.

use std::sync::Arc;

use apollo_compiler::collections::IndexMap;
use apollo_compiler::collections::IndexSet;
use apollo_compiler::executable::Field;

use super::response_shape_compare::ComparisonError;
use super::response_shape_compare::PathConstraint;
use super::response_shape_compare::PossibleTypes;
use crate::ValidFederationSchema;
use crate::error::FederationError;
use crate::internal_error;
use crate::link::federation_spec_definition::get_federation_spec_definition_from_subgraph;
use crate::schema::position::CompositeTypeDefinitionPosition;
use crate::schema::position::ObjectTypeDefinitionPosition;
use crate::utils::FallibleIterator;

/// An extra `PathConstraint` oracle for the federated lanes: tracks the set of subgraphs that
/// are possible under the current context and derives a field's possible response types from
/// the subgraph schemas' own field definitions.
pub(crate) struct SubgraphConstraint<'a> {
    /// Reference to the all subgraph schemas in the supergraph.
    subgraphs_by_name: &'a IndexMap<Arc<str>, ValidFederationSchema>,

    /// possible_subgraphs: The set of subgraphs that are possible under the current context.
    possible_subgraphs: IndexSet<Arc<str>>,
}

/// Is the object type resolvable in the subgraph schema?
fn is_resolvable(
    ty_pos: &ObjectTypeDefinitionPosition,
    schema: &ValidFederationSchema,
) -> Result<bool, FederationError> {
    let federation_spec_definition = get_federation_spec_definition_from_subgraph(schema)?;
    let key_directive_definition = federation_spec_definition.key_directive_definition(schema)?;
    let ty_def = ty_pos.get(schema.schema())?;
    ty_def
        .directives
        .get_all(&key_directive_definition.name)
        .map(|directive| federation_spec_definition.key_directive_arguments(directive))
        .find_ok(|key_directive_application| key_directive_application.resolvable)
        .map(|result| result.is_some())
}

impl<'a> SubgraphConstraint<'a> {
    /// A constraint with no subgraph information: all subgraphs are possible.
    pub(crate) fn new(subgraphs_by_name: &'a IndexMap<Arc<str>, ValidFederationSchema>) -> Self {
        let all_subgraphs = subgraphs_by_name.keys().cloned().collect();
        SubgraphConstraint {
            subgraphs_by_name,
            possible_subgraphs: all_subgraphs,
        }
    }

    // Current subgraphs + entity subgraphs
    fn possible_subgraphs_for_type(
        &self,
        ty_pos: &ObjectTypeDefinitionPosition,
    ) -> Result<IndexSet<Arc<str>>, FederationError> {
        let mut result = self.possible_subgraphs.clone();
        for (subgraph_name, subgraph_schema) in self.subgraphs_by_name.iter() {
            if let Some(entity_ty_pos) = subgraph_schema.entity_type()? {
                let entity_ty_def = entity_ty_pos.get(subgraph_schema.schema())?;
                if entity_ty_def.members.contains(&ty_pos.type_name)
                    && is_resolvable(ty_pos, subgraph_schema)?
                {
                    result.insert(subgraph_name.clone());
                }
            }
        }
        Ok(result)
    }

    // (Parent type & field type consistency in subgraphs) Considering the field's possible parent
    // types and their possible entity subgraphs, find all object types that the field can resolve
    // to, along with the subgraphs that can resolve it.
    fn subgraph_types_for_field(
        &self,
        field_name: &str,
        parent_types: &IndexSet<ObjectTypeDefinitionPosition>,
    ) -> Result<(Self, PossibleTypes), FederationError> {
        let mut possible_subgraphs = IndexSet::default();
        let mut subgraph_types = IndexSet::default();
        for parent_type in parent_types {
            let candidate_subgraphs = self.possible_subgraphs_for_type(parent_type)?;
            for subgraph_name in candidate_subgraphs.iter() {
                let Some(subgraph_schema) = self.subgraphs_by_name.get(subgraph_name) else {
                    return Err(internal_error!("subgraph not found: {subgraph_name}"));
                };
                // check if this subgraph has the definition for `<parent_type> { <field> }`
                let Some(parent_type) = parent_type.try_get(subgraph_schema.schema()) else {
                    continue;
                };
                let Some(field) = parent_type.fields.get(field_name) else {
                    continue;
                };
                let field_type_name = field.ty.inner_named_type();
                let field_type_pos = subgraph_schema.get_type(field_type_name)?;
                if let Ok(composite_type) =
                    CompositeTypeDefinitionPosition::try_from(field_type_pos)
                {
                    let ground_set = subgraph_schema.possible_runtime_types(composite_type)?;
                    possible_subgraphs.insert(subgraph_name.clone());
                    subgraph_types.extend(ground_set);
                }
            }
        }
        let field_types = if subgraph_types.is_empty() {
            // No subgraph has a composite field definition (e.g. meta-fields).
            // Fall back to unconstrained, so the sub-selections are fully compared.
            PossibleTypes::All
        } else {
            PossibleTypes::Restricted(subgraph_types)
        };
        Ok((
            SubgraphConstraint {
                subgraphs_by_name: self.subgraphs_by_name,
                possible_subgraphs,
            },
            field_types,
        ))
    }
}

impl PathConstraint for SubgraphConstraint<'_> {
    fn for_field(
        &self,
        representative_field: &Field,
        parent_types: &PossibleTypes,
    ) -> Result<(Self, PossibleTypes), ComparisonError> {
        let PossibleTypes::Restricted(parent_types) = parent_types else {
            // Unconstrained parent types: remain unconstrained.
            return Ok((
                SubgraphConstraint::new(self.subgraphs_by_name),
                PossibleTypes::All,
            ));
        };
        self.subgraph_types_for_field(&representative_field.name, parent_types)
            .map_err(|e| {
                // Note: This is an internal federation error, not a comparison error.
                //       But, we are only allowed to return `ComparisonError` to keep the
                //       response_shape_compare module free from internal errors.
                ComparisonError::new(format!(
                    "failed to compute subgraph types for {} on {:?} due to an error:\n{e}",
                    representative_field.name, parent_types,
                ))
            })
    }
    /// The subgraphs still possible are what this narrows by, so two agree only when those do.
    fn narrows_alike(&self, other: &Self) -> bool {
        self.possible_subgraphs == other.possible_subgraphs
    }
}
