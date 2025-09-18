use std::fmt;

use apollo_compiler::Node;
use apollo_compiler::ast;
use apollo_compiler::executable;
use apollo_compiler::executable::FieldSet;
use apollo_compiler::executable::Name;
use itertools::Itertools;

use super::query_plan_analysis::AnalysisContext;
use super::response_shape::Clause;
use super::response_shape::PossibleDefinitions;
use super::response_shape::ResponseShape;
use super::response_shape::compute_response_shape_for_selection_set;
use super::response_shape_compare::compare_representative_field;
use super::response_shape_compare::compare_response_shapes_with_constraint;
use super::subgraph_constraint::SubgraphConstraint;
use crate::FederationError;
use crate::bail;
use crate::internal_error;
use crate::link::federation_spec_definition::FederationSpecDefinition;
use crate::link::federation_spec_definition::KeyDirectiveArguments;
use crate::link::federation_spec_definition::get_federation_spec_definition_from_subgraph;
use crate::query_plan::requires_selection;
use crate::schema::ValidFederationSchema;
use crate::schema::position::CompositeTypeDefinitionPosition;
use crate::schema::position::INTROSPECTION_TYPENAME_FIELD_NAME;
use crate::schema::position::TypeDefinitionPosition;
use crate::utils::FallibleIterator;

/// Query plan analysis error enum
#[derive(Debug, derive_more::From)]
pub(crate) enum AnalysisErrorMessage {
    /// Correctness checker's internal error
    FederationError(FederationError),
    /// Error in the input query plan
    QueryPlanError(String),
}

pub(crate) struct AnalysisError {
    message: AnalysisErrorMessage,
    context: Vec<String>,
}

impl AnalysisError {
    pub(crate) fn with_context(mut self, context: impl Into<String>) -> Self {
        self.context.push(context.into());
        self
    }
}

impl fmt::Display for AnalysisErrorMessage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AnalysisErrorMessage::FederationError(err) => write!(f, "{err}"),
            AnalysisErrorMessage::QueryPlanError(err) => write!(f, "{err}"),
        }
    }
}

impl fmt::Display for AnalysisError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "{}", self.message)?;
        let num_contexts = self.context.len();
        for (i, ctx) in self.context.iter().enumerate() {
            writeln!(f, "[{index}/{num_contexts}] {ctx}", index = i + 1)?;
        }
        Ok(())
    }
}

impl From<FederationError> for AnalysisError {
    fn from(err: FederationError) -> Self {
        AnalysisError {
            message: AnalysisErrorMessage::FederationError(err),
            context: vec![],
        }
    }
}

impl From<String> for AnalysisError {
    fn from(err: String) -> Self {
        AnalysisError {
            message: AnalysisErrorMessage::QueryPlanError(err),
            context: vec![],
        }
    }
}

//==================================================================================================
// Checking subgraph fetch requirements

fn compute_response_shape_for_field_set(
    schema: &ValidFederationSchema,
    parent_type: Name,
    field_set: &str,
) -> Result<ResponseShape, FederationError> {
    // Similar to `crate::schema::field_set::parse_field_set` function.
    let field_set =
        FieldSet::parse_and_validate(schema.schema(), parent_type, field_set, "field_set.graphql")?;
    compute_response_shape_for_selection_set(schema, &field_set.selection_set)
}

fn compute_response_shape_for_field_set_with_typename(
    schema: &ValidFederationSchema,
    parent_type: Name,
    field_set: &str,
) -> Result<ResponseShape, FederationError> {
    // Similar to `crate::schema::field_set::parse_field_set` function.
    let field_set =
        FieldSet::parse_and_validate(schema.schema(), parent_type, field_set, "field_set.graphql")?;
    let mut selection_set = field_set.into_inner().selection_set;
    let typename = selection_set
        .new_field(schema.schema(), INTROSPECTION_TYPENAME_FIELD_NAME.clone())
        .map_err(|_| {
            internal_error!(
                "Unexpected error: {} not found in schema",
                INTROSPECTION_TYPENAME_FIELD_NAME
            )
        })?;
    selection_set.push(typename);
    compute_response_shape_for_selection_set(schema, &selection_set)
}

