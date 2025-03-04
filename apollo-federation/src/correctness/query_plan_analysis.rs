// Analyze a QueryPlan and compute its overall response shape

use apollo_compiler::executable::Name;
use itertools::Itertools;

use super::response_shape::Clause;
use super::response_shape::Literal;
use super::response_shape::NormalizedTypeCondition;
use super::response_shape::PossibleDefinitions;
use super::response_shape::ResponseShape;
use super::response_shape::compute_response_shape_for_entity_fetch_operation;
use super::response_shape::compute_response_shape_for_operation;
use crate::FederationError;
use crate::SingleFederationError;
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
    fn add_boolean_conditions(&self, literals: &[Literal]) -> Self {
        let added_clause = Clause::from_literals(literals);
        self.concatenate_and_simplify_boolean_conditions(&Clause::default(), &added_clause)
    }
}

//==================================================================================================
// Interpretation of QueryPlan

fn format_federation_error(e: FederationError) -> String {
    match e {
        FederationError::SingleFederationError(e) => match e {
            SingleFederationError::Internal { message } => message,
            _ => e.to_string(),
        },
        _ => e.to_string(),
    }
}

pub fn interpret_query_plan(
    schema: &ValidFederationSchema,
    root_type: &Name,
    plan: &QueryPlan,
) -> Result<ResponseShape, String> {
    let state = ResponseShape::new(root_type.clone());
    let Some(plan_node) = &plan.node else {
        // empty plan
        return Ok(state);
    };
    interpret_top_level_plan_node(schema, &state, plan_node)
}

fn interpret_top_level_plan_node(
    schema: &ValidFederationSchema,
    state: &ResponseShape,
    node: &TopLevelPlanNode,
) -> Result<ResponseShape, String> {
    let conditions = vec![];
    match node {
        TopLevelPlanNode::Fetch(fetch) => interpret_fetch_node(schema, state, &conditions, fetch),
        TopLevelPlanNode::Sequence(sequence) => {
            interpret_sequence_node(schema, state, &conditions, sequence)
        }
        TopLevelPlanNode::Parallel(parallel) => {
            interpret_parallel_node(schema, state, &conditions, parallel)
        }
        TopLevelPlanNode::Flatten(flatten) => {
            interpret_flatten_node(schema, state, &conditions, flatten)
        }
        TopLevelPlanNode::Condition(condition) => {
            interpret_condition_node(schema, state, &conditions, condition)
        }
        TopLevelPlanNode::Defer(defer) => interpret_defer_node(schema, state, &conditions, defer),
        TopLevelPlanNode::Subscription(subscription) => {
            let mut result =
                interpret_fetch_node(schema, state, &conditions, &subscription.primary)?;
            if let Some(rest) = &subscription.rest {
                let rest = interpret_plan_node(schema, &result, &conditions, rest)?;
                result.merge_with(&rest).map_err(|e| {
                    format!(
                        "Failed to merge response shapes in subscription node: {}\nrest: {rest}",
                        format_federation_error(e),
                    )
                })?;
            }
            Ok(result)
        }
    }
}

/// `conditions` are accumulated conditions to be applied at each fetch node's response shape.
fn interpret_plan_node(
    schema: &ValidFederationSchema,
    state: &ResponseShape,
    conditions: &[Literal],
    node: &PlanNode,
) -> Result<ResponseShape, String> {
    match node {
        PlanNode::Fetch(fetch) => interpret_fetch_node(schema, state, conditions, fetch),
        PlanNode::Sequence(sequence) => {
            interpret_sequence_node(schema, state, conditions, sequence)
        }
        PlanNode::Parallel(parallel) => {
            interpret_parallel_node(schema, state, conditions, parallel)
        }
        PlanNode::Flatten(flatten) => interpret_flatten_node(schema, state, conditions, flatten),
        PlanNode::Condition(condition) => {
            interpret_condition_node(schema, state, conditions, condition)
        }
        PlanNode::Defer(defer) => interpret_defer_node(schema, state, conditions, defer),
    }
}

// `type_filter`: The type condition to apply to the response shape.
// - This is from the previous path elements.
// - It can be empty if there is no type conditions.
// - Also, multiple type conditions can be accumulated (meaning the conjunction of them).
fn rename_at_path(
    schema: &ValidFederationSchema,
    state: &ResponseShape,
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
            let Some(defs) = state.get(name) else {
                // If some sub-states don't have the name, skip it and return the same state.
                return Ok(state.clone());
            };
            let rename_here = rest.is_empty();
            // Compute the normalized type condition for the type filter.
            let type_filter = if let Some((first_type, rest_of_types)) = type_filter.split_first() {
                let mut type_condition =
                    NormalizedTypeCondition::from_type_name(first_type.clone(), schema)
                        .map_err(format_federation_error)?;
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
            // Interpret the node in every matching sub-state.
            let mut updated_defs = PossibleDefinitions::default(); // for the old name
            let mut target_defs = PossibleDefinitions::default(); // for the new name
            for (type_cond, defs_per_type_cond) in defs.iter() {
                if let Some(type_filter) = &type_filter {
                    if !type_filter.implies(type_cond) {
                        // Not applicable => same as before
                        updated_defs.insert(type_cond.clone(), defs_per_type_cond.clone());
                        continue;
                    }
                }
                if rename_here {
                    // move all definitions to the target_defs
                    target_defs.insert(type_cond.clone(), defs_per_type_cond.clone());
                    continue;
                }

                // otherwise, rename in the sub-states
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
            let mut result = state.clone();
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
            rename_at_path(schema, state, type_filter, rest, new_name)
        }
        FetchDataPathElement::Parent => {
            Err("rename_at_path: unexpected parent path element".to_string())
        }
    }
}

