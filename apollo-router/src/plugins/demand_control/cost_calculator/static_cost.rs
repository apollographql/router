use std::sync::Arc;

use ahash::HashMap;
use ahash::HashSet;
use apollo_compiler::ast;
use apollo_compiler::ast::NamedType;
use apollo_compiler::executable::ExecutableDocument;
use apollo_compiler::executable::Field;
use apollo_compiler::executable::FragmentMap;
use apollo_compiler::executable::FragmentSpread;
use apollo_compiler::executable::InlineFragment;
use apollo_compiler::executable::Operation;
use apollo_compiler::executable::Selection;
use apollo_compiler::executable::SelectionSet;
use apollo_compiler::schema::ExtendedType;
use apollo_federation::query_plan::serializable_document::SerializableDocument;
use apollo_federation::subgraph::spec::ENTITIES_QUERY;
use serde_json_bytes::Value;

use super::CostBySubgraph;
use super::DemandControlError;
use super::directives::IncludeDirective;
use super::directives::SkipDirective;
use super::schema::DemandControlledSchema;
use super::schema::FieldDefinition;
use super::schema::InputDefinition;
use crate::configuration::subgraph::SubgraphConfiguration;
use crate::graphql::Response;
use crate::graphql::ResponseVisitor;
use crate::json_ext::Object;
use crate::plugins::demand_control::cost_calculator::directives::ListSizeDirective;
use crate::query_planner::DeferredNode;
use crate::query_planner::PlanNode;
use crate::query_planner::Primary;
use crate::query_planner::QueryPlan;
use crate::spec::TYPENAME;

pub(crate) struct StaticCostCalculator {
    list_size: u32,
    subgraph_list_sizes: Arc<SubgraphConfiguration<Option<u32>>>,
    supergraph_schema: Arc<DemandControlledSchema>,
    subgraph_schemas: Arc<HashMap<String, DemandControlledSchema>>,
}

struct ScoringContext<'a> {
    schema: &'a DemandControlledSchema,
    query: &'a ExecutableDocument,
    variables: &'a Object,
    should_estimate_requires: bool,
    /// When scoring a subgraph operation triggered by an entity fetch (i.e. FetchNode.requires
    /// is non-empty), this holds the estimated number of representations that will be sent.
    /// Used as the `instance_count` for the `_entities` root field instead of the configured
    /// default `list_size`.
    entity_count_hint: Option<i32>,
}

/// Context for scoring a query plan (`score_plan_node` and its helpers).
struct PlanScoringContext<'a> {
    variables: &'a Object,
    /// Estimated entity count at each entity-fetch flatten path, keyed by normalized response
    /// path. Precomputed by `estimate_entity_counts`.
    entity_counts: &'a HashMap<NormalizedPath, i32>,
}

/// A static response path reduced to what identifies a position in the response shape: field
/// response keys and list (`@`) markers. Simplified from Flatten node's `json_ext::Path`.
type NormalizedPath = Vec<NormalizedPathElement>;

#[derive(Clone, PartialEq, Eq, Hash)]
enum NormalizedPathElement {
    Key(String),
    List,
}

// The path is from Flatten node, so it won't have Index/Fragment variants.
fn normalized_path(path: &crate::json_ext::Path) -> NormalizedPath {
    use crate::json_ext::PathElement;
    path.0
        .iter()
        .filter_map(|element| match element {
            PathElement::Key(key, _) => Some(NormalizedPathElement::Key(key.clone())),
            PathElement::Flatten(_) => Some(NormalizedPathElement::List),
            PathElement::Index(_) | PathElement::Fragment(_) => None,
        })
        .collect()
}

/// Sizing information on a field's output
/// - Includes estimated instance count and the `@listSize` directive context its children inherit.
struct OutputSizing {
    /// The list multiplier for this field. It should be 1 for non-list fields.
    instance_count: i32,
    /// Size assigned to this field by an ancestor's `@listSize(sizedFields:)`, if any.
    list_size_from_upstream: Option<i32>,
    /// Ancestor sized-field directives descended into this field — the child's inherited directives.
    descended_list_sizes: Vec<ListSizeDirective>,
    /// `@listSize` directives declared on this field — the child selection set's parent directives.
    own_list_size_directives: Vec<ListSizeDirective>,
}

fn score_argument(
    argument: &apollo_compiler::ast::Value,
    argument_definition: &InputDefinition,
    schema: &DemandControlledSchema,
    variables: &Object,
) -> Result<f64, DemandControlError> {
    match (argument, argument_definition.ty()) {
        (_, ExtendedType::Interface(_))
        | (_, ExtendedType::Object(_))
        | (_, ExtendedType::Union(_)) => Err(DemandControlError::QueryParseFailure(format!(
            "Argument {} has type {}, but objects, interfaces, and unions are disallowed in this position",
            argument_definition.name(),
            argument_definition.ty().name()
        ))),

        (ast::Value::Object(inner_args), ExtendedType::InputObject(_)) => {
            let mut cost = argument_definition
                .cost_directive()
                .map_or(1.0, |cost| cost.weight());
            for (arg_name, arg_val) in inner_args {
                let arg_def = schema.input_field_definition(argument_definition.ty().name(), arg_name).ok_or_else(|| {
                    DemandControlError::QueryParseFailure(format!(
                        "Argument {} was found in query, but its type ({}) was not found in the schema",
                        arg_name,
                        argument_definition.ty().name()
                    ))
                })?;
                cost += score_argument(arg_val, arg_def, schema, variables)?;
            }
            Ok(cost)
        }
        (ast::Value::List(inner_args), _) => {
            let mut cost = argument_definition
                .cost_directive()
                .map_or(0.0, |cost| cost.weight());
            for arg_val in inner_args {
                cost += score_argument(arg_val, argument_definition, schema, variables)?;
            }
            Ok(cost)
        }
        (ast::Value::Variable(name), _) => {
            // We make a best effort attempt to score the variable, but some of these may not exist in the variables
            // sent on the supergraph request, such as `$representations`.
            if let Some(variable) = variables.get(name.as_str()) {
                score_variable(variable, argument_definition, schema)
            } else {
                Ok(0.0)
            }
        }
        (ast::Value::Null, _) => Ok(0.0),
        _ => Ok(argument_definition
            .cost_directive()
            .map_or(0.0, |cost| cost.weight())),
    }
}

fn score_variable(
    variable: &Value,
    argument_definition: &InputDefinition,
    schema: &DemandControlledSchema,
) -> Result<f64, DemandControlError> {
    match (variable, argument_definition.ty()) {
        (_, ExtendedType::Interface(_))
        | (_, ExtendedType::Object(_))
        | (_, ExtendedType::Union(_)) => Err(DemandControlError::QueryParseFailure(format!(
            "Argument {} has type {}, but objects, interfaces, and unions are disallowed in this position",
            argument_definition.name(),
            argument_definition.ty().name()
        ))),

        (Value::Object(inner_args), ExtendedType::InputObject(_)) => {
            let mut cost = argument_definition
                .cost_directive()
                .map_or(1.0, |cost| cost.weight());
            for (arg_name, arg_val) in inner_args {
                let arg_def = schema.input_field_definition(argument_definition.ty().name(), arg_name.as_str()).ok_or_else(|| {
                    DemandControlError::QueryParseFailure(format!(
                        "Argument {} was found in query, but its type ({}) was not found in the schema",
                        argument_definition.name(),
                        argument_definition.ty().name()
                    ))
                })?;
                cost += score_variable(arg_val, arg_def, schema)?;
            }
            Ok(cost)
        }
        (Value::Array(inner_args), _) => {
            let mut cost = argument_definition
                .cost_directive()
                .map_or(0.0, |cost| cost.weight());
            for arg_val in inner_args {
                cost += score_variable(arg_val, argument_definition, schema)?;
            }
            Ok(cost)
        }
        (Value::Null, _) => Ok(0.0),
        _ => Ok(argument_definition
            .cost_directive()
            .map_or(0.0, |cost| cost.weight())),
    }
}

impl StaticCostCalculator {
    pub(crate) fn new(
        supergraph_schema: Arc<DemandControlledSchema>,
        subgraph_schemas: Arc<HashMap<String, DemandControlledSchema>>,
        subgraph_list_sizes: Arc<SubgraphConfiguration<Option<u32>>>,
        list_size: u32,
    ) -> Self {
        Self {
            list_size,
            subgraph_list_sizes,
            supergraph_schema,
            subgraph_schemas,
        }
    }

    fn subgraph_list_size(&self, subgraph_name: &str) -> Option<u32> {
        *self.subgraph_list_sizes.get(subgraph_name)
    }

    /// Builds the `ListSizeDirective` vector for a field from its definition.
    fn own_list_size_directives(
        definition: &FieldDefinition,
        field: &Field,
        variables: &Object,
    ) -> Result<Vec<ListSizeDirective>, DemandControlError> {
        definition
            .list_size_directive_entries()
            .iter()
            .map(|entry| {
                ListSizeDirective::new(
                    &entry.directive,
                    field,
                    variables,
                    entry.parsed_sized_fields.clone(),
                )
            })
            .collect()
    }