/// Used for FetchNode's `requires` field values
/// - The `requires` items are a single inline fragment.
/// - Computes a response shape of the inline fragment and returns it.
fn compute_response_shape_for_require_selection(
    schema: &ValidFederationSchema,
    require: &requires_selection::Selection,
) -> Result<ResponseShape, FederationError> {
    let requires_selection::Selection::InlineFragment(inline) = require else {
        bail!("Expected require selection to be an inline fragment, but got: {require:#?}")
    };
    let Some(type_condition) = &inline.type_condition else {
        bail!("Expected a type condition on require inline fragment")
    };
    // Convert the `inline`'s selection set into the `executable::SelectionSet` type,
    // so we can use `compute_response_shape_for_selection_set` function.
    let selections = convert_requires_selections(schema, type_condition, &inline.selections)?;
    let mut selection_set = executable::SelectionSet::new(type_condition.clone());
    selection_set.extend(selections);
    compute_response_shape_for_selection_set(schema, &selection_set)
}

/// Converts a `requires_selection::Selection` array into a Vec of `executable::Selection`.
fn convert_requires_selections(
    schema: &ValidFederationSchema,
    ty: &Name,
    selections: &[requires_selection::Selection],
) -> Result<Vec<executable::Selection>, FederationError> {
    let mut result = Vec::new();
    for selection in selections {
        match selection {
            requires_selection::Selection::Field(field) => {
                let field_def = get_field_definition(schema, ty, &field.name)?;
                // Note: `field` has no alias, arguments nor directive applications.
                let converted = executable::Field::new(field.name.clone(), field_def);
                let converted = if field.selections.is_empty() {
                    converted
                } else {
                    let sub_ty = converted.ty().inner_named_type().clone();
                    let sub_selections =
                        convert_requires_selections(schema, &sub_ty, &field.selections)?;
                    converted.with_selections(sub_selections)
                };
                result.push(converted.into());
            }
            requires_selection::Selection::InlineFragment(inline) => {
                let converted = if let Some(type_condition) = &inline.type_condition {
                    executable::InlineFragment::with_type_condition(type_condition.clone())
                } else {
                    executable::InlineFragment::without_type_condition(ty.clone())
                };
                let sub_ty = converted.selection_set.ty.clone();
                let sub_selections =
                    convert_requires_selections(schema, &sub_ty, &inline.selections)?;
                result.push(converted.with_selections(sub_selections).into());
            }
        }
    }
    Ok(result)
}

/// Get the `ast::FieldDefinition` for the given type and field name from the schema.
fn get_field_definition(
    schema: &ValidFederationSchema,
    parent_type: &Name,
    field_name: &Name,
) -> Result<Node<ast::FieldDefinition>, FederationError> {
    let parent_type_pos: CompositeTypeDefinitionPosition =
        schema.get_type(parent_type.clone())?.try_into()?;
    let field_def_pos = parent_type_pos.field(field_name.clone())?;
    field_def_pos
        .get(schema.schema())
        .map(|component| component.node.clone())
        .map_err(|err| err.into())
}

/// Collect `@requires` conditions from the fields used in the fetch response shape based on their
/// definitions in the subgraph schema.
/// - `response_shape`: the fetch response shape for the entity
/// - Only consider the fields at the root level of the response shape.
/// - The fields may have Boolean conditions. Then, the computed conditions will inherit them.
/// - The collected conditions are merged into a single response shape.
fn collect_require_condition(
    supergraph_schema: &ValidFederationSchema,
    subgraph_schema: &ValidFederationSchema,
    federation_spec_definition: &FederationSpecDefinition,
    response_shape: &ResponseShape,
) -> Result<ResponseShape, FederationError> {
    let requires_directive_definition =
        federation_spec_definition.requires_directive_definition(subgraph_schema)?;
    let parent_type = response_shape.default_type_condition();
    let mut result = ResponseShape::new(parent_type.clone());
    let all_variants = response_shape
        .iter()
        .flat_map(|(_key, defs)| defs.iter())
        .flat_map(|(_, per_type_cond)| per_type_cond.conditional_variants());
    for variant in all_variants {
        let field_def = &variant.representative_field().definition;
        for directive in field_def
            .directives
            .get_all(&requires_directive_definition.name)
        {
            let requires_application =
                federation_spec_definition.requires_directive_arguments(directive)?;
            let rs = compute_response_shape_for_field_set(
                supergraph_schema,
                parent_type.clone(),
                requires_application.fields,
            )?;
            result.merge_with(&rs.add_boolean_conditions(variant.boolean_clause()))?;
        }
    }
    Ok(result)
}

