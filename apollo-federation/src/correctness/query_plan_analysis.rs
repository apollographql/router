// Analyze a QueryPlan and compute its overall response shape

use std::collections::HashSet;
use std::sync::Arc;

use apollo_compiler::collections::IndexMap;
use apollo_compiler::executable::Field;
use apollo_compiler::executable::Name;
use itertools::Itertools;

use super::query_plan_soundness::check_requires;
use super::response_shape::Clause;
use super::response_shape::DefinitionVariant;
use super::response_shape::Literal;
use super::response_shape::NormalizedTypeCondition;
use super::response_shape::PossibleDefinitions;
use super::response_shape::PossibleDefinitionsPerTypeCondition;
use super::response_shape::ResponseShape;
use super::response_shape::compute_response_shape_for_entity_fetch_operation;
use super::response_shape::compute_response_shape_for_operation;
use super::response_shape_compare::collect_definitions_for_type_condition;
use super::response_shape_compare::collect_variants_for_boolean_condition;
use crate::FederationError;
use crate::SingleFederationError;
use crate::bail;
use crate::query_plan::ConditionNode;
use crate::query_plan::DeferNode;
use crate::query_plan::FetchDataPathElement;
use crate::query_plan::FetchDataRewrite;
use crate::query_plan::FetchNode;
use crate::query_plan::FlattenNode;
use crate::query_plan::ParallelNode;
use crate::query_plan::PlanNode;
use crate::query_plan::QueryPlan;
use crate::query_plan::SequenceNode;
use crate::query_plan::TopLevelPlanNode;
use crate::schema::ValidFederationSchema;
use crate::schema::position::ObjectTypeDefinitionPosition;

//==================================================================================================
// ResponseShape extra methods to support query plan analysis

impl ResponseShape {
    /// Simplify the boolean conditions in the response shape so that there are no redundant
    /// conditions on fields by removing conditions that are also present on an ancestor field.
    fn simplify_boolean_conditions(&self) -> Self {
        self.inner_simplify_boolean_conditions(&Clause::default())
    }

    fn inner_simplify_boolean_conditions(&self, inherited_clause: &Clause) -> Self {
        let mut result = ResponseShape::new(self.default_type_condition().clone());
        for (key, defs) in self.iter() {
            let mut updated_defs = PossibleDefinitions::default();
            for (type_cond, defs_per_type_cond) in defs.iter() {
                let updated_variants =
                    defs_per_type_cond
                        .conditional_variants()
                        .iter()
                        .filter_map(|variant| {
                            let new_clause = variant.boolean_clause().clone();
                            inherited_clause.concatenate_and_simplify(&new_clause).map(
                                |(inherited_clause, field_clause)| {
                                    let sub_rs =
                                        variant.sub_selection_response_shape().as_ref().map(|rs| {
                                            rs.inner_simplify_boolean_conditions(&inherited_clause)
                                        });
                                    variant.with_updated_fields(field_clause, sub_rs)
                                },
                            )
                        });
                let updated_defs_per_type_cond = defs_per_type_cond
                    .with_updated_conditional_variants(updated_variants.collect());
                updated_defs.insert(type_cond.clone(), updated_defs_per_type_cond);
            }
            result.insert(key.clone(), updated_defs);
        }
        result
    }

    /// concatenate `added_clause` to each field's boolean condition (only at the top level)
    /// then simplify the boolean conditions below the top-level.
    fn concatenate_and_simplify_boolean_conditions(
        &self,
        inherited_clause: &Clause,
        added_clause: &Clause,
    ) -> Self {
        let mut result = ResponseShape::new(self.default_type_condition().clone());
        for (key, defs) in self.iter() {
            let mut updated_defs = PossibleDefinitions::default();
            for (type_cond, defs_per_type_cond) in defs.iter() {
                let updated_variants =
                    defs_per_type_cond
                        .conditional_variants()
                        .iter()
                        .filter_map(|variant| {
                            let new_clause = if added_clause.is_always_true() {
                                variant.boolean_clause().clone()
                            } else {
                                variant.boolean_clause().concatenate(added_clause)?
                            };
                            inherited_clause.concatenate_and_simplify(&new_clause).map(
                                |(inherited_clause, field_clause)| {
                                    let sub_rs =
                                        variant.sub_selection_response_shape().as_ref().map(|rs| {
                                            rs.inner_simplify_boolean_conditions(&inherited_clause)
                                        });
                                    variant.with_updated_fields(field_clause, sub_rs)
                                },
                            )
                        });
                let updated_defs_per_type_cond = defs_per_type_cond
                    .with_updated_conditional_variants(updated_variants.collect());
                updated_defs.insert(type_cond.clone(), updated_defs_per_type_cond);
            }
            result.insert(key.clone(), updated_defs);
        }
        result
    }