fn apply_rewrites(
    schema: &ValidFederationSchema,
    state: &ResponseShape,
    rewrite: &FetchDataRewrite,
) -> Result<ResponseShape, String> {
    match rewrite {
        FetchDataRewrite::ValueSetter(_) => {
            Err("apply_rewrites: unexpected value setter".to_string())
        }
        FetchDataRewrite::KeyRenamer(renamer) => rename_at_path(
            schema,
            state,
            Default::default(), // new type filter
            &renamer.path,
            renamer.rename_key_to.clone(),
        ),
    }
}

fn interpret_fetch_node(
    schema: &ValidFederationSchema,
    _state: &ResponseShape,
    conditions: &[Literal],
    fetch: &FetchNode,
) -> Result<ResponseShape, String> {
    let mut result = if !fetch.requires.is_empty() {
        // TODO: check requires
        compute_response_shape_for_entity_fetch_operation(
            fetch
                .operation_document
                .as_parsed()
                .map_err(|e| e.to_string()),
            schema,
        )
        .map(|rs| rs.add_boolean_conditions(conditions))
    } else {
        compute_response_shape_for_operation(
            fetch
                .operation_document
                .as_parsed()
                .map_err(|e| e.to_string()),
            schema,
        )
        .map(|rs| rs.add_boolean_conditions(conditions))
    }
    .map_err(|e| {
        format!(
            "Failed to compute the response shape from fetch node: {}\nnode: {fetch}",
            format_federation_error(e),
        )
    })?;
    for rewrite in &fetch.output_rewrites {
        result = apply_rewrites(schema, &result, rewrite)?;
    }
    Ok(result)
}

/// Add a literal to the conditions
fn append_literal(conditions: &[Literal], literal: Literal) -> Vec<Literal> {
    let mut result = conditions.to_vec();
    result.push(literal);
    result
}

fn interpret_condition_node(
    schema: &ValidFederationSchema,
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
                schema,
                state,
                &sub_conditions,
                if_clause,
            )?)
        }
        (None, Some(else_clause)) => {
            let literal = Literal::Neg(condition_variable.clone());
            let sub_conditions = append_literal(conditions, literal);
            Ok(interpret_plan_node(
                schema,
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
            let if_val = interpret_plan_node(schema, state, &sub_conditions_pos, if_clause)?;
            let else_val = interpret_plan_node(schema, state, &sub_conditions_neg, else_clause)?;
            let mut result = if_val;
            result.merge_with(&else_val).map_err(|e| {
                format!(
                    "Failed to merge response shapes from then and else clauses:\n{}",
                    format_federation_error(e),
                )
            })?;
            Ok(result)
        }
    }
}