/// Check if the `@requires` and `@key` conditions match a `requires` item & the current state.
/// - `type_condition`: the type condition for the entity fetch
/// - `boolean_clause`: the Boolean condition on the fetch node
/// - `entity_require_shape`: the response shape of the `requires` item specified on the fetch node
/// - `require_condition`: the response shape of the computed require condition (from `@requires`).
/// - Note: `entity_require_shape`'s type condition only needs to be a subset of the
///   `type_condition`. So, they don't have to be the same. It happens when the entity's
///   subgraph type is an interface object type.
/// - The key directive and `require_condition` put together must have the same set of response
///   keys as the `entity_require_shape`.
fn key_directive_matches(
    context: &AnalysisContext,
    state: &ResponseShape,
    boolean_clause: &Clause,
    entity_require_shape: &ResponseShape,
    require_condition: &ResponseShape,
    key_directive_application: &KeyDirectiveArguments<'_>,
) -> Result<(), AnalysisError> {
    // Note: We use the `entity_require_shape`'s type to interpret the `@key` directive's field
    //       set, instead of the entity's actual type, since the fetch node's `requires` item may
    //       be on a more specific type than the entity's type in subgraph.
    let key_type_condition = entity_require_shape.default_type_condition();
    let key_condition = compute_response_shape_for_field_set_with_typename(
        context.supergraph_schema(),
        key_type_condition.clone(),
        key_directive_application.fields,
    )?;
    // `condition`: the whole condition computed from the fetch query & subgraph schema.
    let mut condition = key_condition.clone();
    condition.merge_with(require_condition)?;
    // Check if `entity_require_shape` is a subset of `condition` in terms of response keys.
    if !key_only_compare_response_shapes(entity_require_shape, &condition) {
        return Err(format!(
            "The `requires` item does not match the subgraph schema\n\
             * @key field set: {key_condition}\n\
             * @requires field set: {require_condition}"
        )
        .into());
    }
    let final_require_shape = condition.add_boolean_conditions(boolean_clause);
    let path_constraint = SubgraphConstraint::at_root(context.subgraphs_by_name());
    let assumption = Clause::default(); // empty assumption at the top level
    compare_response_shapes_with_constraint(
        &path_constraint,
        &assumption,
        &final_require_shape,
        state,
    )
    .map_err(|e| {
        format!(
            "The state does not satisfy the subgraph schema requirements:\n\
                 * schema requires: ({condition_type}) {condition}\n\
                 * Comparison error:\n{e}\n\
                 * state: ({state_type}) {state}",
            condition_type = condition.default_type_condition(),
            state_type = state.default_type_condition(),
        )
        .into()
    })
}