    /// Add a new condition to a ResponseShape.
    /// - This method is intended for the top-level response shape.
    pub(crate) fn add_boolean_conditions(&self, clause: &Clause) -> Self {
        self.concatenate_and_simplify_boolean_conditions(&Clause::default(), clause)
    }
}

//==================================================================================================
// Interpretation of QueryPlan
// - `interpret_*_node` functions returns the response shape that will be fetched by the node,
//   including its sub-nodes.
// - They take the `state` parameter that represents everything that has been fetched so far,
//   which is used to check the soundness property of the query plan.

/// The common data used by `interpret_query_plan` function and its subroutines.
/// - Contains the supergraph schema and all subgraph schemas.
pub struct AnalysisContext<'a> {
    supergraph_schema: ValidFederationSchema,
    subgraphs_by_name: &'a IndexMap<Arc<str>, ValidFederationSchema>,
}

impl AnalysisContext<'_> {
    pub fn new(
        supergraph_schema: ValidFederationSchema,
        subgraphs_by_name: &IndexMap<Arc<str>, ValidFederationSchema>,
    ) -> AnalysisContext<'_> {
        AnalysisContext {
            supergraph_schema,
            subgraphs_by_name,
        }
    }

    pub fn supergraph_schema(&self) -> &ValidFederationSchema {
        &self.supergraph_schema
    }

    pub fn subgraphs_by_name(&self) -> &IndexMap<Arc<str>, ValidFederationSchema> {
        self.subgraphs_by_name
    }

    fn get_subgraph_schema(&self, subgraph_name: &str) -> Option<&ValidFederationSchema> {
        self.subgraphs_by_name.get(subgraph_name)
    }
}

fn format_federation_error(e: FederationError) -> String {
    match e {
        FederationError::SingleFederationError(e) => match e {
            SingleFederationError::Internal { message } => message,
            _ => e.to_string(),
        },
        _ => e.to_string(),
    }
}

/// Computes the overall ResponseShape of the query plan while checking the soundness of each
/// entity fetch.
pub fn interpret_query_plan(
    context: &AnalysisContext,
    root_type: &Name,
    plan: &QueryPlan,
) -> Result<ResponseShape, String> {
    let state = ResponseShape::new(root_type.clone());
    let Some(plan_node) = &plan.node else {
        // empty plan
        return Ok(state);
    };
    interpret_top_level_plan_node(context, &state, plan_node)
}

fn interpret_top_level_plan_node(
    context: &AnalysisContext,
    state: &ResponseShape,
    node: &TopLevelPlanNode,
) -> Result<ResponseShape, String> {
    let conditions = vec![];
    match node {
        TopLevelPlanNode::Fetch(fetch) => interpret_fetch_node(context, state, &conditions, fetch),
        TopLevelPlanNode::Sequence(sequence) => {
            interpret_sequence_node(context, state, &conditions, sequence)
        }
        TopLevelPlanNode::Parallel(parallel) => {
            interpret_parallel_node(context, state, &conditions, parallel)
        }
        TopLevelPlanNode::Flatten(flatten) => {
            interpret_flatten_node(context, state, &conditions, flatten)
        }
        TopLevelPlanNode::Condition(condition) => {
            interpret_condition_node(context, state, &conditions, condition)
        }
        TopLevelPlanNode::Defer(defer) => interpret_defer_node(context, state, &conditions, defer),
        TopLevelPlanNode::Subscription(subscription) => {
            interpret_subscription_node(context, state, &conditions, subscription)
        }
    }
}