    /// Computes the `field`'s output size from the `@listSize` context flowing into it, plus the
    /// directive context its children inherit. This is the sizing logic shared by `score_field`
    /// and `record_counts_in_selection_set`; both feed it the same `(list_size_directives,
    /// inherited_list_sizes)` pair and read the same output information back.
    ///
    /// `definition` is the field's schema definition. If it's None, treated as "no `@listSize`".
    fn output_sizing(
        &self,
        definition: Option<&FieldDefinition>,
        field: &Field,
        list_size_directives: &[ListSizeDirective],
        inherited_list_sizes: &[ListSizeDirective],
        subgraph: &str,
        variables: &Object,
    ) -> Result<OutputSizing, DemandControlError> {
        // A size assigned to this field by a parent/ancestor `@listSize(sizedFields:)`. With
        // repeatable @listSize, multiple directives can apply, so take the largest.
        let list_size_from_upstream = list_size_directives
            .iter()
            .filter_map(|dir| dir.size_of(field))
            .max()
            .or_else(|| {
                inherited_list_sizes
                    .iter()
                    .filter_map(|dir| dir.size_of(field))
                    .max()
            });

        // The directives that descend into this field, sizing its nested lists (e.g. "page" under
        // "results"). Collected from all directives that can descend to the same field.
        let descended_list_sizes: Vec<ListSizeDirective> = list_size_directives
            .iter()
            .chain(inherited_list_sizes.iter())
            .filter_map(|dir| dir.descend(field.name.as_str()))
            .collect();

        let own_list_size_directives = match definition {
            Some(definition) => Self::own_list_size_directives(definition, field, variables)?,
            None => Vec::new(),
        };

        // This field's own `@listSize`, or an ancestor sizedFields size that descended onto it.
        let effective_expected_size = own_list_size_directives
            .iter()
            .chain(descended_list_sizes.iter())
            .filter_map(|dir| dir.expected_size)
            .max();

        // The list multiplier, in priority order: a size from a parent's `sizedFields`
        // (`list_size_from_upstream`), this field's own `@listSize` (`effective_expected_size`),
        // the per-subgraph configured default, then the global `list_size`.
        let instance_count = if field.ty().is_list() {
            list_size_from_upstream
                .or(effective_expected_size)
                .or_else(|| self.subgraph_list_size(subgraph).map(|s| s as i32))
                .unwrap_or(self.list_size as i32)
        } else {
            1
        };

        Ok(OutputSizing {
            instance_count,
            list_size_from_upstream,
            descended_list_sizes,
            own_list_size_directives,
        })
    }

    /// Scores a field within a GraphQL operation, handling some expected cases where
    /// directives change how the query is fetched. In the case of the federation
    /// directive `@requires`, the cost of the required selection is added to the
    /// cost of the current field. There's a chance this double-counts the cost of
    /// a selection if two fields require the same thing, or if a field is selected
    /// along with a field that it requires.
    ///
    /// ```graphql
    /// type Query {
    ///     foo: Foo @external
    ///     bar: Bar @requires(fields: "foo")
    ///     baz: Baz @requires(fields: "foo")
    /// }
    /// ```
    ///
    /// This should be okay, as we don't want this implementation to have to know about
    /// any deduplication happening in the query planner, and we're estimating an upper
    /// bound for cost anyway.
    fn score_field(
        &self,
        ctx: &ScoringContext,
        field: &Field,
        parent_type: &NamedType,
        list_size_directives: &[ListSizeDirective],
        inherited_list_sizes: &[ListSizeDirective],
        subgraph: &str,
    ) -> Result<f64, DemandControlError> {
        // When we pre-process the schema, __typename isn't included. So, we short-circuit here to avoid failed lookups.
        if field.name == TYPENAME {
            return Ok(0.0);
        }
        if StaticCostCalculator::skipped_by_directives(field) {
            return Ok(0.0);
        }

        let definition = ctx
            .schema
            .output_field_definition(parent_type, &field.name)
            .ok_or_else(|| {
                DemandControlError::QueryParseFailure(format!(
                    "Field {} was found in query, but its type is missing from the schema.",
                    field.name
                ))
            })?;

        let sizing = self.output_sizing(
            Some(definition),
            field,
            list_size_directives,
            inherited_list_sizes,
            subgraph,
            ctx.variables,
        )?;

        let instance_count = if field.ty().is_list()
            && field.name == ENTITIES_QUERY
            && sizing.list_size_from_upstream.is_none()
            && let Some(hint) = ctx.entity_count_hint
        {
            // Entity fetch root field with the entity count derived from the FlattenNode path
            // in the query plan; it is more accurate than the static `list_size` default.
            hint
        } else {
            sizing.instance_count
        };

        // Determine the cost for this particular field. Scalars are free, non-scalars are not.
        // For fields with selections, add in the cost of the selections as well.
        let mut type_cost = if let Some(cost_directive) = definition.cost_directive() {
            cost_directive.weight()
        } else if definition.ty().is_interface()
            || definition.ty().is_object()
            || definition.ty().is_union()
        {
            1.0
        } else {
            0.0
        };
        type_cost += self.score_selection_set(
            ctx,
            &field.selection_set,
            field.ty().inner_named_type(),
            &sizing.own_list_size_directives,
            &sizing.descended_list_sizes,
            subgraph,
        )?;

        let mut arguments_cost = 0.0;
        for argument in &field.arguments {
            let argument_definition =
                definition.argument_by_name(&argument.name).ok_or_else(|| {
                    DemandControlError::QueryParseFailure(format!(
                        "Argument {} of field {} is missing a definition in the schema",
                        argument.name, field.name
                    ))
                })?;
            arguments_cost += score_argument(
                &argument.value,
                argument_definition,
                ctx.schema,
                ctx.variables,
            )?;
        }

        let mut requirements_cost = 0.0;
        if ctx.should_estimate_requires {
            // If the field is marked with `@requires`, the required selection may not be included
            // in the query's selection. Adding that requirement's cost to the field ensures it's
            // accounted for.
            let requirements = definition.requires_directive().map(|d| &d.fields);
            if let Some(selection_set) = requirements {
                requirements_cost = self.score_selection_set(
                    ctx,
                    selection_set,
                    parent_type,
                    &sizing.own_list_size_directives,
                    &[],
                    subgraph,
                )?;
            }
        }

        let cost = (instance_count as f64) * type_cost + arguments_cost + requirements_cost;
        tracing::debug!(
            "Field {} cost breakdown: (count) {} * (type cost) {} + (arguments) {} + (requirements) {} = {}",
            field.name,
            instance_count,
            type_cost,
            arguments_cost,
            requirements_cost,
            cost
        );

        Ok(cost)
    }

    fn score_fragment_spread(
        &self,
        ctx: &ScoringContext,
        fragment_spread: &FragmentSpread,
        list_size_directives: &[ListSizeDirective],
        inherited_list_sizes: &[ListSizeDirective],
        subgraph: &str,
    ) -> Result<f64, DemandControlError> {
        let fragment = fragment_spread.fragment_def(ctx.query).ok_or_else(|| {
            DemandControlError::QueryParseFailure(format!(
                "Parsed operation did not have a definition for fragment {}",
                fragment_spread.fragment_name
            ))
        })?;
        self.score_selection_set(
            ctx,
            &fragment.selection_set,
            fragment.type_condition(),
            list_size_directives,
            inherited_list_sizes,
            subgraph,
        )
    }

    fn score_inline_fragment(
        &self,
        ctx: &ScoringContext,
        inline_fragment: &InlineFragment,
        parent_type: &NamedType,
        list_size_directives: &[ListSizeDirective],
        inherited_list_sizes: &[ListSizeDirective],
        subgraph: &str,
    ) -> Result<f64, DemandControlError> {
        self.score_selection_set(
            ctx,
            &inline_fragment.selection_set,
            inline_fragment
                .type_condition
                .as_ref()
                .unwrap_or(parent_type),
            list_size_directives,
            inherited_list_sizes,
            subgraph,
        )
    }

    fn score_operation(
        &self,
        operation: &Operation,
        ctx: &ScoringContext,
        subgraph: &str,
    ) -> Result<f64, DemandControlError> {
        let mut cost = if operation.is_mutation() { 10.0 } else { 0.0 };

        let Some(root_type_name) = ctx.schema.root_operation(operation.operation_type) else {
            return Err(DemandControlError::QueryParseFailure(format!(
                "Cannot cost {} operation because the schema does not support this root type",
                operation.operation_type
            )));
        };

        cost += self.score_selection_set(
            ctx,
            &operation.selection_set,
            root_type_name,
            &[],
            &[],
            subgraph,
        )?;

        Ok(cost)
    }

    fn score_selection(
        &self,
        ctx: &ScoringContext,
        selection: &Selection,
        parent_type: &NamedType,
        list_size_directives: &[ListSizeDirective],
        inherited_list_sizes: &[ListSizeDirective],
        subgraph: &str,
    ) -> Result<f64, DemandControlError> {
        match selection {
            Selection::Field(f) => self.score_field(
                ctx,
                f,
                parent_type,
                list_size_directives,
                inherited_list_sizes,
                subgraph,
            ),
            Selection::FragmentSpread(s) => self.score_fragment_spread(
                ctx,
                s,
                list_size_directives,
                inherited_list_sizes,
                subgraph,
            ),
            Selection::InlineFragment(i) => self.score_inline_fragment(
                ctx,
                i,
                parent_type,
                list_size_directives,
                inherited_list_sizes,
                subgraph,
            ),
        }
    }

    fn score_selection_set(
        &self,
        ctx: &ScoringContext,
        selection_set: &SelectionSet,
        parent_type_name: &NamedType,
        list_size_directives: &[ListSizeDirective],
        inherited_list_sizes: &[ListSizeDirective],
        subgraph: &str,
    ) -> Result<f64, DemandControlError> {
        let mut cost = 0.0;
        for selection in selection_set.selections.iter() {
            cost += self.score_selection(
                ctx,
                selection,
                parent_type_name,
                list_size_directives,
                inherited_list_sizes,
                subgraph,
            )?;
        }
        Ok(cost)
    }

    // TODO (FED-1000): Evaluate the directives using the ctx.variables.
    fn skipped_by_directives(field: &Field) -> bool {
        let include_directive = IncludeDirective::from_field(field);
        if let Ok(Some(IncludeDirective { is_included: false })) = include_directive {
            return true;
        }

        let skip_directive = SkipDirective::from_field(field);
        if let Ok(Some(SkipDirective { is_skipped: true })) = skip_directive {
            return true;
        }

        false
    }