/// Check if the entity's `@key`/`@requires` conditions are satisfied in the current `state`.
/// Also, checks if the fetch node's `requires` item for the entity matches the `@key`/`@requires`
/// conditions.
/// - `state`: the input state
/// - `boolean_clause`: the fetch node's Boolean conditions
/// - `entity_response_shape`: the fetch node's response shape for the entity
/// - `entity_require`: the fetch node's `requires` array item for the entity
fn check_require(
    context: &AnalysisContext,
    subgraph_schema: &ValidFederationSchema,
    state: &ResponseShape,
    boolean_clause: &Clause,
    entity_response_shape: &ResponseShape,
    entity_require: &requires_selection::Selection,
) -> Result<(), AnalysisError> {
    let subgraph_entity_type_name = entity_response_shape.default_type_condition();
    let subgraph_entity_type_pos = subgraph_schema.get_type(subgraph_entity_type_name.clone())?;
    let directives = match &subgraph_entity_type_pos {
        TypeDefinitionPosition::Object(type_pos) => {
            let type_def = type_pos
                .get(subgraph_schema.schema())
                .map_err(FederationError::from)?;
            &type_def.directives
        }
        TypeDefinitionPosition::Interface(type_pos) => {
            let type_def = type_pos
                .get(subgraph_schema.schema())
                .map_err(FederationError::from)?;
            &type_def.directives
        }
        _ => bail!("check_require: unexpected kind of entity type: {subgraph_entity_type_name}"),
    };

    let entity_require_shape =
        compute_response_shape_for_require_selection(context.supergraph_schema(), entity_require)
            .map_err(|err| {
            format!(
                "check_require: failed to compute response shape:\n{err}\n\
                    require selection: {entity_require}"
            )
        })?;

    let federation_spec_definition = get_federation_spec_definition_from_subgraph(subgraph_schema)?;
    // Collect all applicable `@requires` field sets and put them in a response shape.
    let require_condition = collect_require_condition(
        context.supergraph_schema(),
        subgraph_schema,
        federation_spec_definition,
        entity_response_shape,
    )
    .map_err(|err| {
        format!(
            "check_require: failed to collect require conditions from the subgraph schema:\n\
                {err}\n\
                entity_response_shape: {entity_response_shape}"
        )
    })?;

    // Find the matching `@key` directive
    // - Note: The type condition may have multiple `@key` directives. Try find one that works.
    let key_directive_definition =
        federation_spec_definition.key_directive_definition(subgraph_schema)?;
    let mut mismatch_cases = Vec::new();
    let mut unresolvable_cases = Vec::new();
    let found = directives
        .get_all(&key_directive_definition.name)
        .map(|directive| federation_spec_definition.key_directive_arguments(directive))
        .ok_and_any(|ref key_directive_application| {
            match key_directive_matches(
                context,
                state,
                boolean_clause,
                &entity_require_shape,
                &require_condition,
                key_directive_application,
            ) {
                Ok(_) => {
                    if key_directive_application.resolvable {
                        true
                    } else {
                        unresolvable_cases.push(format!(
                            "The matched @key directive is not resolvable.\n\
                             * @key field set: {key_field_set}",
                            key_field_set = key_directive_application.fields
                        ));
                        false
                    }
                }
                Err(e) => {
                    mismatch_cases.push(e);
                    false
                }
            }
        })?;
    if found {
        Ok(())
    } else {
        // soundness error
        if unresolvable_cases.is_empty() {
            if mismatch_cases.len() == 1 {
                Err(format!(
                    "check_require: no matching require condition found (@key didn't match)\n\
                     * plan requires: {entity_require_shape}\n\
                     Mismatch description:\n{mismatch_cases}",
                    mismatch_cases = mismatch_cases.iter().map(|e| e.to_string()).join("\n")
                )
                .into())
            } else {
                Err(format!(
                    "check_require: no matching require condition found (all @key directives failed to match)\n\
                     * plan requires: {entity_require_shape}\n\
                     Mismatches:\n{mismatch_cases}",
                     mismatch_cases = mismatch_cases.iter().enumerate().map(|(i, e)|
                        format!("[{index}/{bound}] {e}", index=i+1, bound=mismatch_cases.len())
                    ).join("\n")
                ).into())
            }
        } else {
            Err(format!(
                "check_require: @key matched, but none of them are resolvable.\n\
                    * plan requires: {entity_require_shape}\n\
                    Unresolvable cases:\n{unresolvable_cases}",
                unresolvable_cases = unresolvable_cases
                    .iter()
                    .enumerate()
                    .map(|(i, e)| format!(
                        "[{index}/{bound}] {e}",
                        index = i + 1,
                        bound = unresolvable_cases.len()
                    ))
                    .join("\n")
            )
            .into())
        }
    }
}