fn interpret_plan_node_at_path(
    schema: &ValidFederationSchema,
    state: &ResponseShape,
    conditions: &[Literal],
    type_condition: Option<&Vec<Name>>,
    path: &[FetchDataPathElement],
    node: &PlanNode,
) -> Result<Option<ResponseShape>, String> {
    let Some((first, rest)) = path.split_first() else {
        return Ok(Some(interpret_plan_node(schema, state, conditions, node)?));
    };
    match first {
        FetchDataPathElement::Key(name, next_type_condition) => {
            // Note: `next_type_condition` is applied to the next key down the path.
            let filter_type_cond = type_condition
                .map(|cond| {
                    let obj_types: Result<Vec<ObjectTypeDefinitionPosition>, FederationError> =
                        cond.iter()
                            .map(|name| {
                                let ty: ObjectTypeDefinitionPosition =
                                    schema.get_type(name.clone())?.try_into()?;
                                Ok(ty)
                            })
                            .collect();
                    let obj_types = obj_types.map_err(format_federation_error)?;
                    Ok::<_, String>(NormalizedTypeCondition::from_object_types(
                        obj_types.into_iter(),
                    ))
                })
                .transpose()?;
            let Some(defs) = state.get(name) else {
                // If some sub-states don't have the name, skip it and return None.
                // However, one of the `defs` must have one sub-state that has the name (see below).
                return Ok(None);
            };
            // Interpret the node in every matching sub-state.
            let mut updated_defs = PossibleDefinitions::default();
            for (type_cond, defs_per_type_cond) in defs.iter() {
                if let Some(filter_type_cond) = &filter_type_cond {
                    if !filter_type_cond.implies(type_cond) {
                        // Not applicable => same as before
                        updated_defs.insert(type_cond.clone(), defs_per_type_cond.clone());
                        continue;
                    }
                }
                let updated_variants =
                    defs_per_type_cond
                        .conditional_variants()
                        .iter()
                        .filter_map(|variant| {
                            let Some(sub_state) = variant.sub_selection_response_shape() else {
                                return Some(Err(format!(
                                    "No sub-selection at path: {}",
                                    path.iter()
                                        .map(|p| p.to_string())
                                        .collect::<Vec<_>>()
                                        .join(".")
                                )));
                            };
                            let updated_sub_state = interpret_plan_node_at_path(
                                schema,
                                sub_state,
                                conditions,
                                next_type_condition.as_ref(),
                                rest,
                                node,
                            );
                            match updated_sub_state {
                                Err(e) => Some(Err(e)),
                                Ok(updated_sub_state) => {
                                    updated_sub_state.map(|updated_sub_state| {
                                        Ok(variant.with_updated_sub_selection_response_shape(
                                            updated_sub_state,
                                        ))
                                    })
                                }
                            }
                        });
                let updated_variants: Result<Vec<_>, _> = updated_variants.collect();
                let updated_variants = updated_variants?;
                if !updated_variants.is_empty() {
                    let updated_defs_per_type_cond =
                        defs_per_type_cond.with_updated_conditional_variants(updated_variants);
                    updated_defs.insert(type_cond.clone(), updated_defs_per_type_cond);
                }
            }
            if updated_defs.is_empty() {
                // Nothing to interpret here => return None
                return Ok(None);
            }
            let mut result = state.clone();
            result.insert(name.clone(), updated_defs);
            Ok(Some(result))
        }
        FetchDataPathElement::AnyIndex(next_type_condition) => {
            if type_condition.is_some() {
                return Err("flatten: unexpected multiple type conditions".to_string());
            }
            let type_condition = next_type_condition.as_ref();
            interpret_plan_node_at_path(schema, state, conditions, type_condition, rest, node)
        }
        FetchDataPathElement::TypenameEquals(_type_name) => {
            Err("flatten: unexpected TypenameEquals variant".to_string())
        }
        FetchDataPathElement::Parent => Err("flatten: unexpected Parent variant".to_string()),
    }
}

fn interpret_flatten_node(
    schema: &ValidFederationSchema,
    state: &ResponseShape,
    conditions: &[Literal],
    flatten: &FlattenNode,
) -> Result<ResponseShape, String> {
    let result = interpret_plan_node_at_path(
        schema,
        state,
        conditions,
        None, // no type condition at the top level
        &flatten.path,
        &flatten.node,
    )?;
    let Some(result) = result else {
        // `flatten.path` is addressing a non-existing response object.
        // Ideally, this should not happen, but QP may try to fetch infeasible selections.
        // TODO: Report this as a over-fetching later.
        return Ok(state.clone());
    };
    Ok(result.simplify_boolean_conditions())
}

fn interpret_sequence_node(
    schema: &ValidFederationSchema,
    state: &ResponseShape,
    conditions: &[Literal],
    sequence: &SequenceNode,
) -> Result<ResponseShape, String> {
    let mut response_shape = state.clone();
    for node in &sequence.nodes {
        let node_rs = interpret_plan_node(schema, &response_shape, conditions, node)?;
        response_shape.merge_with(&node_rs).map_err(|e| {
            format!(
                "Failed to merge response shapes in sequence node: {}\nnode: {node}",
                format_federation_error(e),
            )
        })?;
    }
    Ok(response_shape)
}

fn interpret_parallel_node(
    schema: &ValidFederationSchema,
    state: &ResponseShape,
    conditions: &[Literal],
    parallel: &ParallelNode,
) -> Result<ResponseShape, String> {
    let mut response_shape = state.clone();
    for node in &parallel.nodes {
        // Note: Use the same original state for each parallel node
        let node_rs = interpret_plan_node(schema, state, conditions, node)?;
        response_shape.merge_with(&node_rs).map_err(|e| {
            format!(
                "Failed to merge response shapes in parallel node: {}\nnode: {node}",
                format_federation_error(e),
            )
        })?;
    }
    Ok(response_shape)
}

fn interpret_defer_node(
    schema: &ValidFederationSchema,
    state: &ResponseShape,
    conditions: &[Literal],
    defer: &DeferNode,
) -> Result<ResponseShape, String> {
    // new `state` after the primary node
    let state = if let Some(primary_node) = &defer.primary.node {
        &interpret_plan_node(schema, state, conditions, primary_node)?
    } else {
        state
    };

    // interpret the deferred nodes and merge their response shapes.
    let mut result = state.clone();
    for defer_block in &defer.deferred {
        let Some(node) = &defer_block.node else {
            // Nothing to do => skip
            continue;
        };
        // Note: Use the same primary node state for each parallel node
        let node_rs = interpret_plan_node(schema, state, conditions, node)?;
        result.merge_with(&node_rs).map_err(|e| {
            format!(
                "Failed to merge response shapes in deferred node: {}\nnode: {node}",
                format_federation_error(e),
            )
        })?;
    }
    Ok(result)
}