    fn score_plan_node(
        &self,
        plan_node: &PlanNode,
        ctx: &PlanScoringContext,
    ) -> Result<CostBySubgraph, DemandControlError> {
        match plan_node {
            PlanNode::Sequence { nodes } => self.summed_score_of_nodes(nodes, ctx),
            PlanNode::Parallel { nodes } => self.summed_score_of_nodes(nodes, ctx),
            PlanNode::Flatten(flatten_node) => {
                // Check if the inner node is an entity fetch (non-empty requires).
                // If so, supply the entity count precomputed for this flatten path so that
                // _entities is scored with the right number of representations.
                if let PlanNode::Fetch(fetch_node) = flatten_node.node.as_ref()
                    && !fetch_node.requires.is_empty()
                {
                    let entity_count = ctx
                        .entity_counts
                        .get(&normalized_path(&flatten_node.path))
                        .copied();
                    return self.estimated_cost_of_operation(
                        &fetch_node.service_name,
                        &fetch_node.operation,
                        ctx,
                        entity_count,
                    );
                }
                // Non-entity flatten or nested structure: recurse normally.
                self.score_plan_node(&flatten_node.node, ctx)
            }
            // TODO (FED-1000): Evaluate the condition using the ctx.variables.
            PlanNode::Condition {
                condition: _,
                if_clause,
                else_clause,
            } => self.max_score_of_nodes(if_clause, else_clause, ctx),
            PlanNode::Defer { primary, deferred } => {
                self.summed_score_of_deferred_nodes(primary, deferred, ctx)
            }
            PlanNode::Fetch(fetch_node) => self.estimated_cost_of_operation(
                &fetch_node.service_name,
                &fetch_node.operation,
                ctx,
                None,
            ),
            PlanNode::Subscription { primary, rest: _ } => self.estimated_cost_of_operation(
                &primary.service_name,
                &primary.operation,
                ctx,
                None,
            ),
        }
    }

    fn estimated_cost_of_operation(
        &self,
        subgraph: &str,
        operation: &SerializableDocument,
        ctx: &PlanScoringContext,
        entity_count_hint: Option<i32>,
    ) -> Result<CostBySubgraph, DemandControlError> {
        tracing::debug!("On subgraph {}, scoring operation: {}", subgraph, operation);

        let schema = self.subgraph_schemas.get(subgraph).ok_or_else(|| {
            DemandControlError::QueryParseFailure(format!(
                "Query planner did not provide a schema for service {subgraph}"
            ))
        })?;

        let operation = operation
            .as_parsed()
            .map_err(DemandControlError::SubgraphOperationNotInitialized)?;
        let cost = self.estimated(
            operation,
            schema,
            ctx.variables,
            false,
            subgraph,
            entity_count_hint,
        )?;
        Ok(CostBySubgraph::new(subgraph, cost))
    }

    fn max_score_of_nodes(
        &self,
        left: &Option<Box<PlanNode>>,
        right: &Option<Box<PlanNode>>,
        ctx: &PlanScoringContext,
    ) -> Result<CostBySubgraph, DemandControlError> {
        match (left, right) {
            (None, None) => Ok(CostBySubgraph::default()),
            (None, Some(right)) => self.score_plan_node(right, ctx),
            (Some(left), None) => self.score_plan_node(left, ctx),
            (Some(left), Some(right)) => {
                let left_score = self.score_plan_node(left, ctx)?;
                let right_score = self.score_plan_node(right, ctx)?;
                Ok(CostBySubgraph::maximum(left_score, right_score))
            }
        }
    }

    fn summed_score_of_deferred_nodes(
        &self,
        primary: &Primary,
        deferred: &Vec<DeferredNode>,
        ctx: &PlanScoringContext,
    ) -> Result<CostBySubgraph, DemandControlError> {
        let mut score = CostBySubgraph::default();
        if let Some(node) = &primary.node {
            score += self.score_plan_node(node, ctx)?;
        }
        for d in deferred {
            if let Some(node) = &d.node {
                score += self.score_plan_node(node, ctx)?;
            }
        }
        Ok(score)
    }

    fn summed_score_of_nodes(
        &self,
        nodes: &Vec<PlanNode>,
        ctx: &PlanScoringContext,
    ) -> Result<CostBySubgraph, DemandControlError> {
        let mut sum = CostBySubgraph::default();
        for node in nodes {
            sum += self.score_plan_node(node, ctx)?;
        }
        Ok(sum)
    }

    /// Determine cost for a single-subgraph operation.
    pub(crate) fn estimated(
        &self,
        query: &ExecutableDocument,
        schema: &DemandControlledSchema,
        variables: &Object,
        should_estimate_requires: bool,
        subgraph: &str,
        entity_count_hint: Option<i32>,
    ) -> Result<f64, DemandControlError> {
        let mut cost = 0.0;
        let ctx = ScoringContext {
            schema,
            query,
            variables,
            should_estimate_requires,
            entity_count_hint,
        };
        if let Some(op) = &query.operations.anonymous {
            cost += self.score_operation(op, &ctx, subgraph)?;
        }
        for (_name, op) in query.operations.named.iter() {
            cost += self.score_operation(op, &ctx, subgraph)?;
        }
        Ok(cost)
    }

    /// Determine cost for an operation which may span multiple subgraphs.
    pub(crate) fn planned(
        &self,
        query_plan: &QueryPlan,
        variables: &Object,
    ) -> Result<CostBySubgraph, DemandControlError> {
        let entity_counts = self.estimate_entity_counts(&query_plan.root, variables)?;
        let ctx = PlanScoringContext {
            variables,
            entity_counts: &entity_counts,
        };
        self.score_plan_node(&query_plan.root, &ctx)
    }

    /// Estimate the number of entities at each entity-fetch flatten path.
    ///
    /// An entity at a flatten path was produced by an earlier fetch, whose cost we already
    /// estimate by sizing its lists. We walk the plan's fetches and record, at each flatten path,
    /// the number of objects the producing fetch places there — reusing the same `@listSize`
    /// sizing as cost scoring. A later entity fetch at that path then just looks the count up; it
    /// extends those objects with more fields without changing how many there are.
    fn estimate_entity_counts(
        &self,
        root: &PlanNode,
        variables: &Object,
    ) -> Result<HashMap<NormalizedPath, i32>, DemandControlError> {
        let mut surveyed: HashSet<NormalizedPath> = HashSet::default();
        collect_flatten_paths(root, &mut surveyed);

        let mut counts: HashMap<NormalizedPath, i32> = HashMap::default();
        self.accumulate_entity_counts(root, variables, &surveyed, &mut counts)?;
        Ok(counts)
    }

    /// Walk the plan in execution order, recording entity counts at surveyed flatten paths. Fetches
    /// are processed before the flattens that depend on them (`Sequence` order), so an entity
    /// fetch's base count is already in `counts` by the time we reach it.
    fn accumulate_entity_counts(
        &self,
        node: &PlanNode,
        variables: &Object,
        surveyed: &HashSet<NormalizedPath>,
        counts: &mut HashMap<NormalizedPath, i32>,
    ) -> Result<(), DemandControlError> {
        match node {
            PlanNode::Sequence { nodes } | PlanNode::Parallel { nodes } => {
                for node in nodes {
                    self.accumulate_entity_counts(node, variables, surveyed, counts)?;
                }
            }
            PlanNode::Fetch(fetch_node) => {
                // Root fetch: its selections land at the response root, one object each.
                self.record_counts_in_fetch(
                    &fetch_node.service_name,
                    &fetch_node.operation,
                    &[],
                    1,
                    false,
                    variables,
                    surveyed,
                    counts,
                )?;
            }
            PlanNode::Flatten(flatten_node) => {
                if let PlanNode::Fetch(fetch_node) = flatten_node.node.as_ref() {
                    let base_path = normalized_path(&flatten_node.path);
                    // The objects being extended already exist at the flatten path; their count was
                    // recorded by an earlier fetch. Fall back to `list_size` if we somehow haven't
                    // seen the producer.
                    let base_count = counts
                        .get(&base_path)
                        .copied()
                        .unwrap_or(self.list_size as i32);
                    self.record_counts_in_fetch(
                        &fetch_node.service_name,
                        &fetch_node.operation,
                        &base_path,
                        base_count,
                        !fetch_node.requires.is_empty(),
                        variables,
                        surveyed,
                        counts,
                    )?;
                } else {
                    self.accumulate_entity_counts(&flatten_node.node, variables, surveyed, counts)?;
                }
            }
            // TODO (FED-1000): Evaluate the condition using the ctx.variables.
            PlanNode::Condition {
                condition: _,
                if_clause,
                else_clause,
            } => {
                if let Some(node) = if_clause {
                    self.accumulate_entity_counts(node, variables, surveyed, counts)?;
                }
                if let Some(node) = else_clause {
                    self.accumulate_entity_counts(node, variables, surveyed, counts)?;
                }
            }
            PlanNode::Defer { primary, deferred } => {
                if let Some(node) = &primary.node {
                    self.accumulate_entity_counts(node, variables, surveyed, counts)?;
                }
                for deferred_node in deferred {
                    if let Some(node) = &deferred_node.node {
                        self.accumulate_entity_counts(node, variables, surveyed, counts)?;
                    }
                }
            }
            PlanNode::Subscription { primary, rest: _ } => {
                self.record_counts_in_fetch(
                    &primary.service_name,
                    &primary.operation,
                    &[],
                    1,
                    false,
                    variables,
                    surveyed,
                    counts,
                )?;
            }
        }
        Ok(())
    }