/// `conditions` are accumulated conditions to be applied at each fetch node's response shape.
fn interpret_plan_node(
    context: &AnalysisContext,
    state: &ResponseShape,
    conditions: &[Literal],
    node: &PlanNode,
) -> Result<ResponseShape, String> {
    match node {
        PlanNode::Fetch(fetch) => interpret_fetch_node(context, state, conditions, fetch),
        PlanNode::Sequence(sequence) => {
            interpret_sequence_node(context, state, conditions, sequence)
        }
        PlanNode::Parallel(parallel) => {
            interpret_parallel_node(context, state, conditions, parallel)
        }
        PlanNode::Flatten(flatten) => interpret_flatten_node(context, state, conditions, flatten),
        PlanNode::Condition(condition) => {
            interpret_condition_node(context, state, conditions, condition)
        }
        PlanNode::Defer(defer) => interpret_defer_node(context, state, conditions, defer),
    }
}

/// `type_filter`: The type condition to apply to the response shape.
/// - This is from the previous path elements.
/// - It can be empty if there is no type conditions.
/// - Also, multiple type conditions can be accumulated (meaning the conjunction of them).
fn rename_at_path(
    schema: &ValidFederationSchema,
    response: &ResponseShape,
    mut type_filter: Vec<Name>,
    path: &[FetchDataPathElement],
    new_name: Name,
) -> Result<ResponseShape, String> {
    let Some((first, rest)) = path.split_first() else {
        return Err("rename_at_path: unexpected empty path".to_string());
    };
    match first {
        FetchDataPathElement::Key(name, _conditions) => {
            if _conditions.is_some() {
                return Err("rename_at_path: unexpected key conditions".to_string());
            }
            let Some(defs) = response.get(name) else {
                // If the response doesn't have the named key, skip it and return the same response.
                return Ok(response.clone());
            };
            let rename_here = rest.is_empty();
            // Compute the normalized type condition for the type filter.
            let type_filter = if let Some((first_type, rest_of_types)) = type_filter.split_first() {
                let Some(mut type_condition) =
                    NormalizedTypeCondition::from_type_name(first_type.clone(), schema)
                        .map_err(format_federation_error)?
                else {
                    return Err(format!(
                        "rename_at_path: unexpected empty type condition: {first_type}"
                    ));
                };
                for type_name in rest_of_types {
                    let Some(updated) = type_condition
                        .add_type_name(type_name.clone(), schema)
                        .map_err(format_federation_error)?
                    else {
                        return Err(format!(
                            "rename_at_path: inconsistent type conditions: {type_filter:?}"
                        ));
                    };
                    type_condition = updated;
                }
                Some(type_condition)
            } else {
                None
            };
            // Apply renaming in every matching sub-response.
            let mut updated_defs = PossibleDefinitions::default(); // for the old name
            let mut target_defs = PossibleDefinitions::default(); // for the new name
            for (type_cond, defs_per_type_cond) in defs.iter() {
                if let Some(type_filter) = &type_filter
                    && !type_filter.implies(type_cond)
                {
                    // Not applicable => same as before
                    updated_defs.insert(type_cond.clone(), defs_per_type_cond.clone());
                    continue;
                }
                if rename_here {
                    // move all definitions to the target_defs
                    target_defs.insert(type_cond.clone(), defs_per_type_cond.clone());
                    continue;
                }

                // otherwise, rename in the sub-response
                let updated_variants =
                    defs_per_type_cond
                        .conditional_variants()
                        .iter()
                        .map(|variant| {
                            let Some(sub_state) = variant.sub_selection_response_shape() else {
                                return Err(format!(
                                    "No sub-selection at path: {}",
                                    path.iter().join(".")
                                ));
                            };
                            let updated_sub_state = rename_at_path(
                                schema,
                                sub_state,
                                Default::default(), // new type filter
                                rest,
                                new_name.clone(),
                            )?;
                            Ok(
                                variant
                                    .with_updated_sub_selection_response_shape(updated_sub_state),
                            )
                        });
                let updated_variants: Result<Vec<_>, _> = updated_variants.collect();
                let updated_variants = updated_variants?;
                let updated_defs_per_type_cond =
                    defs_per_type_cond.with_updated_conditional_variants(updated_variants);
                updated_defs.insert(type_cond.clone(), updated_defs_per_type_cond);
            }
            let mut result = response.clone();
            result.insert(name.clone(), updated_defs);
            if rename_here {
                // also, update the new response key
                let prev_defs = result.get(&new_name);
                match prev_defs {
                    None => {
                        result.insert(new_name, target_defs);
                    }
                    Some(prev_defs) => {
                        let mut merged_defs = prev_defs.clone();
                        for (type_cond, defs_per_type_cond) in target_defs.iter() {
                            let existed =
                                merged_defs.insert(type_cond.clone(), defs_per_type_cond.clone());
                            if existed {
                                return Err(format!(
                                    "rename_at_path: new name/type already exists: {new_name} on {type_cond}"
                                ));
                            }
                        }
                        result.insert(new_name, merged_defs);
                    }
                }
            }
            Ok(result)
        }
        FetchDataPathElement::AnyIndex(_conditions) => {
            Err("rename_at_path: unexpected AnyIndex path element".to_string())
        }
        FetchDataPathElement::TypenameEquals(type_name) => {
            type_filter.push(type_name.clone());
            rename_at_path(schema, response, type_filter, rest, new_name)
        }
        FetchDataPathElement::Parent => {
            Err("rename_at_path: unexpected parent path element".to_string())
        }
    }
}

