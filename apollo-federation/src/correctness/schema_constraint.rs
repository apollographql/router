// Path-specific type constraints imposed by the API schema.

use apollo_compiler::collections::IndexSet;
use apollo_compiler::executable::Field;

use super::response_shape::NormalizedTypeCondition;
use super::response_shape::PossibleDefinitions;
use super::response_shape_compare::ComparisonError;
use super::response_shape_compare::PathConstraint;
use crate::ValidFederationSchema;
use crate::error::FederationError;
use crate::schema::position::CompositeTypeDefinitionPosition;
use crate::schema::position::ObjectTypeDefinitionPosition;

/// `PathConstraint` imposed by a schema. This is the base constraint of every comparison lane;
/// the `SubgraphConstraint` oracle can be layered on top of it via the pair `PathConstraint`
/// impl.
///
/// The schema must define the full type universe of the comparison: every type and field that
/// either response shape may reference, so that `possible_types` never under-approximates and
/// narrowing never skips a feasible case. In the query-plan lanes, that is the supergraph
/// schema (not the API schema): the plan-side shapes and the `@requires`/`@key` condition
/// shapes may use any fields from the supergraph schema, while the operation side is
/// constrained to the API schema, a subset of the supergraph schema. In `compare_operations`,
/// it is the schema both operations are defined against.
///
/// This tracks the set of runtime object types that are possible at the current path position.
/// When `compare_possible_definitions` case-splits a type condition over its ground types, the
/// case under consideration must remain visible while comparing sub-selection shapes: with a
/// covariant field return (e.g. `ObjectA.next: ObjectA!` narrowing `Entity.next: Entity!`), the
/// possible types of a field's response depend on which runtime type its parent turned out to
/// be. `under_type_condition` records the case and `for_field` re-derives the field's possible
/// response types from each possible runtime type's own field definition.
pub(crate) struct SchemaConstraint<'a> {
    schema: &'a ValidFederationSchema,

    /// The set of object types that are possible under the current context.
    /// - Note: The empty set means all types are possible (unconstrained).
    possible_types: IndexSet<ObjectTypeDefinitionPosition>,
}

impl<'a> SchemaConstraint<'a> {
    /// A constraint with no type information: all types are possible. The comparison may start
    /// at any scope (e.g. the entity type for `@requires`/`@key` conditions); the first type
    /// condition encountered narrows the constraint to that scope.
    pub(crate) fn new(schema: &'a ValidFederationSchema) -> Self {
        SchemaConstraint {
            schema,
            possible_types: Default::default(),
        }
    }

    /// (Parent type & field type consistency) Considering the field's possible parent types
    /// (`self.possible_types`), find all object types that the field can resolve to.
    fn possible_types_for_field(&self, field_name: &str) -> Result<Self, FederationError> {
        let mut possible_types = IndexSet::default();
        for parent_type in &self.possible_types {
            let parent_type_def = parent_type.get(self.schema.schema())?;
            // Skip parent types without the field definition (e.g. meta-fields).
            let Some(field) = parent_type_def.fields.get(field_name) else {
                continue;
            };
            let field_type_pos = self.schema.get_type(field.ty.inner_named_type())?;
            if let Ok(composite_type) = CompositeTypeDefinitionPosition::try_from(field_type_pos) {
                possible_types.extend(self.schema.possible_runtime_types(composite_type)?);
            }
        }
        Ok(SchemaConstraint {
            schema: self.schema,
            possible_types,
        })
    }
}

impl PathConstraint for SchemaConstraint<'_> {
    fn under_type_condition(&self, type_cond: &NormalizedTypeCondition) -> Self {
        let ground_set = type_cond.ground_set().iter();
        let possible_types = if self.possible_types.is_empty() {
            ground_set.cloned().collect()
        } else {
            // Both the current constraint and the type condition apply here.
            // - Callers only use satisfiable type conditions, so the intersection is non-empty.
            ground_set
                .filter(|ty| self.possible_types.contains(*ty))
                .cloned()
                .collect()
        };
        SchemaConstraint {
            schema: self.schema,
            possible_types,
        }
    }

    fn for_field(&self, representative_field: &Field) -> Result<Self, ComparisonError> {
        self.possible_types_for_field(&representative_field.name)
            .map_err(|e| {
                // Note: This is an internal federation error, not a comparison error.
                //       But, we are only allowed to return `ComparisonError` to keep the
                //       response_shape_compare module free from internal errors.
                ComparisonError::new(format!(
                    "failed to compute possible types for {} on {:?} due to an error:\n{e}",
                    representative_field.name, self.possible_types,
                ))
            })
    }

    fn allows(&self, ty: &ObjectTypeDefinitionPosition) -> bool {
        self.possible_types.is_empty() || self.possible_types.contains(ty)
    }

    fn allows_any(&self, defs: &PossibleDefinitions) -> bool {
        if self.possible_types.is_empty() {
            return true;
        }
        let intersects = |ground_set: &[ObjectTypeDefinitionPosition]| {
            // See if `self.possible_types` and `ground_set` have any intersection.
            ground_set.iter().any(|ty| self.possible_types.contains(ty))
        };
        defs.iter()
            .any(|(type_cond, _)| intersects(type_cond.ground_set()))
    }
}