    /// Walk one fetch's subgraph operation from `(base_path, base_count)`, recording object counts
    /// at surveyed paths. For an entity fetch the `_entities` field and its `... on T` fragments
    /// are path-transparent — their selections land directly at `base_path`, seeded by `base_count`
    /// representations.
    #[allow(clippy::too_many_arguments)]
    fn record_counts_in_fetch(
        &self,
        subgraph: &str,
        operation: &SerializableDocument,
        base_path: &[NormalizedPathElement],
        base_count: i32,
        is_entity_fetch: bool,
        variables: &Object,
        surveyed: &HashSet<NormalizedPath>,
        counts: &mut HashMap<NormalizedPath, i32>,
    ) -> Result<(), DemandControlError> {
        let Some(schema) = self.subgraph_schemas.get(subgraph) else {
            return Ok(());
        };
        let document = operation
            .as_parsed()
            .map_err(DemandControlError::SubgraphOperationNotInitialized)?;
        let Ok(operation) = document.operations.get(None) else {
            return Ok(());
        };

        if is_entity_fetch {
            for selection in &operation.selection_set.selections {
                if let Selection::Field(field) = selection
                    && field.name == ENTITIES_QUERY
                {
                    self.record_counts_in_selection_set(
                        schema,
                        subgraph,
                        &document.fragments,
                        &field.selection_set,
                        base_path.to_vec(),
                        base_count,
                        &[],
                        &[],
                        variables,
                        surveyed,
                        counts,
                    )?;
                }
            }
        } else {
            self.record_counts_in_selection_set(
                schema,
                subgraph,
                &document.fragments,
                &operation.selection_set,
                base_path.to_vec(),
                base_count,
                &[],
                &[],
                variables,
                surveyed,
                counts,
            )?;
        }
        Ok(())
    }

    /// Recurse a selection set, extending the response path and multiplying the object count by
    /// each list field's `instance_count` (sized exactly as `score_field` sizes a list). Records
    /// the count whenever the path matches a surveyed flatten path.
    /// - Mirrors `score_selection_set`/`score_selection`/`score_field`.
    #[allow(clippy::too_many_arguments)]
    fn record_counts_in_selection_set(
        &self,
        schema: &DemandControlledSchema,
        subgraph: &str,
        fragments: &FragmentMap,
        selection_set: &SelectionSet,
        current_path: NormalizedPath,
        current_count: i32,
        parent_list_sizes: &[ListSizeDirective],
        inherited_list_sizes: &[ListSizeDirective],
        variables: &Object,
        surveyed: &HashSet<NormalizedPath>,
        counts: &mut HashMap<NormalizedPath, i32>,
    ) -> Result<(), DemandControlError> {
        let parent_type = &selection_set.ty;
        for selection in &selection_set.selections {
            match selection {
                Selection::Field(field) => {
                    // Sizing mirrors `score_field`.
                    if field.name == TYPENAME || StaticCostCalculator::skipped_by_directives(field)
                    {
                        continue;
                    }

                    let definition = schema.output_field_definition(parent_type, &field.name);
                    let sizing = self.output_sizing(
                        definition,
                        field,
                        parent_list_sizes,
                        inherited_list_sizes,
                        subgraph,
                        variables,
                    )?;

                    let mut child_path = current_path.clone();
                    child_path.push(NormalizedPathElement::Key(
                        field.response_key().as_str().to_owned(),
                    ));
                    if field.ty().is_list() {
                        child_path.push(NormalizedPathElement::List);
                    }
                    let child_count = current_count.saturating_mul(sizing.instance_count);

                    if surveyed.contains(&child_path) {
                        let entry = counts.entry(child_path.clone()).or_insert(0);
                        *entry = (*entry).max(child_count);
                    }

                    self.record_counts_in_selection_set(
                        schema,
                        subgraph,
                        fragments,
                        &field.selection_set,
                        child_path,
                        child_count,
                        &sizing.own_list_size_directives,
                        &sizing.descended_list_sizes,
                        variables,
                        surveyed,
                        counts,
                    )?;
                }
                Selection::InlineFragment(inline_fragment) => {
                    self.record_counts_in_selection_set(
                        schema,
                        subgraph,
                        fragments,
                        &inline_fragment.selection_set,
                        current_path.clone(),
                        current_count,
                        parent_list_sizes,
                        inherited_list_sizes,
                        variables,
                        surveyed,
                        counts,
                    )?;
                }
                Selection::FragmentSpread(fragment_spread) => {
                    if let Some(fragment) = fragments.get(&fragment_spread.fragment_name) {
                        self.record_counts_in_selection_set(
                            schema,
                            subgraph,
                            fragments,
                            &fragment.selection_set,
                            current_path.clone(),
                            current_count,
                            parent_list_sizes,
                            inherited_list_sizes,
                            variables,
                            surveyed,
                            counts,
                        )?;
                    }
                }
            }
        }
        Ok(())
    }

    pub(crate) fn actual(
        &self,
        request: &ExecutableDocument,
        response: &Response,
        variables: &Object,
    ) -> Result<f64, DemandControlError> {
        let mut visitor = ResponseCostCalculator::new(&self.supergraph_schema);
        visitor.visit(request, response, variables);
        Ok(visitor.cost)
    }
}

/// Surveys every flatten path in the plan (the response paths entity fetches target), normalized
/// for matching against paths built while walking subgraph operations.
fn collect_flatten_paths(node: &PlanNode, out: &mut HashSet<NormalizedPath>) {
    match node {
        PlanNode::Sequence { nodes } | PlanNode::Parallel { nodes } => {
            for node in nodes {
                collect_flatten_paths(node, out);
            }
        }
        PlanNode::Flatten(flatten_node) => {
            out.insert(normalized_path(&flatten_node.path));
            collect_flatten_paths(&flatten_node.node, out);
        }
        PlanNode::Condition {
            condition: _,
            if_clause,
            else_clause,
        } => {
            if let Some(node) = if_clause {
                collect_flatten_paths(node, out);
            }
            if let Some(node) = else_clause {
                collect_flatten_paths(node, out);
            }
        }
        PlanNode::Defer { primary, deferred } => {
            if let Some(node) = &primary.node {
                collect_flatten_paths(node, out);
            }
            for deferred_node in deferred {
                if let Some(node) = &deferred_node.node {
                    collect_flatten_paths(node, out);
                }
            }
        }
        PlanNode::Fetch(_) | PlanNode::Subscription { .. } => {}
    }
}

pub(crate) struct ResponseCostCalculator<'a> {
    pub(crate) cost: f64,
    schema: &'a DemandControlledSchema,
}

impl<'schema> ResponseCostCalculator<'schema> {
    pub(crate) fn new(schema: &'schema DemandControlledSchema) -> Self {
        Self { cost: 0.0, schema }
    }

    fn score_response_field(
        &mut self,
        request: &ExecutableDocument,
        variables: &Object,
        parent_ty: &NamedType,
        field: &Field,
        value: &Value,
        include_argument_score: bool,
    ) {
        // When we pre-process the schema, __typename isn't included. So, we short-circuit here to avoid failed lookups.
        if field.name == TYPENAME {
            return;
        }

        let definition = self.schema.output_field_definition(parent_ty, &field.name);

        // We need to have a field definition for later processing, unless the query is an
        // `_entities` query. If the field should be there and isn't, return now.
        let is_entities_query = parent_ty == "Query" && field.name == ENTITIES_QUERY;
        if definition.is_none() && !is_entities_query {
            tracing::debug!(
                "Failed to get schema definition for field {}.{}. The resulting response cost will be a partial result.",
                parent_ty,
                field.name,
            );
            return;
        }

        match value {
            Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {
                self.cost += definition
                    .and_then(|d| d.cost_directive())
                    .map_or(0.0, |cost| cost.weight());
            }
            Value::Array(items) => {
                for item in items {
                    self.visit_list_item(request, variables, parent_ty, field, item);
                }
            }
            Value::Object(children) => {
                self.cost += definition
                    .and_then(|d| d.cost_directive())
                    .map_or(1.0, |cost| cost.weight());
                self.visit_selections(request, variables, &field.selection_set, children);
            }
        }

        if include_argument_score && let Some(definition) = definition {
            for argument in &field.arguments {
                if let Some(argument_definition) = definition.argument_by_name(&argument.name) {
                    if let Ok(score) =
                        score_argument(&argument.value, argument_definition, self.schema, variables)
                    {
                        self.cost += score;
                    }
                } else {
                    tracing::debug!(
                        "Failed to get schema definition for argument {}.{}({}:). The resulting response cost will be a partial result.",
                        parent_ty,
                        field.name,
                        argument.name,
                    )
                }
            }
        }
    }
}