fn apply_output_rewrite(
    schema: &ValidFederationSchema,
    response: &ResponseShape,
    rewrite: &FetchDataRewrite,
) -> Result<ResponseShape, String> {
    match rewrite {
        FetchDataRewrite::ValueSetter(_) => {
            Err("apply_output_rewrite: unexpected value setter".to_string())
        }
        FetchDataRewrite::KeyRenamer(renamer) => rename_at_path(
            schema,
            response,
            Default::default(), // new type filter
            &renamer.path,
            renamer.rename_key_to.clone(),
        ),
    }
}

fn check_input_rewrite(rewrite: &FetchDataRewrite) -> Result<(), String> {
    match rewrite {
        FetchDataRewrite::KeyRenamer(rename) => Err(format!(
            "check_input_rewrite: unexpected key renamer: {rename:?}"
        )),
        FetchDataRewrite::ValueSetter(_) => {
            // This case is only created in `compute_input_rewrites_on_key_fetch`. It overwrites
            // the existing `__typename` response value. But, it won't affect the response shape
            // anyways. So, we can ignore it here.
            Ok(())
        }
    }
}

fn interpret_fetch_node(
    context: &AnalysisContext,
    state: &ResponseShape,
    conditions: &[Literal],
    fetch: &FetchNode,
) -> Result<ResponseShape, String> {
    let schema = &context.supergraph_schema;
    let operation_doc = fetch
        .operation_document
        .as_parsed()
        .map_err(|e| e.to_string())?;
    let boolean_clause = Clause::from_literals(conditions);
    let mut result = if !fetch.requires.is_empty() {
        // Response shapes per entity selection
        let response_shapes =
            compute_response_shape_for_entity_fetch_operation(operation_doc, schema).map_err(
                |e| {
                    format!(
                        "Failed to compute the response shape from fetch node: {}\nnode: {fetch}",
                        format_federation_error(e),
                    )
                },
            )?;

        // Soundness check
        // TODO: also check `context_rewrites` requirements.
        let subgraph_name = &fetch.subgraph_name;
        let Some(subgraph_schema) = context.get_subgraph_schema(subgraph_name) else {
            return Err(format!(
                "Subgraph schema not found for {subgraph_name}:\n{fetch}"
            ));
        };
        check_requires(
            context,
            subgraph_schema,
            state,
            &boolean_clause,
            &response_shapes,
            &fetch.requires,
        )
        .map_err(|e| format!("{e}\nfetch node: {fetch}"))?;

        // Compute the merged result from the individual entity response shapes.
        merge_response_shapes(response_shapes.iter()).map_err(|e| {
            format!(
                "Failed to merge response shapes in fetch node: {}\nnode: {fetch}",
                format_federation_error(e),
            )
        })
    } else {
        compute_response_shape_for_operation(operation_doc, schema).map_err(|e| {
            format!(
                "Failed to compute the response shape from fetch node: {}\nnode: {fetch}",
                format_federation_error(e),
            )
        })
    }
    .map(|rs| rs.add_boolean_conditions(&boolean_clause))?;
    for rewrite in fetch.input_rewrites.iter() {
        check_input_rewrite(rewrite)?;
    }
    for rewrite in &fetch.output_rewrites {
        result = apply_output_rewrite(schema, &result, rewrite)?;
    }
    if !fetch.context_rewrites.is_empty() {
        result = remove_context_arguments(&fetch.context_rewrites, &result)?;
    }
    Ok(result)
}