/// Check subgraph requirements for all entity fetches
/// - `state`: the input state
/// - `boolean_clause`: the fetch node's Boolean conditions
/// - `response_shapes`: the fetch node's response shape for entities
/// - `requires`: the fetch node's `requires` field value for entities
pub(crate) fn check_requires(
    context: &AnalysisContext,
    subgraph_schema: &ValidFederationSchema,
    state: &ResponseShape,
    boolean_clause: &Clause,
    response_shapes: &[ResponseShape],
    requires: &[requires_selection::Selection],
) -> Result<(), AnalysisError> {
    // The `requires` array and `response_shape` array should match. So, usually they are 1:1.
    // However, the entity fetch operation may be simplified by merging identical entity fetches.
    // In that case, the `requires` array may have more items than the `response_shape` array and
    // we need to find a corresponding entity response shape item for each `requires` item.
    match requires.len().cmp(&response_shapes.len()) {
        std::cmp::Ordering::Less => Err(
            "check_requires: Fewer number of requires items than entity fetch cases"
                .to_string()
                .into(),
        ),
        std::cmp::Ordering::Equal => {
            // 1:1 match
            for (rs, require) in response_shapes.iter().zip(requires.iter()) {
                check_require(
                    context,
                    subgraph_schema,
                    state,
                    boolean_clause,
                    rs,
                    require,
                )
                .map_err(|e| {
                    e.with_context(format!(
                        "check_requires: Subgraph require check failed for type condition: {type_condition}",
                        type_condition = rs.default_type_condition()
                    ))
                })?;
            }
            Ok(())
        }
        std::cmp::Ordering::Greater => {
            // 1:many => check if each `requires` item has a matching entity fetch case
            requires.iter().try_for_each(|require| {
                let mut errors = Vec::new();
                let has_any = response_shapes.iter().any(|rs| {
                    match check_require(
                        context,
                        subgraph_schema,
                        state,
                        boolean_clause,
                        rs,
                        require,
                    ) {
                        Ok(_) => true,
                        Err(e) => { errors.push(e); false }
                    }
                });
                if has_any {
                    Ok(())
                } else {
                    Err(format!(
                        "check_requires: Subgraph require check failed to find a matching entity fetch for the requires item: {require}\nErrors:\n{errors}",
                        errors = errors.iter().enumerate().map(|(i, e)|
                            format!("[{index}/{bound}] {e}", index=i+1, bound=errors.len())
                        ).join("\n"),
                    ).into())
                }
            })
        }
    }
}

//==================================================================================================
// Key-only ResponseShape comparison
// - This is used to verify that each `requires` item on the fetch node represents the actual
//   subgraph constraints specified in the subgraph schema's `@key/@requires` directives.
// - The `requires` items on FetchNode only have the response key name (field name) without
//   arguments, directives nor aliases.
//   * Thus, their definitions (and response shapes) won't have Boolean conditions.
// - `@key/@requires` directive's field set selections can't have aliases nor directives.
//   * Note: `@requires` response shape inherits the Boolean condition from their individual entity
//           fetch selections. Thus, Boolean conditions can be different and one response key
//           may have multiple Boolean variants.
// - `requires` items and `@key/@requires` directive's field set selections have this common
//   property: Their response keys will always be the same as their definition's field name.
//   * Even though the field arguments are missing in the `requires` items, since aliasing is not
//     allowed, we can still match them just by their their response keys.
// - Note: The type condition on `requires` item can be different from its corresponding entity
//   type. The entity type can be a superset of the `requires` item's type condition, when the
//   entity type in the subgraph is an interface object type.
// - Argument convention: For each comparison function,
//   * `this` is the response shape of the `requires` item.
//   * `other` is the response shape of the `@key/@requires` directives from the subgraph schema.
// - Essentially, this module is a simplified version of the `response_shape_compare` module.
//
// Example fetch node:
//   FetchNode(service_name: "supply") {
//     ... on Movie { # suppose `Movie` implements the `Product` interface.
//       id # from a `@key` directive
//       data # from a `@requires` directive (but the argument is missing)
//     }
//   } => {
//     ... on Product { # `Product` is an interface object type in this subgraph.
//       name
//       sku @include(if: $includeSku)
//           # The `@requires` response shape inherits this Boolean condition.
//     }
//   }
//
// Suppose the `Product` type is defined in the subgraph schema as following,
//   type Product @key(fields: "id") {
//      id: ID!
//      name: String!
//      sku: String! @requires(fields: "data(arg: 42)")
//   }