impl ResponseVisitor for ResponseCostCalculator<'_> {
    fn visit_field(
        &mut self,
        request: &ExecutableDocument,
        variables: &Object,
        parent_ty: &NamedType,
        field: &Field,
        value: &Value,
    ) {
        self.score_response_field(request, variables, parent_ty, field, value, true);
    }

    fn visit_list_item(
        &mut self,
        request: &apollo_compiler::ExecutableDocument,
        variables: &Object,
        parent_ty: &apollo_compiler::executable::NamedType,
        field: &apollo_compiler::executable::Field,
        value: &Value,
    ) {
        self.score_response_field(request, variables, parent_ty, field, value, false);
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use ahash::HashMapExt;
    use apollo_compiler::validation::Valid;
    use apollo_federation::query_plan::query_planner::QueryPlanner;
    use bytes::Bytes;
    use test_log::test;
    use tower::Service;
    use tracing::instrument::WithSubscriber;

    use super::*;
    use crate::Configuration;
    use crate::Context;
    use crate::assert_snapshot_subscriber;
    use crate::compute_job::ComputeJobType;
    use crate::plugins::authorization::CacheKeyMetadata;
    use crate::query_planner::QueryPlannerService;
    use crate::services::QueryPlannerContent;
    use crate::services::QueryPlannerRequest;
    use crate::services::layers::query_analysis::ParsedDocument;
    use crate::services::query_planner::PlanOptions;
    use crate::spec;
    use crate::spec::Query;

    impl StaticCostCalculator {
        /// Seems unnecessary, but let's refactor later.
        fn rust_planned(
            &self,
            query_plan: &apollo_federation::query_plan::QueryPlan,
            variables: &Object,
        ) -> Result<f64, DemandControlError> {
            let planner_node: PlanNode = query_plan.node.as_ref().unwrap().into();
            let entity_counts = self.estimate_entity_counts(&planner_node, variables)?;
            let ctx = PlanScoringContext {
                variables,
                entity_counts: &entity_counts,
            };
            Ok(self.score_plan_node(&planner_node, &ctx)?.total())
        }
    }

    fn parse_schema_and_operation(
        schema_str: &str,
        query_str: &str,
        config: &Configuration,
    ) -> (spec::Schema, ParsedDocument) {
        let schema = spec::Schema::parse(schema_str, config).unwrap();
        let query = Query::parse_document(query_str, None, &schema, config).unwrap();
        (schema, query)
    }

    /// Estimate cost of an operation executed on a supergraph.
    ///
    /// Does not consider cost-by-subgraph or make use of `StaticCostCalculator::subgraph_list_size`,
    /// which are only available when estimating the cost against a query plan.
    fn estimated_cost(schema_str: &str, query_str: &str, variables_str: &str) -> f64 {
        let (schema, query) =
            parse_schema_and_operation(schema_str, query_str, &Default::default());
        let variables = serde_json::from_str::<Value>(variables_str)
            .unwrap()
            .as_object()
            .cloned()
            .unwrap_or_default();
        let schema =
            DemandControlledSchema::new(Arc::new(schema.supergraph_schema().clone())).unwrap();
        let calculator = StaticCostCalculator::new(
            Arc::new(schema),
            Default::default(),
            Default::default(),
            100,
        );

        calculator
            .estimated(
                &query.executable,
                &calculator.supergraph_schema,
                &variables,
                true,
                "",
                None,
            )
            .unwrap()
    }

    /// Estimate cost of an operation on a plain, non-federated schema.
    ///
    /// Does not consider cost-by-subgraph or make use of `StaticCostCalculator::subgraph_list_size`,
    /// which are only available when estimating the cost against a query plan.
    fn basic_estimated_cost(schema_str: &str, query_str: &str, variables_str: &str) -> f64 {
        let schema =
            apollo_compiler::Schema::parse_and_validate(schema_str, "schema.graphqls").unwrap();
        let query = apollo_compiler::ExecutableDocument::parse_and_validate(
            &schema,
            query_str,
            "query.graphql",
        )
        .unwrap();
        let variables = serde_json::from_str::<Value>(variables_str)
            .unwrap()
            .as_object()
            .cloned()
            .unwrap_or_default();
        let schema = DemandControlledSchema::new(Arc::new(schema)).unwrap();
        let calculator = StaticCostCalculator::new(
            Arc::new(schema),
            Default::default(),
            Default::default(),
            100,
        );

        calculator
            .estimated(
                &query,
                &calculator.supergraph_schema,
                &variables,
                true,
                "",
                None,
            )
            .unwrap()
    }

    async fn planned_cost_js(schema_str: &str, query_str: &str, variables_str: &str) -> f64 {
        let config: Arc<Configuration> = Arc::new(Default::default());
        let (schema, query) = parse_schema_and_operation(schema_str, query_str, &config);
        let variables = serde_json::from_str::<Value>(variables_str)
            .unwrap()
            .as_object()
            .cloned()
            .unwrap_or_default();
        let supergraph_schema = schema.supergraph_schema().clone();

        let mut planner = QueryPlannerService::new(schema.into(), config.clone())
            .await
            .unwrap();

        let ctx = Context::new();
        ctx.extensions()
            .with_lock(|lock| lock.insert::<ParsedDocument>(query.clone()));

        let planner_res = planner
            .call(QueryPlannerRequest::new(
                query_str.to_string(),
                None,
                query,
                CacheKeyMetadata::default(),
                PlanOptions::default(),
                ComputeJobType::QueryPlanning,
                variables.clone(),
            ))
            .await
            .unwrap();
        let query_plan = match planner_res.content.unwrap() {
            QueryPlannerContent::Plan { plan } => plan,
            _ => panic!("Query planner returned unexpected non-plan content"),
        };

        let schema = DemandControlledSchema::new(Arc::new(supergraph_schema)).unwrap();
        let mut demand_controlled_subgraph_schemas = HashMap::new();
        for (subgraph_name, subgraph_schema) in planner.subgraph_schemas().iter() {
            let demand_controlled_subgraph_schema =
                DemandControlledSchema::new(subgraph_schema.schema.clone()).unwrap();
            demand_controlled_subgraph_schemas
                .insert(subgraph_name.to_string(), demand_controlled_subgraph_schema);
        }

        let calculator = StaticCostCalculator::new(
            Arc::new(schema),
            Arc::new(demand_controlled_subgraph_schemas),
            Default::default(),
            100,
        );

        calculator.planned(&query_plan, &variables).unwrap().total()
    }

    fn planned_cost_rust(schema_str: &str, query_str: &str, variables_str: &str) -> f64 {
        let config: Arc<Configuration> = Arc::new(Default::default());
        let (schema, query) = parse_schema_and_operation(schema_str, query_str, &config);
        let variables = serde_json::from_str::<Value>(variables_str)
            .unwrap()
            .as_object()
            .cloned()
            .unwrap_or_default();

        let planner =
            QueryPlanner::new(schema.federation_supergraph(), Default::default()).unwrap();

        let query_plan = planner
            .build_query_plan(&query.executable, None, Default::default())
            .unwrap();

        let schema =
            DemandControlledSchema::new(Arc::new(schema.supergraph_schema().clone())).unwrap();
        let mut demand_controlled_subgraph_schemas = HashMap::new();
        for (subgraph_name, subgraph_schema) in planner.subgraph_schemas().iter() {
            let demand_controlled_subgraph_schema =
                DemandControlledSchema::new(Arc::new(subgraph_schema.schema().clone())).unwrap();
            demand_controlled_subgraph_schemas
                .insert(subgraph_name.to_string(), demand_controlled_subgraph_schema);
        }

        let calculator = StaticCostCalculator::new(
            Arc::new(schema),
            Arc::new(demand_controlled_subgraph_schemas),
            Default::default(),
            100,
        );

        calculator.rust_planned(&query_plan, &variables).unwrap()
    }

    fn actual_cost(
        schema_str: &str,
        query_str: &str,
        variables_str: &str,
        response_bytes: &'static [u8],
    ) -> f64 {
        let (schema, query) =
            parse_schema_and_operation(schema_str, query_str, &Default::default());
        let variables = serde_json::from_str::<Value>(variables_str)
            .unwrap()
            .as_object()
            .cloned()
            .unwrap_or_default();
        let response = Response::from_bytes(Bytes::from(response_bytes)).unwrap();
        let schema =
            DemandControlledSchema::new(Arc::new(schema.supergraph_schema().clone())).unwrap();
        StaticCostCalculator::new(
            Arc::new(schema),
            Default::default(),
            Default::default(),
            100,
        )
        .actual(&query.executable, &response, &variables)
        .unwrap()
    }

    /// Actual cost of an operation on a plain, non-federated schema.
    fn basic_actual_cost(
        schema_str: &str,
        query_str: &str,
        variables_str: &str,
        response_bytes: &'static [u8],
    ) -> f64 {
        let schema =
            apollo_compiler::Schema::parse_and_validate(schema_str, "schema.graphqls").unwrap();
        let query = apollo_compiler::ExecutableDocument::parse_and_validate(
            &schema,
            query_str,
            "query.graphql",
        )
        .unwrap();
        let variables = serde_json::from_str::<Value>(variables_str)
            .unwrap()
            .as_object()
            .cloned()
            .unwrap_or_default();
        let response = Response::from_bytes(Bytes::from(response_bytes)).unwrap();

        let schema = DemandControlledSchema::new(Arc::new(schema)).unwrap();
        StaticCostCalculator::new(
            Arc::new(schema),
            Default::default(),
            Default::default(),
            100,
        )
        .actual(&query, &response, &variables)
        .unwrap()
    }

    #[test]
    fn query_cost() {
        let schema = include_str!("./fixtures/basic_schema.graphql");
        let query = include_str!("./fixtures/basic_query.graphql");
        let variables = "{}";

        assert_eq!(basic_estimated_cost(schema, query, variables), 0.0)
    }

    #[test]
    fn mutation_cost() {
        let schema = include_str!("./fixtures/basic_schema.graphql");
        let query = include_str!("./fixtures/basic_mutation.graphql");
        let variables = "{}";

        assert_eq!(basic_estimated_cost(schema, query, variables), 10.0)
    }

    #[test]
    fn object_cost() {
        let schema = include_str!("./fixtures/basic_schema.graphql");
        let query = include_str!("./fixtures/basic_object_query.graphql");
        let variables = "{}";

        assert_eq!(basic_estimated_cost(schema, query, variables), 1.0)
    }

    #[test]
    fn interface_cost() {
        let schema = include_str!("./fixtures/basic_schema.graphql");
        let query = include_str!("./fixtures/basic_interface_query.graphql");
        let variables = "{}";

        assert_eq!(basic_estimated_cost(schema, query, variables), 1.0)
    }

    #[test]
    fn union_cost() {
        let schema = include_str!("./fixtures/basic_schema.graphql");
        let query = include_str!("./fixtures/basic_union_query.graphql");
        let variables = "{}";

        assert_eq!(basic_estimated_cost(schema, query, variables), 1.0)
    }

    #[test]
    fn list_cost() {
        let schema = include_str!("./fixtures/basic_schema.graphql");
        let query = include_str!("./fixtures/basic_object_list_query.graphql");
        let variables = "{}";

        assert_eq!(basic_estimated_cost(schema, query, variables), 100.0)
    }

    #[test]
    fn scalar_list_cost() {
        let schema = include_str!("./fixtures/basic_schema.graphql");
        let query = include_str!("./fixtures/basic_scalar_list_query.graphql");
        let variables = "{}";

        assert_eq!(basic_estimated_cost(schema, query, variables), 0.0)
    }

    #[test]
    fn nested_object_lists() {
        let schema = include_str!("./fixtures/basic_schema.graphql");
        let query = include_str!("./fixtures/basic_nested_list_query.graphql");
        let variables = "{}";

        assert_eq!(basic_estimated_cost(schema, query, variables), 10100.0)
    }

    #[test]
    fn input_object_cost() {
        let schema = include_str!("./fixtures/basic_schema.graphql");
        let query = include_str!("./fixtures/basic_input_object_query.graphql");
        let variables = "{}";

        assert_eq!(basic_estimated_cost(schema, query, variables), 4.0)
    }

    #[test]
    fn input_object_cost_with_returned_objects() {
        let schema = include_str!("./fixtures/basic_schema.graphql");
        let query = include_str!("./fixtures/basic_input_object_query_2.graphql");
        let variables = "{}";
        let response = include_bytes!("./fixtures/basic_input_object_response.json");

        assert_eq!(basic_estimated_cost(schema, query, variables), 104.0);
        // The cost of the arguments from the query should be included when scoring the response
        assert_eq!(basic_actual_cost(schema, query, variables, response), 7.0);
    }

    #[test]
    fn skip_directive_excludes_cost() {
        let schema = include_str!("./fixtures/basic_schema.graphql");
        let query = include_str!("./fixtures/basic_skipped_query.graphql");
        let variables = "{}";

        assert_eq!(basic_estimated_cost(schema, query, variables), 0.0)
    }

    #[test]
    fn include_directive_excludes_cost() {
        let schema = include_str!("./fixtures/basic_schema.graphql");
        let query = include_str!("./fixtures/basic_excluded_query.graphql");
        let variables = "{}";

        assert_eq!(basic_estimated_cost(schema, query, variables), 0.0)
    }

    #[test(tokio::test)]
    async fn fragments_cost() {
        let schema = include_str!("./fixtures/basic_supergraph_schema.graphql");
        let query = include_str!("./fixtures/basic_fragments_query.graphql");
        let variables = "{}";

        assert_eq!(basic_estimated_cost(schema, query, variables), 102.0);
        assert_eq!(planned_cost_js(schema, query, variables).await, 102.0);
        assert_eq!(planned_cost_rust(schema, query, variables), 102.0);
    }

    #[test(tokio::test)]
    async fn federated_query_with_name() {
        let schema = include_str!("./fixtures/federated_ships_schema.graphql");
        let query = include_str!("./fixtures/federated_ships_named_query.graphql");
        let variables = "{}";
        let response = include_bytes!("./fixtures/federated_ships_named_response.json");

        assert_eq!(estimated_cost(schema, query, variables), 100.0);
        assert_eq!(actual_cost(schema, query, variables, response), 2.0);
    }

    #[test(tokio::test)]
    async fn federated_query_with_requires() {
        let schema = include_str!("./fixtures/federated_ships_schema.graphql");
        let query = include_str!("./fixtures/federated_ships_required_query.graphql");
        let variables = "{}";
        let response = include_bytes!("./fixtures/federated_ships_required_response.json");

        assert_eq!(estimated_cost(schema, query, variables), 10200.0);
        assert_eq!(planned_cost_js(schema, query, variables).await, 10400.0);
        assert_eq!(planned_cost_rust(schema, query, variables), 10400.0);
        assert_eq!(actual_cost(schema, query, variables, response), 2.0);
    }

    #[test(tokio::test)]
    async fn entity_fetch_cardinality_uses_parent_list_size_from_listsize_directive() {
        // When the parent list field has @listSize(assumedSize: 5) and global list_size is 100,
        // the entity fetch for _entities should be estimated with cardinality 5, not 100.
        let schema = include_str!("./fixtures/federated_ships_listsize_schema.graphql");
        let query = include_str!("./fixtures/federated_ships_required_query.graphql");
        let variables = "{}";

        // Supergraph estimate uses @listSize(assumedSize: 5) from ships field:
        // ships (5 instances) × Ship (1.0) = 5.0
        assert_eq!(estimated_cost(schema, query, variables), 5.0);

        // Planned cost: entity fetch _entities should use cardinality 5 (ships list size),
        // not 100 (global list_size default).
        // Fetch 1 (vehicles, ships list):       ships[5] × Ship(1.0) = 5.0
        // Fetch 2 (vehicles, entity fetch):     _entities[5] × Ship(1.0) = 5.0
        // Total = 10.0
        //
        // Before the fix: Fetch 2 would use list_size=100, giving 100.0, total = 105.0
        assert_eq!(planned_cost_rust(schema, query, variables), 10.0);
    }

    #[test(tokio::test)]
    async fn entity_fetch_cardinality_multiplies_for_nested_lists() {
        // For a two-level nested list (companies[N].employees[M]), the entity fetch
        // for employees should be estimated with cardinality N × M.
        // With list_size = 100: cardinality = 100 companies × 100 employees = 10000.
        //
        // Before the fix: cardinality would be 100 (just global list_size).
        let schema = include_str!("./fixtures/federated_nested_list_schema.graphql");
        let query = include_str!("./fixtures/federated_nested_list_query.graphql");
        let variables = "{}";

        // Fetch 1 (companies): companies[100] × Company(1.0) + employees[100] × Employee(1.0) = 100 + 10000 = 10100.0
        // Fetch 2 (employees, entity):  _entities[10000] × Employee(1.0) = 10000.0
        // Total = 20100.0
        //
        // Before the fix: Fetch 2 = _entities[100] × 1.0 = 100.0, total = 10200.0
        assert_eq!(planned_cost_rust(schema, query, variables), 20100.0);
    }

    #[test(tokio::test)]
    async fn federated_query_with_fragments() {
        let schema = include_str!("./fixtures/federated_ships_schema.graphql");
        let query = include_str!("./fixtures/federated_ships_fragment_query.graphql");
        let variables = "{}";
        let response = include_bytes!("./fixtures/federated_ships_fragment_response.json");

        assert_eq!(estimated_cost(schema, query, variables), 300.0);
        assert_eq!(planned_cost_js(schema, query, variables).await, 400.0);
        assert_eq!(planned_cost_rust(schema, query, variables), 400.0);
        assert_eq!(actual_cost(schema, query, variables, response), 6.0);
    }

    #[test(tokio::test)]
    async fn federated_query_with_inline_fragments() {
        let schema = include_str!("./fixtures/federated_ships_schema.graphql");
        let query = include_str!("./fixtures/federated_ships_inline_fragment_query.graphql");
        let variables = "{}";
        let response = include_bytes!("./fixtures/federated_ships_fragment_response.json");

        assert_eq!(estimated_cost(schema, query, variables), 300.0);
        assert_eq!(planned_cost_js(schema, query, variables).await, 400.0);
        assert_eq!(planned_cost_rust(schema, query, variables), 400.0);
        assert_eq!(actual_cost(schema, query, variables, response), 6.0);
    }

    #[test(tokio::test)]
    async fn federated_query_with_defer() {
        let schema = include_str!("./fixtures/federated_ships_schema.graphql");
        let query = include_str!("./fixtures/federated_ships_deferred_query.graphql");
        let variables = "{}";
        let response = include_bytes!("./fixtures/federated_ships_deferred_response.json");

        assert_eq!(estimated_cost(schema, query, variables), 10200.0);
        assert_eq!(planned_cost_js(schema, query, variables).await, 10400.0);
        assert_eq!(planned_cost_rust(schema, query, variables), 10400.0);
        assert_eq!(actual_cost(schema, query, variables, response), 2.0);
    }

    #[test(tokio::test)]
    async fn federated_query_with_adjustable_list_cost() {
        // NB: does not consider cost-by-subgraph or make use of `StaticCostCalculator::subgraph_list_size`,
        // which are only available when estimating the cost against a query plan.
        let schema = include_str!("./fixtures/federated_ships_schema.graphql");
        let query = include_str!("./fixtures/federated_ships_deferred_query.graphql");
        let (schema, query) = parse_schema_and_operation(schema, query, &Default::default());
        let schema = Arc::new(
            DemandControlledSchema::new(Arc::new(schema.supergraph_schema().clone())).unwrap(),
        );

        let calculator =
            StaticCostCalculator::new(schema.clone(), Default::default(), Default::default(), 100);
        let conservative_estimate = calculator
            .estimated(
                &query.executable,
                &calculator.supergraph_schema,
                &Default::default(),
                true,
                "",
                None,
            )
            .unwrap();

        let calculator =
            StaticCostCalculator::new(schema.clone(), Default::default(), Default::default(), 5);
        let narrow_estimate = calculator
            .estimated(
                &query.executable,
                &calculator.supergraph_schema,
                &Default::default(),
                true,
                "",
                None,
            )
            .unwrap();

        assert_eq!(conservative_estimate, 10200.0);
        assert_eq!(narrow_estimate, 35.0);
    }

    #[test(tokio::test)]
    async fn federated_query_with_typenames() {
        let schema = include_str!("./fixtures/federated_ships_schema.graphql");
        let query = include_str!("./fixtures/federated_ships_typename_query.graphql");
        let variables = "{}";
        let response = include_bytes!("./fixtures/federated_ships_typename_response.json");

        async {
            assert_eq!(actual_cost(schema, query, variables, response), 2.0);
        }
        // This was previously logging a warning for every __typename in the response. At the time of writing,
        // this should not produce logs. Generally, it should not produce undue noise for valid requests.
        .with_subscriber(assert_snapshot_subscriber!())
        .await
    }

    #[test(tokio::test)]
    async fn custom_cost_query() {
        let schema = include_str!("./fixtures/custom_cost_schema.graphql");
        let query = include_str!("./fixtures/custom_cost_query.graphql");
        let variables = "{}";
        let response = include_bytes!("./fixtures/custom_cost_response.json");

        assert_eq!(estimated_cost(schema, query, variables), 127.0);
        assert_eq!(planned_cost_js(schema, query, variables).await, 127.0);
        assert_eq!(planned_cost_rust(schema, query, variables), 127.0);
        assert_eq!(actual_cost(schema, query, variables, response), 125.0);
    }

    #[test(tokio::test)]
    async fn custom_cost_query_with_renamed_directives() {
        let schema = include_str!("./fixtures/custom_cost_schema_with_renamed_directives.graphql");
        let query = include_str!("./fixtures/custom_cost_query.graphql");
        let variables = "{}";
        let response = include_bytes!("./fixtures/custom_cost_response.json");

        assert_eq!(estimated_cost(schema, query, variables), 127.0);
        assert_eq!(planned_cost_js(schema, query, variables).await, 127.0);
        assert_eq!(planned_cost_rust(schema, query, variables), 127.0);
        assert_eq!(actual_cost(schema, query, variables, response), 125.0);
    }

    #[test(tokio::test)]
    async fn custom_cost_query_with_default_slicing_argument() {
        let schema = include_str!("./fixtures/custom_cost_schema.graphql");
        let query =
            include_str!("./fixtures/custom_cost_query_with_default_slicing_argument.graphql");
        let variables = "{}";
        let response = include_bytes!("./fixtures/custom_cost_response.json");

        assert_eq!(estimated_cost(schema, query, variables), 132.0);
        assert_eq!(planned_cost_js(schema, query, variables).await, 132.0);
        assert_eq!(planned_cost_rust(schema, query, variables), 132.0);
        assert_eq!(actual_cost(schema, query, variables, response), 125.0);
    }

    #[test(tokio::test)]
    async fn custom_cost_query_with_variable_slicing_argument() {
        let schema = include_str!("./fixtures/custom_cost_schema.graphql");
        let query =
            include_str!("./fixtures/custom_cost_query_with_variable_slicing_argument.graphql");
        let variables = r#"{"costlyInput": {"somethingWithCost": 10}, "fieldCountVar": 5}"#;
        let response = include_bytes!("./fixtures/custom_cost_response.json");

        assert_eq!(estimated_cost(schema, query, variables), 127.0);
        assert_eq!(planned_cost_js(schema, query, variables).await, 127.0);
        assert_eq!(planned_cost_rust(schema, query, variables), 127.0);
        assert_eq!(actual_cost(schema, query, variables, response), 125.0);
    }

    #[test]
    fn arbitrary_json_as_custom_scalar_in_variables() {
        let schema = include_str!("./fixtures/arbitrary_json_schema.graphql");
        let query = r#"
            query FetchData($myJsonValue: ArbitraryJson) {
                fetch(args: {
                    json: $myJsonValue
                })
            }
        "#;
        let variables = r#"
            {
                "myJsonValue": {
                    "field.with.dots": 1
                }
            }
        "#;

        assert_eq!(estimated_cost(schema, query, variables), 1.0);
    }

    #[test(tokio::test)]
    async fn subscription_request() {
        let schema = include_str!("./fixtures/subscription_schema.graphql");
        let query = include_str!("./fixtures/subscription_query.graphql");
        let variables = "{}";

        assert_eq!(estimated_cost(schema, query, variables), 1.0);
        assert_eq!(planned_cost_js(schema, query, variables).await, 1.0);
        assert_eq!(planned_cost_rust(schema, query, variables), 1.0);
    }

    /// Tests to ensure Vec-based implementation (for supporting multiple @listSize directives)
    /// maintains backward compatibility with existing single-directive behavior
    mod backward_compatibility_tests {
        use super::estimated_cost;

        const SCHEMA: &str = include_str!("./fixtures/custom_cost_schema.graphql");

        #[rstest::rstest]
        #[case::no_directive("query { enumWithCost }", "{}", 15.0)]
        #[case::single_slicing_argument_with_array(
            r#"query { itemsByIds(ids: ["a", "b"]) { id } }"#,
            "{}",
            2.0
        )]
        #[case::slicing_argument_with_variable(
            r#"query Q($ids: [ID!]!) { itemsByIds(ids: $ids) { id } }"#,
            r#"{"ids": ["x", "y", "z"]}"#,
            3.0
        )]
        #[case::nested_sized_fields(
            r#"query { containerWithNestedList(first: 5) { page { id } } }"#,
            "{}",
            6.0
        )]
        #[case::assumed_size_fallback(
            r#"query Q($ids: [ID!]) { itemsByIdsWithAssumedSize(ids: $ids) { id } }"#,
            r#"{"ids": null}"#,
            50.0
        )]
        #[case::sized_fields_propagate_to_nested_lists(
            r#"query { fieldWithDynamicListSize { items { id } } }"#,
            "{}",
            11.0  // SizedField: 1, items: 10 * 1 = 10 (from default first: 10)
        )]
        fn vec_based_implementation_maintains_backward_compatibility(
            #[case] query: &str,
            #[case] variables: &str,
            #[case] expected_cost: f64,
        ) {
            assert_eq!(estimated_cost(SCHEMA, query, variables), expected_cost);
        }
    }

    /// Tests for array-based slicing arguments in @listSize directive
    mod array_slicing_argument_tests {
        use super::estimated_cost;

        const SCHEMA: &str = include_str!("./fixtures/custom_cost_schema.graphql");

        #[rstest::rstest]
        #[case::inline_array_of_3(
            r#"query { itemsByIds(ids: ["a", "b", "c"]) { id } }"#,
            "{}",
            3.0
        )]
        #[case::empty_inline_array(r#"query { itemsByIds(ids: []) { id } }"#, "{}", 0.0)]
        #[case::variable_array_of_5(
            r#"query Q($ids: [ID!]!) { itemsByIds(ids: $ids) { id } }"#,
            r#"{"ids": ["a", "b", "c", "d", "e"]}"#,
            5.0
        )]
        #[case::variable_empty_array(
            r#"query Q($ids: [ID!]!) { itemsByIds(ids: $ids) { id } }"#,
            r#"{"ids": []}"#,
            0.0
        )]
        fn array_length_determines_list_size(
            #[case] query: &str,
            #[case] variables: &str,
            #[case] expected_cost: f64,
        ) {
            assert_eq!(estimated_cost(SCHEMA, query, variables), expected_cost);
        }

        #[rstest::rstest]
        #[case::null_variable(r#"{"ids": null}"#)]
        #[case::missing_variable("{}")]
        fn null_or_missing_array_falls_back_to_assumed_size(#[case] variables: &str) {
            let query = r#"query Q($ids: [ID!]) { itemsByIdsWithAssumedSize(ids: $ids) { id } }"#;
            // assumedSize is 50 in the schema
            assert_eq!(estimated_cost(SCHEMA, query, variables), 50.0);
        }
    }

    /// Tests for nested input path resolution in @listSize slicingArguments
    ///
    /// Note: Expected costs include the cost of input objects (1 per nested object).
    /// For inline objects: SearchInput (1) + PaginationInput (1) = 2 base cost
    /// For variables: input objects are costed based on their nesting level
    mod nested_input_path_tests {
        use super::estimated_cost;

        const SCHEMA: &str = include_str!("./fixtures/custom_cost_schema.graphql");

        // Input object costs:
        // - SearchInput: 1
        // - PaginationInput: 1
        // Total input cost for search queries: 2

        #[rstest::rstest]
        #[case::inline_nested_first_10(
            r#"query { search(input: {pagination: {first: 10}}) { id } }"#,
            "{}",
            12.0  // 10 (list size) + 2 (input objects: SearchInput + PaginationInput)
        )]
        #[case::inline_nested_first_5(
            r#"query { search(input: {pagination: {first: 5}, query: "test"}) { id } }"#,
            "{}",
            7.0  // 5 (list size) + 2 (input objects)
        )]
        #[case::variable_nested_object(
            r#"query Q($input: SearchInput!) { search(input: $input) { id } }"#,
            r#"{"input": {"pagination": {"first": 7}, "query": "test"}}"#,
            9.0  // 7 (list size) + 2 (input objects)
        )]
        #[case::variable_nested_first_only(
            r#"query Q($input: SearchInput!) { search(input: $input) { id } }"#,
            r#"{"input": {"pagination": {"first": 3}}}"#,
            5.0  // 3 (list size) + 2 (input objects)
        )]
        fn nested_path_determines_list_size(
            #[case] query: &str,
            #[case] variables: &str,
            #[case] expected_cost: f64,
        ) {
            assert_eq!(estimated_cost(SCHEMA, query, variables), expected_cost);
        }

        // Input object costs for searchWithAssumedSize:
        // - SearchInput: 1
        // - PaginationInput: 1 (if present)
        // When path not found, falls back to assumedSize (25)

        #[rstest::rstest]
        #[case::missing_nested_value(
            r#"{"input": {"pagination": {}}}"#,
            27.0  // 25 (assumed size) + 2 (SearchInput + PaginationInput)
        )]
        #[case::missing_pagination(
            r#"{"input": {}}"#,
            26.0  // 25 (assumed size) + 1 (SearchInput only)
        )]
        #[case::null_input(
            r#"{"input": null}"#,
            25.0  // 25 (assumed size) + 0 (null is not scored)
        )]
        fn missing_nested_path_falls_back_to_assumed_size(
            #[case] variables: &str,
            #[case] expected_cost: f64,
        ) {
            let query =
                r#"query Q($input: SearchInput) { searchWithAssumedSize(input: $input) { id } }"#;
            assert_eq!(estimated_cost(SCHEMA, query, variables), expected_cost);
        }

        // DeeplyNestedInput has 3 levels: DeeplyNestedInput(1) + NestedLevel1Input(1) + NestedLevel2Input(1) = 3

        #[test]
        fn deeply_nested_path_inline() {
            let query = r#"query { deeplyNested(input: {level1: {level2: {count: 15}}}) { id } }"#;
            // 15 (list size) + 3 (input objects: DeeplyNestedInput + NestedLevel1Input + NestedLevel2Input)
            assert_eq!(estimated_cost(SCHEMA, query, "{}"), 18.0);
        }

        #[test]
        fn deeply_nested_path_variable() {
            let query =
                r#"query Q($input: DeeplyNestedInput!) { deeplyNested(input: $input) { id } }"#;
            let variables = r#"{"input": {"level1": {"level2": {"count": 12}}}}"#;
            // 12 (list size) + 3 (input objects)
            assert_eq!(estimated_cost(SCHEMA, query, variables), 15.0);
        }

        #[test]
        fn inline_nested_object_with_other_fields() {
            // Ensure other fields in the nested object don't affect the size resolution
            let query = r#"query { search(input: {pagination: {first: 8, after: "cursor"}, query: "search term"}) { id } }"#;
            // 8 (list size) + 2 (input objects: SearchInput + PaginationInput)
            assert_eq!(estimated_cost(SCHEMA, query, "{}"), 10.0);
        }
    }

    /// Nested sizedFields in @listSize (e.g. "results { page }")
    mod nested_sized_fields_tests {
        use super::estimated_cost;

        const SCHEMA: &str = include_str!("./fixtures/custom_cost_schema.graphql");

        #[rstest::rstest]
        #[case::simple_sized_fields_on_nested_type(
            r#"query { containerWithNestedList(first: 5) { page { id } metadata } }"#,
            "{}",
            6.0  // ResultContainer: 1, page: 5 * 1 = 5, metadata: 0
        )]
        #[case::nested_sized_fields_two_levels(
            r#"query { deepContainerWithNestedList(first: 7) { results { page { id } } } }"#,
            "{}",
            9.0  // DeepContainer: 1, results: 1, page: 7 * 1 = 7
        )]
        #[case::nested_sized_fields_with_variable(
            r#"query Q($n: Int!) { deepContainerWithNestedList(first: $n) { results { page { id } } } }"#,
            r#"{"n": 3}"#,
            5.0
        )]
        #[case::nested_sized_fields_with_default_value(
            r#"query { deepContainerWithNestedList { results { page { id } } } }"#,
            "{}",
            12.0  // default first: 10
        )]
        #[case::nested_sized_fields_not_selected(
            r#"query { deepContainerWithNestedList(first: 100) { total } }"#,
            "{}",
            1.0
        )]
        #[case::intermediate_container_without_sized_field(
            r#"query { deepContainerWithNestedList(first: 100) { results { metadata } } }"#,
            "{}",
            2.0
        )]
        #[case::mixed_sized_fields_single_and_nested(
            r#"query {
                deepContainerWithMixedSizedFields(first: 5) {
                    page { id }
                    results { page { id } }
                }
            }"#,
            "{}",
            12.0  // DeepContainer: 1, page: 5 * 1 = 5, results: 1, page: 5 * 1 = 5
        )]
        fn nested_sized_fields_cases(
            #[case] query: &str,
            #[case] variables: &str,
            #[case] expected_cost: f64,
        ) {
            assert_eq!(estimated_cost(SCHEMA, query, variables), expected_cost);
        }

        /// Schema load fails when a sizedFields path has more than one leaf (one-leaf-per-path rule).
        #[test]
        fn multiple_leaves_in_one_path_fails_at_schema_load() {
            use std::sync::Arc;

            use crate::plugins::demand_control::cost_calculator::schema::DemandControlledSchema;
            use crate::spec;

            // Schema with sizedFields: ["results { page metadata }"] - two leaves in one path
            let schema_str = include_str!("./fixtures/custom_cost_schema.graphql").replace(
                r#"sizedFields: ["results { page }"]"#,
                r#"sizedFields: ["results { page metadata }"]"#,
            );
            // ResultContainer has page: [A] and metadata: String. So "results { page metadata }"
            // has two top-level selections with no sub-selections (page is a leaf, metadata is a leaf).
            let schema = spec::Schema::parse(&schema_str, &Default::default()).unwrap();
            let result = DemandControlledSchema::new(Arc::new(schema.supergraph_schema().clone()));

            match &result {
                Err(e) => assert!(
                    e.to_string().contains("at most one list field per path"),
                    "expected error about one list field per path, got: {}",
                    e
                ),
                Ok(_) => {
                    panic!("expected schema load to fail for multiple list fields in one path")
                }
            }
        }
    }

    /// Walks `query_str` (parsed against `schema`) with the count walk and returns the count
    /// recorded at `path`. Exercises `record_counts_in_selection_set` directly — the core of the
    /// count pre-pass — without building a full query plan.
    fn recorded_count_for(
        schema: &Valid<apollo_compiler::Schema>,
        query_str: &str,
        path: &[&str],
        list_size: u32,
    ) -> Option<i32> {
        let executable =
            ExecutableDocument::parse_and_validate(schema, query_str, "query.graphql").unwrap();
        let dc_schema = Arc::new(DemandControlledSchema::new(Arc::new(schema.clone())).unwrap());
        let calc =
            StaticCostCalculator::new(dc_schema, Default::default(), Default::default(), list_size);
        let operation = executable.operations.get(None).unwrap();

        let target = normalized_path(&crate::json_ext::Path::from_slice(path));
        let mut surveyed = HashSet::default();
        surveyed.insert(target.clone());
        let mut counts = HashMap::default();
        calc.record_counts_in_selection_set(
            &calc.supergraph_schema,
            "",
            &executable.fragments,
            &operation.selection_set,
            Vec::new(),
            1,
            &[],
            &[],
            &Object::new(),
            &surveyed,
            &mut counts,
        )
        .unwrap();
        counts.get(&target).copied()
    }

    /// Convenience wrapper for plain (non-federated) schemas that don't need `@link` resolution.
    fn count_test(schema_str: &str, query_str: &str, path: &[&str], list_size: u32) -> Option<i32> {
        let schema =
            apollo_compiler::Schema::parse_and_validate(schema_str, "schema.graphqls").unwrap();
        recorded_count_for(&schema, query_str, path, list_size)
    }

    #[test]
    fn count_single_list_no_directive() {
        // ships: [Ship!]! with no @listSize → global list_size (10).
        let schema = r#"
            type Query { ships: [Ship!]! }
            type Ship { id: ID! }
        "#;
        assert_eq!(
            count_test(schema, "{ ships { id } }", &["ships", "@"], 10),
            Some(10)
        );
    }

    #[test]
    fn count_nested_lists_multiply() {
        // companies[10] × employees[10] = 100 at companies/@/employees/@.
        let schema = r#"
            type Query { companies: [Company!]! }
            type Company { employees: [Employee!]! }
            type Employee { id: ID! }
        "#;
        assert_eq!(
            count_test(
                schema,
                "{ companies { employees { id } } }",
                &["companies", "@", "employees", "@"],
                10
            ),
            Some(100)
        );
    }

    #[test]
    fn count_object_field_does_not_multiply() {
        // user (non-list) then orders[10] → 1 × 10 = 10 at user/orders/@.
        let schema = r#"
            type Query { user: User! }
            type User { orders: [Order!]! }
            type Order { id: ID! }
        "#;
        assert_eq!(
            count_test(
                schema,
                "{ user { orders { id } } }",
                &["user", "orders", "@"],
                10
            ),
            Some(10)
        );
    }

    #[test]
    fn count_slicing_argument_from_query() {
        // itemsByIds(ids: [...]) @listSize(slicingArguments: ["ids"]) → reads the query argument's
        // array length (3), not the global list_size (100).
        let schema_str = include_str!("./fixtures/custom_cost_schema.graphql");
        let config: Configuration = Default::default();
        let schema = crate::spec::Schema::parse(schema_str, &config).unwrap();
        assert_eq!(
            recorded_count_for(
                schema.supergraph_schema(),
                r#"{ itemsByIds(ids: ["a", "b", "c"]) { id } }"#,
                &["itemsByIds", "@"],
                100
            ),
            Some(3)
        );
    }

    /// Builds a rust query plan for a federated schema and returns the estimated entity count
    /// recorded at `path`, exercising `estimate_entity_counts` directly.
    fn entity_count_at(
        schema_str: &str,
        query_str: &str,
        variables_str: &str,
        path: &[&str],
        list_size: u32,
    ) -> Option<i32> {
        let config: Arc<Configuration> = Arc::new(Default::default());
        let (schema, query) = parse_schema_and_operation(schema_str, query_str, &config);
        let variables = serde_json::from_str::<Value>(variables_str)
            .unwrap()
            .as_object()
            .cloned()
            .unwrap_or_default();

        let planner =
            QueryPlanner::new(schema.federation_supergraph(), Default::default()).unwrap();
        let query_plan = planner
            .build_query_plan(&query.executable, None, Default::default())
            .unwrap();

        let dc_schema =
            DemandControlledSchema::new(Arc::new(schema.supergraph_schema().clone())).unwrap();
        let mut subgraph_schemas = HashMap::new();
        for (subgraph_name, subgraph_schema) in planner.subgraph_schemas().iter() {
            subgraph_schemas.insert(
                subgraph_name.to_string(),
                DemandControlledSchema::new(Arc::new(subgraph_schema.schema().clone())).unwrap(),
            );
        }
        let calculator = StaticCostCalculator::new(
            Arc::new(dc_schema),
            Arc::new(subgraph_schemas),
            Default::default(),
            list_size,
        );

        let node: PlanNode = query_plan.node.as_ref().unwrap().into();
        let counts = calculator
            .estimate_entity_counts(&node, &variables)
            .unwrap();
        counts
            .get(&normalized_path(&crate::json_ext::Path::from_slice(path)))
            .copied()
    }

    #[test]
    fn entity_counts_single_list_uses_parent_assumed_size() {
        // ships: [Ship!]! @listSize(assumedSize: 5). The entity fetch at /ships/@ is seeded with
        // the 5 ships the producing fetch yields, not the global list_size (100).
        let schema = include_str!("./fixtures/federated_ships_listsize_schema.graphql");
        let query = include_str!("./fixtures/federated_ships_required_query.graphql");
        assert_eq!(
            entity_count_at(schema, query, "{}", &["ships", "@"], 100),
            Some(5)
        );
    }

    #[test]
    fn entity_counts_nested_lists_multiply() {
        // companies[100] × employees[100] = 10000 employees reach the entity-fetch path, the
        // product recorded by the producing (companies) fetch.
        let schema = include_str!("./fixtures/federated_nested_list_schema.graphql");
        let query = include_str!("./fixtures/federated_nested_list_query.graphql");
        assert_eq!(
            entity_count_at(
                schema,
                query,
                "{}",
                &["companies", "@", "employees", "@"],
                100
            ),
            Some(10000)
        );
    }
}