fn merge_response_shapes<'a>(
    mut iter: impl Iterator<Item = &'a ResponseShape>,
) -> Result<ResponseShape, FederationError> {
    let Some(first) = iter.next() else {
        bail!("No response shapes to merge")
    };
    let mut result = first.clone();
    for rs in iter {
        result.merge_with(rs)?;
    }
    Ok(result)
}

/// Remove context arguments that are added to fetch operations.
/// Returns a new response shape with all field arguments referencing a context variable removed.
fn remove_context_arguments(
    context_rewrites: &[Arc<FetchDataRewrite>],
    response: &ResponseShape,
) -> Result<ResponseShape, String> {
    let context_variables: Result<HashSet<Name>, _> = context_rewrites
        .iter()
        .map(|rewrite| match rewrite.as_ref() {
            FetchDataRewrite::KeyRenamer(renamer) => Ok(renamer.rename_key_to.clone()),
            FetchDataRewrite::ValueSetter(_) => {
                Err("unexpected value setter in context rewrites".to_string())
            }
        })
        .collect();
    Ok(remove_context_arguments_in_response_shape(
        &context_variables?,
        response,
    ))
}

/// `context_variables`: the set of context variable names
fn remove_context_arguments_in_response_shape(
    context_variables: &HashSet<Name>,
    response_shape: &ResponseShape,
) -> ResponseShape {
    let mut result = ResponseShape::new(response_shape.default_type_condition().clone());
    for (key, defs) in response_shape.iter() {
        let mut updated_defs = PossibleDefinitions::default();
        for (type_cond, defs_per_type_cond) in defs.iter() {
            let updated_selection_key = remove_context_arguments_in_field(
                context_variables,
                defs_per_type_cond.field_selection_key(),
            );
            let updated_variants =
                defs_per_type_cond
                    .conditional_variants()
                    .iter()
                    .map(|variant| {
                        let updated_representative_field = remove_context_arguments_in_field(
                            context_variables,
                            variant.representative_field(),
                        );
                        let sub_rs = variant.sub_selection_response_shape().as_ref().map(|rs| {
                            remove_context_arguments_in_response_shape(context_variables, rs)
                        });
                        DefinitionVariant::new(
                            variant.boolean_clause().clone(),
                            updated_representative_field,
                            sub_rs,
                        )
                    });
            let updated_variants: Vec<_> = updated_variants.collect();
            let updated_defs_per_type_cond =
                PossibleDefinitionsPerTypeCondition::new(updated_selection_key, updated_variants);
            updated_defs.insert(type_cond.clone(), updated_defs_per_type_cond);
        }
        result.insert(key.clone(), updated_defs);
    }
    result
}

/// `context_variables`: the set of context variable names
fn remove_context_arguments_in_field(context_variables: &HashSet<Name>, field: &Field) -> Field {
    let arguments = field
        .arguments
        .iter()
        .filter_map(|arg| {
            // see if the argument value is one of the context variables
            match arg.value.as_variable() {
                Some(var) if context_variables.contains(var) => None,
                _ => Some(arg.clone()),
            }
        })
        .collect();
    Field {
        arguments,
        ..field.clone()
    }
}

/// Add a literal to the conditions
fn append_literal(conditions: &[Literal], literal: Literal) -> Vec<Literal> {
    let mut result = conditions.to_vec();
    result.push(literal);
    result
}

fn interpret_condition_node(
    context: &AnalysisContext,
    state: &ResponseShape,
    conditions: &[Literal],
    condition: &ConditionNode,
) -> Result<ResponseShape, String> {
    let condition_variable = &condition.condition_variable;
    match (&condition.if_clause, &condition.else_clause) {
        (None, None) => Err("Condition node must have either if or else clause".to_string()),
        (Some(if_clause), None) => {
            let literal = Literal::Pos(condition_variable.clone());
            let sub_conditions = append_literal(conditions, literal);
            Ok(interpret_plan_node(
                context,
                state,
                &sub_conditions,
                if_clause,
            )?)
        }
        (None, Some(else_clause)) => {
            let literal = Literal::Neg(condition_variable.clone());
            let sub_conditions = append_literal(conditions, literal);
            Ok(interpret_plan_node(
                context,
                state,
                &sub_conditions,
                else_clause,
            )?)
        }
        (Some(if_clause), Some(else_clause)) => {
            let lit_pos = Literal::Pos(condition_variable.clone());
            let lit_neg = Literal::Neg(condition_variable.clone());
            let sub_conditions_pos = append_literal(conditions, lit_pos);
            let sub_conditions_neg = append_literal(conditions, lit_neg);
            let if_val = interpret_plan_node(context, state, &sub_conditions_pos, if_clause)?;
            let else_val = interpret_plan_node(context, state, &sub_conditions_neg, else_clause)?;
            let mut result = if_val;
            result.merge_with(&else_val).map_err(|e| {
                format!("Failed to merge response shapes from then and else clauses:\n{e}",)
            })?;
            Ok(result)
        }
    }
}

