// Path-specific type constraints imposed by the API schema.

use apollo_compiler::collections::IndexSet;
use apollo_compiler::executable::Field;

use super::response_shape_compare::ComparisonError;
use super::response_shape_compare::PathConstraint;
use super::response_shape_compare::PossibleTypes;
use crate::ValidFederationSchema;
use crate::error::FederationError;
use crate::schema::position::CompositeTypeDefinitionPosition;
use crate::schema::position::ObjectTypeDefinitionPosition;

/// `PathConstraint` imposed by a schema. This is the base constraint of every comparison lane;
/// the `SubgraphConstraint` oracle can be layered on top of it via the pair `PathConstraint`
/// impl.
///
/// With a covariant field output type (e.g. `ObjectA.next: ObjectA!` narrowing
/// `Entity.next: Entity!`), the possible types of a field's response depend on which runtime
/// type its parent turned out to be. `for_field` derives the field's possible response types
/// from each possible runtime type's own field definition.
///
/// The schema must define the full type universe of the comparison: every type and field that
/// either response shape may reference, so that the derived possible types never
/// under-approximate and narrowing never skips a feasible case. In the query-plan lanes, that
/// is the supergraph schema (not the API schema): the plan-side shapes and the
/// `@requires`/`@key` condition shapes may use any fields from the supergraph schema, while the
/// operation side is constrained to the API schema, a subset of the supergraph schema. In
/// `compare_operations`, it is the schema both operations are defined against.
pub(crate) struct SchemaConstraint<'a> {
    schema: &'a ValidFederationSchema,
}

impl<'a> SchemaConstraint<'a> {
    pub(crate) fn new(schema: &'a ValidFederationSchema) -> Self {
        SchemaConstraint { schema }
    }

    /// (Parent type & field type consistency) Considering the field's possible parent types,
    /// find all object types that the field can resolve to.
    fn possible_types_for_field(
        &self,
        field_name: &str,
        parent_types: &IndexSet<ObjectTypeDefinitionPosition>,
    ) -> Result<PossibleTypes, FederationError> {
        let mut possible_types = IndexSet::default();
        for parent_type in parent_types {
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
        if possible_types.is_empty() {
            // No parent type has a composite field definition (e.g. meta-fields like
            // `__schema`). Fall back to unconstrained, so the sub-selections are fully compared.
            Ok(PossibleTypes::All)
        } else {
            Ok(PossibleTypes::Restricted(possible_types))
        }
    }
}

impl PathConstraint for SchemaConstraint<'_> {
    fn for_field(
        &self,
        representative_field: &Field,
        parent_types: &PossibleTypes,
    ) -> Result<(Self, PossibleTypes), ComparisonError> {
        let field_types = match parent_types {
            PossibleTypes::All => PossibleTypes::All,
            PossibleTypes::Restricted(parent_types) => self
                .possible_types_for_field(&representative_field.name, parent_types)
                .map_err(|e| {
                    // Note: This is an internal federation error, not a comparison error.
                    //       But, we are only allowed to return `ComparisonError` to keep the
                    //       response_shape_compare module free from internal errors.
                    ComparisonError::new(format!(
                        "failed to compute possible types for {} on {:?} due to an error:\n{e}",
                        representative_field.name, parent_types,
                    ))
                })?,
        };
        Ok((SchemaConstraint::new(self.schema), field_types))
    }
}