mod key_only_response_shape_compare {
    use super::super::response_shape::DefinitionVariant;
    use super::super::response_shape::PossibleDefinitionsPerTypeCondition;
    use super::*;

    /// Check if the key set of `this` is the same as that of `other` (ignoring their definitions)
    pub(super) fn key_only_compare_response_shapes(
        this: &ResponseShape,
        other: &ResponseShape,
    ) -> bool {
        // Should have the exact same set of response keys.
        this.len() == other.len()
            && this.iter().all(|(key, this_def)| {
                let Some(other_def) = other.get(key) else {
                    return false;
                };
                key_only_compare_possible_definitions(this_def, other_def)
            })
    }

    fn key_only_compare_possible_definitions(
        this: &PossibleDefinitions,
        other: &PossibleDefinitions,
    ) -> bool {
        // Should have the exact same set of type conditions.
        this.len() == other.len()
            && this.iter().all(|(this_cond, this_def)| {
                let Some(other_def) = other.get(this_cond) else {
                    return false;
                };
                key_only_compare_possible_definitions_per_type_condition(this_def, other_def)
            })
    }

    fn key_only_compare_possible_definitions_per_type_condition(
        this: &PossibleDefinitionsPerTypeCondition,
        other: &PossibleDefinitionsPerTypeCondition,
    ) -> bool {
        // The `this` should have exactly one variant with no Boolean conditions, since the
        // `requires` item won't have this detail.
        // On the other hand, the `other` can have variants with Boolean conditions. But, the
        // variants are merged since their Boolean conditions are ignored.
        if this.conditional_variants().len() != 1 {
            return false;
        }
        this.conditional_variants().iter().all(|this_def| {
            let Some(merged_def) = merge_variants_ignoring_boolean_conditions(other) else {
                return false;
            };
            key_only_compare_definition_variant(this_def, &merged_def)
        })
    }

    fn merge_variants_ignoring_boolean_conditions(
        other: &PossibleDefinitionsPerTypeCondition,
    ) -> Option<DefinitionVariant> {
        // Note: Similar to `collect_variants_for_boolean_condition`'s implementation.
        let mut iter = other.conditional_variants().iter();
        let first = iter.next()?;
        let mut result_sub = first.sub_selection_response_shape().cloned();
        for variant in iter {
            if compare_representative_field(
                variant.representative_field(),
                first.representative_field(),
            )
            .is_err()
            {
                // Unexpected: GraphQL invariant violation
                return None;
            }
            match (&mut result_sub, variant.sub_selection_response_shape()) {
                (None, None) => {}
                (Some(result_sub), Some(variant_sub)) => {
                    let result = result_sub.merge_with(variant_sub);
                    if result.is_err() {
                        return None;
                    }
                }
                _ => {
                    return None;
                }
            }
        }
        Some(first.with_updated_fields(Clause::default(), result_sub))
    }

    fn key_only_compare_definition_variant(
        this: &DefinitionVariant,
        other: &DefinitionVariant,
    ) -> bool {
        // Note: The `boolean_clause` of DefinitionVariant is ignored.
        match (
            this.sub_selection_response_shape(),
            other.sub_selection_response_shape(),
        ) {
            (None, None) => true,
            (Some(this_sub), Some(other_sub)) => {
                key_only_compare_response_shapes(this_sub, other_sub)
            }
            _ => false,
        }
    }
}

use key_only_response_shape_compare::key_only_compare_response_shapes;