/// The inner recursive part of `interpret_flatten_node`.
fn interpret_plan_node_at_path(
    context: &AnalysisContext,
    state: &ResponseShape,
    conditions: &[Literal],
    type_condition: Option<&Vec<Name>>,
    path: &[FetchDataPathElement],
    node: &PlanNode,
) -> Result<Option<ResponseShape>, String> {
    let Some((first, rest)) = path.split_first() else {
        return Ok(Some(interpret_plan_node(context, state, conditions, node)?));
    };
    match first {
        FetchDataPathElement::Key(response_key, next_type_condition) => {
            let Some(state_defs) = state.get(response_key) else {
                // If some sub-states don't have the key, skip them and return None.
                return Ok(None);
            };
            let response_defs = interpret_plan_node_for_matching_conditions(
                context,
                state_defs,
                type_condition,
                conditions,
                next_type_condition,
                rest,
                node,
            )?;
            if response_defs.is_empty() {
                // No state definitions match the given condition => return None
                return Ok(None);
            }
            let mut response_shape = ResponseShape::new(state.default_type_condition().clone());
            response_shape.insert(response_key.clone(), response_defs);
            Ok(Some(response_shape))
        }
        FetchDataPathElement::AnyIndex(next_type_condition) => {
            if type_condition.is_some() {
                return Err("flatten: unexpected multiple type conditions".to_string());
            }
            let type_condition = next_type_condition.as_ref();
            interpret_plan_node_at_path(context, state, conditions, type_condition, rest, node)
        }
        FetchDataPathElement::TypenameEquals(_type_name) => {
            Err("flatten: unexpected TypenameEquals variant".to_string())
        }
        FetchDataPathElement::Parent => Err("flatten: unexpected Parent variant".to_string()),
    }
}

/// Interpret the plan node for all matching conditions.
/// Returns a collection of definitions by the type and Boolean conditions.
fn interpret_plan_node_for_matching_conditions(
    context: &AnalysisContext,
    state_defs: &PossibleDefinitions,
    type_condition: Option<&Vec<Name>>,
    boolean_conditions: &[Literal],
    next_type_condition: &Option<Vec<Name>>,
    next_path: &[FetchDataPathElement],
    node: &PlanNode,
) -> Result<PossibleDefinitions, String> {
    // Note: `next_type_condition` is applied to the next key down the path.
    let schema = &context.supergraph_schema;
    let normalized_type_cond = normalize_type_condition(schema, &type_condition)?;

    let mut response_defs = PossibleDefinitions::default();
    if let Some(type_cond) = normalized_type_cond {
        // Type-conditioned fetching => only consider one type condition.
        if let Some(response_per_type_cond) = interpret_plan_node_under_type_condition(
            context,
            state_defs,
            &type_cond,
            boolean_conditions,
            next_type_condition,
            next_path,
            node,
        )? {
            response_defs.insert(type_cond, response_per_type_cond);
        }
    } else {
        // No type-conditioned fetching => consider each type condition separately.
        for (type_cond, _defs_per_type_cond) in state_defs.iter() {
            if let Some(response_per_type_cond) = interpret_plan_node_under_type_condition(
                context,
                state_defs,
                type_cond,
                boolean_conditions,
                next_type_condition,
                next_path,
                node,
            )? {
                response_defs.insert(type_cond.clone(), response_per_type_cond);
            }
        }
    }
    Ok(response_defs)
}

