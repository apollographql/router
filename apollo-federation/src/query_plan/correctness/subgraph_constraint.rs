// Path-specific constraints imposed by subgraph schemas.

use std::sync::Arc;

use apollo_compiler::collections::IndexMap;
use apollo_compiler::collections::IndexSet;
use apollo_compiler::executable::Field;

use super::response_shape::NormalizedTypeCondition;
use super::response_shape::PossibleDefinitions;
use super::response_shape_compare::PathConstraint;
use super::CheckFailure;
use crate::error::FederationError;
use crate::internal_error;
use crate::schema::position::CompositeTypeDefinitionPosition;
use crate::schema::position::ObjectTypeDefinitionPosition;
use crate::ValidFederationSchema;

pub(crate) struct SubgraphConstraint<'a> {
    /// Reference to the all subgraph schemas in the supergraph.
    subgraphs_by_name: &'a IndexMap<Arc<str>, ValidFederationSchema>,

    /// possible_subgraphs: The set of subgraphs that are possible under the current context.
    possible_subgraphs: IndexSet<Arc<str>>,

    /// subgraph_types: The set of object types that are possible under the current context.
    /// - Note: The empty subgraph_types means all types are possible.
    subgraph_types: IndexSet<ObjectTypeDefinitionPosition>,
}

impl<'a> SubgraphConstraint<'a> {
    pub(crate) fn at_root(
        subgraphs_by_name: &'a IndexMap<Arc<str>, ValidFederationSchema>,
    ) -> Self {
        SubgraphConstraint {
            subgraphs_by_name,
            possible_subgraphs: subgraphs_by_name.keys().cloned().collect(),
            subgraph_types: Default::default(),
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
                if entity_ty_def.members.contains(&ty_pos.type_name) {
                    result.insert(subgraph_name.clone());
                }
            }
        }
        Ok(result)
    }

    // (Parent type & field type consistency in subgraphs) Considering the parent types in
    // `self.subgraph_types` and their possible subgraphs, find all object types that the field
    // can resolve to.
    fn subgraph_types_for_field(&self, field_name: &str) -> Result<Self, FederationError> {
        let mut possible_subgraphs = IndexSet::default();
        let mut subgraph_types = IndexSet::default();
        for parent_type in &self.subgraph_types {
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
                let field_type_pos = subgraph_schema.get_type(field_type_name.clone())?;
                if let Ok(composite_type) =
                    CompositeTypeDefinitionPosition::try_from(field_type_pos)
                {
                    let ground_set = subgraph_schema.possible_runtime_types(composite_type)?;
                    possible_subgraphs.insert(subgraph_name.clone());
                    subgraph_types.extend(ground_set.into_iter());
                }
            }
        }
        Ok(SubgraphConstraint {
            subgraphs_by_name: self.subgraphs_by_name,
            possible_subgraphs,
            subgraph_types,
        })
    }
}

impl<'a> PathConstraint<'a> for SubgraphConstraint<'a> {
    /// Is `ty` allowed under the subgraph constraint?
    fn allows(&self, ty: &ObjectTypeDefinitionPosition) -> bool {
        self.subgraph_types.is_empty() || self.subgraph_types.contains(ty)
    }

    /// Is `defs` feasible under the subgraph constraint?
    fn allows_any(&self, defs: &PossibleDefinitions) -> bool {
        if self.subgraph_types.is_empty() {
            return true;
        }
        let intersects = |ground_set: &[ObjectTypeDefinitionPosition]| {
            // See if `self.subgraph_types` and `ground_set` have any intersection.
            ground_set.iter().any(|ty| self.subgraph_types.contains(ty))
        };
        defs.iter()
            .any(|(type_cond, _)| intersects(type_cond.ground_set()))
    }

    fn under_type_condition(&self, type_cond: &NormalizedTypeCondition) -> Self {
        SubgraphConstraint {
            subgraphs_by_name: self.subgraphs_by_name,
            possible_subgraphs: self.possible_subgraphs.clone(),
            subgraph_types: type_cond.ground_set().iter().cloned().collect(),
        }
    }

    fn for_field(&self, representative_field: &Field) -> Result<Self, CheckFailure> {
        self.subgraph_types_for_field(&representative_field.name)
            .map_err(|e| {
                CheckFailure::new(format!(
                    "failed to compute subgraph types for {} on {:?}.\n{e}",
                    representative_field.name, self.subgraph_types,
                ))
            })
    }
}