fn normalize_type_condition(
    schema: &ValidFederationSchema,
    type_condition: &Option<&Vec<Name>>,
) -> Result<Option<NormalizedTypeCondition>, String> {
    let Some(type_condition) = type_condition else {
        return Ok(None);
    };
    let obj_types: Result<Vec<_>, _> = type_condition
        .iter()
        .map(|name| {
            let ty: ObjectTypeDefinitionPosition = schema.get_type(name.clone())?.try_into()?;
            Ok(ty)
        })
        .collect();
    let obj_types = obj_types.map_err(format_federation_error)?;
    let result = NormalizedTypeCondition::from_object_types(obj_types.into_iter())
        .map_err(format_federation_error)?;
    Ok(Some(result))
}

fn interpret_plan_node_under_type_condition(
    context: &AnalysisContext,
    state_defs: &PossibleDefinitions,
    type_cond: &NormalizedTypeCondition,
    conditions: &[Literal],
    next_type_condition: &Option<Vec<Name>>,
    next_path: &[FetchDataPathElement],
    node: &PlanNode,
) -> Result<Option<PossibleDefinitionsPerTypeCondition>, String> {
    // `state_defs` may have multiple overlapping type conditions. We merge them so that the plan
    // node is interpreted in the merged state. That means the `requires` conditions can be checked
    // against the one whole state, instead of split states.
    let Some(merged_state_def) = collect_definitions_for_type_condition(state_defs, type_cond)
        .map_err(|e| e.description().to_string())?
    else {
        // Logically, if no definitions are found for the given type condition (in case of
        // type-conditioned fetching), that means this plan node is infeasible (kind of dead-code).
        // However, since that's not expected to happen in our query plans, we report an error
        // here.
        return Err(format!(
            "No matching definitions in state for type condition: {type_cond}"
        ));
    };
    // Interpret the node under every Boolean combination.
    let response_variants = merged_state_def
        .conditional_variants()
        .iter()
        .filter_map(|variant| {
            let sub_rs = interpret_plan_node_under_boolean_condition(
                context,
                &merged_state_def,
                variant.boolean_clause(),
                conditions,
                next_type_condition,
                next_path,
                node,
            );
            match sub_rs {
                Ok(None) => None,
                Ok(Some(sub_rs)) => {
                    Some(Ok(variant.with_updated_sub_selection_response_shape(sub_rs)))
                }
                Err(e) => Some(Err(e)),
            }
        });
    let response_variants: Result<Vec<_>, _> = response_variants.collect();
    let response_variants = response_variants?;
    if !response_variants.is_empty() {
        Ok(Some(
            merged_state_def.with_updated_conditional_variants(response_variants),
        ))
    } else {
        // None of the variants are applicable.
        Ok(None)
    }
}

fn interpret_plan_node_under_boolean_condition(
    context: &AnalysisContext,
    state_def: &PossibleDefinitionsPerTypeCondition,
    variant_clause: &Clause,
    conditions: &[Literal],
    next_type_condition: &Option<Vec<Name>>,
    next_path: &[FetchDataPathElement],
    node: &PlanNode,
) -> Result<Option<ResponseShape>, String> {
    // We are considering variants that satisfy both the `variant_clause` and the fetch
    // `conditions`. We concatenate them into a single full condition.
    let Some(full_clause) = variant_clause.concatenate(&Clause::from_literals(conditions)) else {
        // This variant's clause is false under the current conditions => skip infeasible variant
        return Ok(None);
    };
    // Collect all applicable variants into a single merged one for the same reason as explained
    // in the `interpret_plan_node_under_type_condition` function.
    let Some(merged_variant) = collect_variants_for_boolean_condition(state_def, &full_clause)
        .map_err(|e| e.description().to_string())?
    else {
        // We must have at least one variant for the given clause.
        return Err(format!(
            "Internal error: failed to collect applicable variants for full clause `{full_clause}`"
        ));
    };
    let Some(sub_state) = merged_variant.sub_selection_response_shape() else {
        // A sub-selection is expected at the FlattenNode path.
        return Err(format!("No sub-selection for variant: {merged_variant}"));
    };
    interpret_plan_node_at_path(
        context,
        sub_state,
        conditions,
        next_type_condition.as_ref(),
        next_path,
        node,
    )
}

fn interpret_flatten_node(
    context: &AnalysisContext,
    state: &ResponseShape,
    conditions: &[Literal],
    flatten: &FlattenNode,
) -> Result<ResponseShape, String> {
    let response_shape = interpret_plan_node_at_path(
        context,
        state,
        conditions,
        None, // no type condition at the top level
        &flatten.path,
        &flatten.node,
    )?;
    let Some(response_shape) = response_shape else {
        // `flatten.path` is addressing a non-existing response object.
        // Ideally, this should not happen, but QP may try to fetch infeasible selections.
        // TODO: Report this as a over-fetching later.
        return Ok(ResponseShape::new(state.default_type_condition().clone()));
    };
    Ok(response_shape.simplify_boolean_conditions())
}

fn interpret_sequence_node(
    context: &AnalysisContext,
    state: &ResponseShape,
    conditions: &[Literal],
    sequence: &SequenceNode,
) -> Result<ResponseShape, String> {
    let mut state = state.clone();
    let mut response_shape = ResponseShape::new(state.default_type_condition().clone());
    for node in &sequence.nodes {
        let node_rs = interpret_plan_node(context, &state, conditions, node)?;

        // Update both state and response_shape
        state.merge_with(&node_rs).map_err(|e| {
            format!(
                "Failed to merge state in sequence node: {e}\
                 node: {node}",
            )
        })?;
        response_shape.merge_with(&node_rs).map_err(|e| {
            format!(
                "Failed to merge response shapes in sequence node: {e}\
                 node: {node}"
            )
        })?;
    }
    Ok(response_shape)
}

fn interpret_parallel_node(
    context: &AnalysisContext,
    state: &ResponseShape,
    conditions: &[Literal],
    parallel: &ParallelNode,
) -> Result<ResponseShape, String> {
    let mut response_shape = ResponseShape::new(state.default_type_condition().clone());
    for node in &parallel.nodes {
        // Note: Use the same original state for each parallel node
        let node_rs = interpret_plan_node(context, state, conditions, node)?;
        response_shape.merge_with(&node_rs).map_err(|e| {
            format!(
                "Failed to merge response shapes in parallel node: {e}\
                 node: {node}"
            )
        })?;
    }
    Ok(response_shape)
}

fn interpret_defer_node(
    context: &AnalysisContext,
    state: &ResponseShape,
    conditions: &[Literal],
    defer: &DeferNode,
) -> Result<ResponseShape, String> {
    let mut response_shape;
    let mut state = state.clone();
    if let Some(primary_node) = &defer.primary.node {
        response_shape = interpret_plan_node(context, &state, conditions, primary_node)?;
        // Update the `state` after the primary node
        state.merge_with(&response_shape).map_err(|e| {
            format!(
                "Failed to merge state in defer node: {e}\
                 state: {state}\
                 primary node response shape: {response_shape}",
            )
        })?;
    } else {
        response_shape = ResponseShape::new(state.default_type_condition().clone());
    }

    // interpret the deferred nodes and merge their response shapes.
    for defer_block in &defer.deferred {
        let Some(node) = &defer_block.node else {
            // Nothing to do => skip
            continue;
        };
        // Note: Use the same state (after the primary) for each deferred node
        let defer_rs = interpret_plan_node(context, &state, conditions, node)?;
        response_shape.merge_with(&defer_rs).map_err(|e| {
            format!(
                "Failed to merge response shapes in deferred node: {e}\
                 previous response_shape: {response_shape}\
                 deferred block response shape: {defer_rs}",
            )
        })?;
    }
    Ok(response_shape)
}

fn interpret_subscription_node(
    context: &AnalysisContext,
    state: &ResponseShape,
    conditions: &[Literal],
    subscription: &crate::query_plan::SubscriptionNode,
) -> Result<ResponseShape, String> {
    let mut response_shape =
        interpret_fetch_node(context, state, conditions, &subscription.primary)?;
    if let Some(rest) = &subscription.rest {
        let mut state = state.clone();
        state.merge_with(&response_shape).map_err(|e| {
            format!(
                "Failed to merge state in subscription node: {e}\
                 state: {state}\
                 primary response shape: {response_shape}",
            )
        })?;
        let rest_rs = interpret_plan_node(context, &state, conditions, rest)?;
        response_shape.merge_with(&rest_rs).map_err(|e| {
            format!(
                "Failed to merge response shapes in subscription node: {e}\
                 previous response shape: {response_shape}\
                 rest response shape: {rest_rs}",
            )
        })?;
    }
    Ok(response_shape)
}
