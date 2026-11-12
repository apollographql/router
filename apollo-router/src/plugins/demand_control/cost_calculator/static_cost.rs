use std::sync::Arc;

use ahash::HashMap;
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
use crate::json_ext::Path;
use crate::json_ext::PathElement;
use crate::plugins::demand_control::cost_calculator::directives::ListSizeDirective;
use crate::query_planner::DeferredNode;
use crate::query_planner::PlanNode;
use crate::query_planner::Primary;
use crate::query_planner::QueryPlan;
use crate::query_planner::fetch::FetchNode;
use crate::query_planner::rewrites::DataRewrite;
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
    /// Estimated entity count at each entity-fetch flatten path. Precomputed by
    /// `estimate_entity_counts`.
    entity_counts: &'a EntityCounts,
}

/// Estimated number of objects at each response path that an entity fetch targets.
///
/// This is a trie over the plan's flatten paths, reduced to what identifies a position in the
/// response shape: field response keys and list (`@`) markers. Type conditions are ignored, so
/// a count is the number of objects at a path regardless of their concrete type. Only flatten
/// paths (and their prefixes) have nodes, which lets the walk over a fetch's operation skip any
/// selection that can't lead to an entity fetch.
#[derive(Default)]
struct EntityCounts {
    keys: HashMap<String, EntityCounts>,
    list: Option<Box<EntityCounts>>,
    /// The largest number of objects any fetch seen so far places at this path.
    count: Option<i32>,
}

impl EntityCounts {
    fn is_empty(&self) -> bool {
        self.keys.is_empty() && self.list.is_none()
    }

    /// Adds nodes for a flatten path and its prefixes.
    fn insert(&mut self, path: &Path) {
        let mut node = self;
        for element in &path.0 {
            node = match element {
                PathElement::Key(key, _) => node.keys.entry(key.clone()).or_default(),
                PathElement::Flatten(_) => node.list.get_or_insert_default(),
                // Flatten paths are static, so they don't contain these.
                PathElement::Index(_) | PathElement::Fragment(_) => node,
            };
        }
    }

    fn node(&self, path: &Path) -> Option<&EntityCounts> {
        let mut node = self;
        for element in &path.0 {
            node = match element {
                PathElement::Key(key, _) => node.keys.get(key)?,
                PathElement::Flatten(_) => node.list.as_deref()?,
                PathElement::Index(_) | PathElement::Fragment(_) => node,
            };
        }
        Some(node)
    }

    fn node_mut(&mut self, path: &Path) -> Option<&mut EntityCounts> {
        let mut node = self;
        for element in &path.0 {
            node = match element {
                PathElement::Key(key, _) => node.keys.get_mut(key)?,
                PathElement::Flatten(_) => node.list.as_deref_mut()?,
                PathElement::Index(_) | PathElement::Fragment(_) => node,
            };
        }
        Some(node)
    }

    /// The estimated number of objects at a flatten path, if a fetch that produces them was seen.
    fn count_at(&self, path: &Path) -> Option<i32> {
        self.node(path)?.count
    }

    /// The node for the field `response_key` of type `ty` selected at this path. A list type
    /// adds one `@` per level of list nesting, like the query planner does for flatten paths.
    fn field_mut(&mut self, response_key: &str, ty: &ast::Type) -> Option<&mut EntityCounts> {
        let mut node = self.keys.get_mut(response_key)?;
        let mut ty = ty;
        while let ast::Type::List(inner) | ast::Type::NonNullList(inner) = ty {
            node = node.list.as_deref_mut()?;
            ty = inner;
        }
        Some(node)
    }

    fn record(&mut self, count: i32) {
        self.count = Some(self.count.map_or(count, |existing| existing.max(count)));
    }
}

/// The parts of walking one fetch's operation to count objects (`record_counts_in_selection_set`)
/// that stay the same throughout the walk.
struct CountingContext<'a> {
    schema: &'a DemandControlledSchema,
    subgraph: &'a str,
    fragments: &'a FragmentMap,
    variables: &'a Object,
    /// Response keys the query planner aliased in this fetch's operation, mapped to the response
    /// key the data ends up under (which is the one flatten paths use).
    renamed_keys: HashMap<&'a str, &'a str>,
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

    /// The size assumed for a list in `subgraph` that has no `@listSize`: the per-subgraph
    /// configured default, then the global `list_size`.
    fn default_list_size(&self, subgraph: &str) -> i32 {
        let list_size = self.subgraph_list_size(subgraph).unwrap_or(self.list_size);
        // Saturate rather than cast: a configured list size above `i32::MAX` must read as
        // very large (expensive), never wrap to a negative value.
        i32::try_from(list_size).unwrap_or(i32::MAX)
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
        //
        // A list's instance count can never be negative. Guard the invariant here so no source —
        // client slicing arguments, upstream propagation, or configured defaults — can drive the
        // estimated cost or an entity count below zero.
        let instance_count = if field.ty().is_list() {
            list_size_from_upstream
                .or(effective_expected_size)
                .unwrap_or_else(|| self.default_list_size(subgraph))
                .max(0)
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
            hint.max(0)
        } else {
            sizing.instance_count
        };

        // Per-call cost: the cost to resolve this field once (from @cost on the field
        // definition, plus arguments and directives). Counted once regardless of list size.
        let field_call_weight = definition
            .field_cost_directive()
            .map_or(0.0, |cost| cost.weight());

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

        // TODO we should be computing cost of directives applied on the field as well
        // Negative weights can be used to indicate that certain arguments are less expensive to run target resolver.
        // Overall cost of a field cannot be negative, if it is we must round it up to zero.
        // see: https://ibm.github.io/graphql-specs/cost-spec.html#sec-Field-Cost.Example-Negative-Weights
        let field_call_cost = (field_call_weight + arguments_cost).max(0.0);

        let type_instance_cost = if let Some(cost_directive) = definition.type_cost_directive() {
            // negative weights can only be specified on arguments
            cost_directive.weight().max(0.0)
        } else if definition.ty().is_interface()
            || definition.ty().is_object()
            || definition.ty().is_union()
        {
            1.0
        } else {
            0.0
        };

        let child_cost = self.score_selection_set(
            ctx,
            &field.selection_set,
            field.ty().inner_named_type(),
            &sizing.own_list_size_directives,
            &sizing.descended_list_sizes,
            subgraph,
        )?;

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

        let cost = field_call_cost
            + (instance_count as f64) * (type_instance_cost + child_cost)
            + requirements_cost;
        tracing::debug!(
            "Overall field {} cost breakdown: field_call_cost {} + ((count) {} * (type_instance_cost {} + child_selection {})) + (requirements) {} = {}",
            field.name,
            field_call_cost,
            instance_count,
            type_instance_cost,
            child_cost,
            requirements_cost,
            cost,
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
        let mut cost = operation_base_cost(operation);

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

    // TODO: Evaluate the directives using the ctx.variables.
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
                    let entity_count = ctx.entity_counts.count_at(&flatten_node.path);
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
            // TODO: Evaluate the condition using the ctx.variables.
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
        let Some(root) = query_plan.root.as_ref() else {
            return Ok(CostBySubgraph::default());
        };
        let entity_counts = self.estimate_entity_counts(root, variables)?;
        let ctx = PlanScoringContext {
            variables,
            entity_counts: &entity_counts,
        };
        self.score_plan_node(root, &ctx)
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
    ) -> Result<EntityCounts, DemandControlError> {
        let mut counts = EntityCounts::default();
        collect_flatten_paths(root, &mut counts);
        // Without flatten paths there are no entity fetches to size, so skip walking the fetches.
        if !counts.is_empty() {
            self.accumulate_entity_counts(root, variables, &mut counts)?;
        }
        Ok(counts)
    }

    /// Walk the plan in execution order, recording object counts at flatten paths. Fetches are
    /// processed before the flattens that depend on them (`Sequence` order), so an entity fetch's
    /// base count is already in `counts` by the time we reach it.
    fn accumulate_entity_counts(
        &self,
        node: &PlanNode,
        variables: &Object,
        counts: &mut EntityCounts,
    ) -> Result<(), DemandControlError> {
        match node {
            PlanNode::Sequence { nodes } | PlanNode::Parallel { nodes } => {
                for node in nodes {
                    self.accumulate_entity_counts(node, variables, counts)?;
                }
            }
            PlanNode::Fetch(fetch_node) => {
                // Root fetch: its selections land at the response root, one object each.
                self.record_counts_in_fetch(fetch_node, counts, 1, variables)?;
            }
            PlanNode::Flatten(flatten_node) => {
                if let PlanNode::Fetch(fetch_node) = flatten_node.node.as_ref() {
                    // Every flatten path was added by `collect_flatten_paths`.
                    let Some(base) = counts.node_mut(&flatten_node.path) else {
                        return Ok(());
                    };
                    // The objects being extended already exist at the flatten path; their count was
                    // recorded by an earlier fetch. If we somehow haven't seen the producer, fall
                    // back to the size `_entities` is scored with when it has no count.
                    let base_count = base
                        .count
                        .unwrap_or_else(|| self.default_list_size(&fetch_node.service_name));
                    self.record_counts_in_fetch(fetch_node, base, base_count, variables)?;
                } else {
                    self.accumulate_entity_counts(&flatten_node.node, variables, counts)?;
                }
            }
            PlanNode::Condition {
                condition: _,
                if_clause,
                else_clause,
            } => {
                if let Some(node) = if_clause {
                    self.accumulate_entity_counts(node, variables, counts)?;
                }
                if let Some(node) = else_clause {
                    self.accumulate_entity_counts(node, variables, counts)?;
                }
            }
            PlanNode::Defer { primary, deferred } => {
                if let Some(node) = &primary.node {
                    self.accumulate_entity_counts(node, variables, counts)?;
                }
                for deferred_node in deferred {
                    if let Some(node) = &deferred_node.node {
                        self.accumulate_entity_counts(node, variables, counts)?;
                    }
                }
            }
            // A subscription's follow-up fetches (`rest`) aren't scored, so they aren't counted.
            PlanNode::Subscription { .. } => {}
        }
        Ok(())
    }

    /// Walk one fetch's subgraph operation, recording object counts at the flatten paths under
    /// `base` (the response path the fetch's data lands at, holding `base_count` objects). For an
    /// entity fetch the `_entities` field and its `... on T` fragments are path-transparent — their
    /// selections land directly at `base`, once per representation.
    fn record_counts_in_fetch(
        &self,
        fetch_node: &FetchNode,
        base: &mut EntityCounts,
        base_count: i32,
        variables: &Object,
    ) -> Result<(), DemandControlError> {
        // Nothing below this fetch's path is the target of an entity fetch.
        if base.is_empty() {
            return Ok(());
        }
        let subgraph = fetch_node.service_name.as_ref();
        let Some(schema) = self.subgraph_schemas.get(subgraph) else {
            return Ok(());
        };
        let document = fetch_node
            .operation
            .as_parsed()
            .map_err(DemandControlError::SubgraphOperationNotInitialized)?;
        let Ok(operation) = document.operations.get(None) else {
            return Ok(());
        };

        // When selections in a fetch can't be merged under one response key, the query planner
        // aliases them and adds a rewrite that restores the original key in the fetched data.
        // Flatten paths refer to the data after rewrites, so map the aliases back.
        let renamed_keys = fetch_node
            .output_rewrites
            .iter()
            .flatten()
            .filter_map(|rewrite| match rewrite {
                DataRewrite::KeyRenamer(renamer) => match renamer.path.0.last() {
                    Some(PathElement::Key(alias, _)) => {
                        Some((alias.as_str(), renamer.rename_key_to.as_str()))
                    }
                    _ => None,
                },
                DataRewrite::ValueSetter(_) => None,
            })
            .collect();
        let ctx = CountingContext {
            schema,
            subgraph,
            fragments: &document.fragments,
            variables,
            renamed_keys,
        };

        if fetch_node.requires.is_empty() {
            self.record_counts_in_selection_set(
                &ctx,
                &operation.selection_set,
                base,
                base_count,
                &[],
                &[],
            )?;
        } else {
            for selection in &operation.selection_set.selections {
                if let Selection::Field(field) = selection
                    && field.name == ENTITIES_QUERY
                {
                    self.record_counts_in_selection_set(
                        &ctx,
                        &field.selection_set,
                        base,
                        base_count,
                        &[],
                        &[],
                    )?;
                }
            }
        }
        Ok(())
    }

    /// Recurse a selection set, following the response path through `counts` and multiplying the
    /// object count by each list field's `instance_count` (sized exactly as `score_field` sizes a
    /// list). Records the count at every flatten path it reaches, and skips selections that don't
    /// lead to one.
    /// - Mirrors `score_selection_set`/`score_selection`/`score_field`.
    fn record_counts_in_selection_set(
        &self,
        ctx: &CountingContext,
        selection_set: &SelectionSet,
        counts: &mut EntityCounts,
        current_count: i32,
        parent_list_sizes: &[ListSizeDirective],
        inherited_list_sizes: &[ListSizeDirective],
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

                    let response_key = field.response_name().as_str();
                    let response_key = ctx
                        .renamed_keys
                        .get(response_key)
                        .copied()
                        .unwrap_or(response_key);
                    let Some(child_counts) = counts.field_mut(response_key, field.ty()) else {
                        continue;
                    };

                    let definition = ctx.schema.output_field_definition(parent_type, &field.name);
                    let sizing = self.output_sizing(
                        definition,
                        field,
                        parent_list_sizes,
                        inherited_list_sizes,
                        ctx.subgraph,
                        ctx.variables,
                    )?;
                    let child_count = current_count.saturating_mul(sizing.instance_count);
                    child_counts.record(child_count);

                    if !child_counts.is_empty() {
                        self.record_counts_in_selection_set(
                            ctx,
                            &field.selection_set,
                            child_counts,
                            child_count,
                            &sizing.own_list_size_directives,
                            &sizing.descended_list_sizes,
                        )?;
                    }
                }
                Selection::InlineFragment(inline_fragment) => {
                    self.record_counts_in_selection_set(
                        ctx,
                        &inline_fragment.selection_set,
                        counts,
                        current_count,
                        parent_list_sizes,
                        inherited_list_sizes,
                    )?;
                }
                Selection::FragmentSpread(fragment_spread) => {
                    if let Some(fragment) = ctx.fragments.get(&fragment_spread.fragment_name) {
                        self.record_counts_in_selection_set(
                            ctx,
                            &fragment.selection_set,
                            counts,
                            current_count,
                            parent_list_sizes,
                            inherited_list_sizes,
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
        let base_cost: f64 = request
            .operations
            .iter()
            .map(|op| operation_base_cost(op))
            .sum();
        Ok(base_cost + visitor.cost)
    }
}

/// Flat cost added to an operation on top of its field costs. Estimated and actual
/// cost must both include it, or every mutation reports a constant delta.
fn operation_base_cost(operation: &Operation) -> f64 {
    if operation.is_mutation() { 10.0 } else { 0.0 }
}

/// Adds every flatten path in the plan (the response paths entity fetches target) to `out`.
fn collect_flatten_paths(node: &PlanNode, out: &mut EntityCounts) {
    match node {
        PlanNode::Sequence { nodes } | PlanNode::Parallel { nodes } => {
            for node in nodes {
                collect_flatten_paths(node, out);
            }
        }
        PlanNode::Flatten(flatten_node) => {
            out.insert(&flatten_node.path);
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

        let mut response_field_cost: f64 = 0.0;

        // Per-call cost: field @cost + arguments, counted once per field resolution
        if include_argument_score {
            response_field_cost += definition
                .and_then(|d| d.field_cost_directive())
                .map_or(0.0, |cost| cost.weight());

            if let Some(definition) = definition {
                for argument in &field.arguments {
                    if let Some(argument_definition) = definition.argument_by_name(&argument.name) {
                        if let Ok(score) = score_argument(
                            &argument.value,
                            argument_definition,
                            self.schema,
                            variables,
                        ) {
                            response_field_cost += score;
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

        // per-instance type cost + child selections
        // NOTE: this is an upper bound as we might not know the actual returned object type
        match value {
            Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {
                response_field_cost += definition
                    .and_then(|d| d.type_cost_directive())
                    .map_or(0.0, |cost| cost.weight());
            }
            Value::Array(items) => {
                for item in items {
                    self.visit_list_item(request, variables, parent_ty, field, item);
                }
            }
            Value::Object(children) => {
                response_field_cost += definition
                    .and_then(|d| d.type_cost_directive())
                    .map_or(1.0, |cost| cost.weight());
                self.visit_selections(request, variables, &field.selection_set, children);
            }
        }

        // Overall cost of a resolved field cannot be negative, if it is we must round it up to zero.
        self.cost += response_field_cost.max(0.0);
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
    use crate::services::QueryPlannerRequest;
    use crate::services::query_parsing::ParsedDocument;
    use crate::services::query_planner::PlanOptions;
    use crate::spec;
    use crate::spec::Query;

    impl StaticCostCalculator {
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

        let schema_arc: Arc<crate::spec::Schema> = schema.into();
        let qp_arc = QueryPlannerService::create_planner(&schema_arc, &config).unwrap();
        let subgraph_schemas = crate::query_planner::build_subgraph_schemas(&qp_arc);
        let mut planner = QueryPlannerService::new(schema_arc, config.clone(), qp_arc);

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
            ))
            .await
            .unwrap();
        let query_plan = planner_res.content.unwrap();

        let schema = DemandControlledSchema::new(Arc::new(supergraph_schema)).unwrap();
        let mut demand_controlled_subgraph_schemas = HashMap::new();
        for (subgraph_name, subgraph_schema) in subgraph_schemas.iter() {
            let demand_controlled_subgraph_schema =
                DemandControlledSchema::new(subgraph_schema.clone()).unwrap();
            demand_controlled_subgraph_schemas
                .insert(subgraph_name.clone(), demand_controlled_subgraph_schema);
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
    fn mutation_actual_cost_includes_base_cost() {
        let schema = include_str!("./fixtures/basic_schema.graphql");
        let query = include_str!("./fixtures/basic_mutation.graphql");
        let response = br#"{"data":{"doSomething":6}}"#;

        assert_eq!(basic_actual_cost(schema, query, "{}", response), 10.0)
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
    fn negative_slicing_argument_is_clamped_and_never_reduces_cost() {
        let schema = include_str!("./fixtures/custom_cost_schema.graphql");
        let query =
            include_str!("./fixtures/custom_cost_query_with_variable_slicing_argument.graphql");

        // A negative client-supplied slicing value must be treated as zero, not as negative
        // cost that would deflate the estimate below what the operation actually costs.
        let negative = estimated_cost(
            schema,
            query,
            r#"{"costlyInput": {"somethingWithCost": 10}, "fieldCountVar": -10}"#,
        );
        let zero = estimated_cost(
            schema,
            query,
            r#"{"costlyInput": {"somethingWithCost": 10}, "fieldCountVar": 0}"#,
        );

        assert!(
            negative >= 0.0,
            "estimated cost must never be negative, got {negative}"
        );
        assert_eq!(
            negative, zero,
            "a negative slicing value must be clamped to zero"
        );
    }

    #[test]
    fn oversized_configured_list_size_does_not_wrap_negative() {
        // A configured default list size above `i32::MAX` must saturate to a large (expensive)
        // value, never wrap to a negative one that would invert the estimate.
        let schema_str = include_str!("./fixtures/basic_schema.graphql");
        let query_str = include_str!("./fixtures/basic_object_list_query.graphql");
        let schema =
            apollo_compiler::Schema::parse_and_validate(schema_str, "schema.graphqls").unwrap();
        let query = apollo_compiler::ExecutableDocument::parse_and_validate(
            &schema,
            query_str,
            "query.graphql",
        )
        .unwrap();
        let schema = DemandControlledSchema::new(Arc::new(schema)).unwrap();
        let calculator = StaticCostCalculator::new(
            Arc::new(schema),
            Default::default(),
            Default::default(),
            u32::MAX,
        );

        let cost = calculator
            .estimated(
                &query,
                &calculator.supergraph_schema,
                &Default::default(),
                true,
                "",
                None,
            )
            .unwrap();

        assert!(
            cost >= 0.0,
            "estimated cost must never be negative for an oversized configured list size, got {cost}"
        );
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
        #[case::no_directive("query { enumWithCost }", "{}", 15.0)] // AorB enum has @cost(weight: 15)
        #[case::single_slicing_argument_with_array(
            r#"query { itemsByIds(ids: ["a", "b"]) { id } }"#,
            "{}",
            2.0  // list size = 2 (inline array length)
        )]
        #[case::slicing_argument_with_variable(
            r#"query Q($ids: [ID!]!) { itemsByIds(ids: $ids) { id } }"#,
            r#"{"ids": ["x", "y", "z"]}"#,
            3.0  // list size = 3 (variable array length)
        )]
        #[case::nested_sized_fields(
            r#"query { containerWithNestedList(first: 5) { page { id } } }"#,
            "{}",
            6.0  // ResultContainer: 1, page: 5 * 1 = 5
        )]
        #[case::assumed_size_fallback(
            r#"query Q($ids: [ID!]) { itemsByIdsWithAssumedSize(ids: $ids) { id } }"#,
            r#"{"ids": null}"#,
            50.0  // assumedSize is 50 in the schema
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
            3.0  // list size = 3 (inline array length)
        )]
        #[case::empty_inline_array(r#"query { itemsByIds(ids: []) { id } }"#, "{}", 0.0)] // list size = 0 (empty array)
        #[case::variable_array_of_5(
            r#"query Q($ids: [ID!]!) { itemsByIds(ids: $ids) { id } }"#,
            r#"{"ids": ["a", "b", "c", "d", "e"]}"#,
            5.0  // list size = 5 (variable array length)
        )]
        #[case::variable_empty_array(
            r#"query Q($ids: [ID!]!) { itemsByIds(ids: $ids) { id } }"#,
            r#"{"ids": []}"#,
            0.0  // list size = 0 (empty variable array)
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
            5.0  // DeepContainer: 1, results: 1, page: 3 * 1 = 3
        )]
        #[case::nested_sized_fields_with_default_value(
            r#"query { deepContainerWithNestedList { results { page { id } } } }"#,
            "{}",
            12.0  // default first: 10 → DeepContainer: 1, results: 1, page: 10 * 1 = 10
        )]
        #[case::nested_sized_fields_not_selected(
            r#"query { deepContainerWithNestedList(first: 100) { total } }"#,
            "{}",
            1.0  // DeepContainer: 1 (sized field `results.page` not selected, so list size unused)
        )]
        #[case::intermediate_container_without_sized_field(
            r#"query { deepContainerWithNestedList(first: 100) { results { metadata } } }"#,
            "{}",
            2.0  // DeepContainer: 1, results: 1 (metadata: 0, sized field `page` not selected)
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

        let target = Path::from_slice(path);
        let mut counts = EntityCounts::default();
        counts.insert(&target);
        let ctx = CountingContext {
            schema: &calc.supergraph_schema,
            subgraph: "",
            fragments: &executable.fragments,
            variables: &Object::new(),
            renamed_keys: Default::default(),
        };
        calc.record_counts_in_selection_set(
            &ctx,
            &operation.selection_set,
            &mut counts,
            1,
            &[],
            &[],
        )
        .unwrap();
        counts.count_at(&target)
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

    #[test]
    fn count_type_conditions_do_not_sum() {
        // The same response key (`toys`) is selected under two object type conditions, each a list
        // of the global `list_size`. The recorded count is that size — the branches are merged
        // (max), not summed.
        let schema = r#"
            type Query { animal: Animal }
            interface Animal { id: ID! }
            type Cat implements Animal { id: ID! toys: [Toy] }
            type Dog implements Animal { id: ID! toys: [Toy] }
            type Toy { id: ID! }
        "#;
        assert_eq!(
            count_test(
                schema,
                "{ animal { ... on Cat { toys { id } } ... on Dog { toys { id } } } }",
                &["animal", "toys", "@"],
                10,
            ),
            Some(10)
        );
    }

    #[test]
    fn count_type_conditions_take_max_size() {
        // The same response key (`toys`) resolves to a different `@listSize` depending on the
        // concrete type — `Cat` → 3, `Dog` → 7. The recorded count is the max across the
        // type-condition branches, regardless of the order they appear in. (Uses a cost-spec
        // supergraph because `@listSize` needs `@link` resolution that plain SDL can't carry.)
        let schema_str =
            include_str!("./fixtures/federated_type_condition_listsize_schema.graphql");
        let config: Configuration = Default::default();
        let schema = crate::spec::Schema::parse(schema_str, &config).unwrap();
        let supergraph = schema.supergraph_schema();

        assert_eq!(
            recorded_count_for(
                supergraph,
                "{ animal { ... on Cat { toys { id } } ... on Dog { toys { id } } } }",
                &["animal", "toys", "@"],
                100,
            ),
            Some(7)
        );
        // Swapped order: still the max, proving it is neither first- nor last-wins.
        assert_eq!(
            recorded_count_for(
                supergraph,
                "{ animal { ... on Dog { toys { id } } ... on Cat { toys { id } } } }",
                &["animal", "toys", "@"],
                100,
            ),
            Some(7)
        );
    }

    /// Builds a rust query plan for a federated schema, along with a calculator to score it.
    fn plan_and_calculator(
        schema_str: &str,
        query_str: &str,
        list_size: u32,
        subgraph_list_sizes: SubgraphConfiguration<Option<u32>>,
    ) -> (PlanNode, StaticCostCalculator) {
        let config: Arc<Configuration> = Arc::new(Default::default());
        let (schema, query) = parse_schema_and_operation(schema_str, query_str, &config);

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
            Arc::new(subgraph_list_sizes),
            list_size,
        );

        (query_plan.node.as_ref().unwrap().into(), calculator)
    }

    fn parse_variables(variables_str: &str) -> Object {
        serde_json::from_str::<Value>(variables_str)
            .unwrap()
            .as_object()
            .cloned()
            .unwrap_or_default()
    }

    /// Returns the estimated entity count recorded at `path` for a rust query plan, exercising
    /// `estimate_entity_counts` directly.
    fn entity_count_at(
        schema_str: &str,
        query_str: &str,
        variables_str: &str,
        path: &[&str],
        list_size: u32,
    ) -> Option<i32> {
        let (node, calculator) =
            plan_and_calculator(schema_str, query_str, list_size, Default::default());
        let counts = calculator
            .estimate_entity_counts(&node, &parse_variables(variables_str))
            .unwrap();
        counts.count_at(&Path::from_slice(path))
    }

    /// Returns the estimated cost per subgraph of a rust query plan.
    fn planned_cost_by_subgraph(
        schema_str: &str,
        query_str: &str,
        variables_str: &str,
        list_size: u32,
        subgraph_list_sizes: SubgraphConfiguration<Option<u32>>,
    ) -> CostBySubgraph {
        let (node, calculator) =
            plan_and_calculator(schema_str, query_str, list_size, subgraph_list_sizes);
        let variables = parse_variables(variables_str);
        let entity_counts = calculator
            .estimate_entity_counts(&node, &variables)
            .unwrap();
        let ctx = PlanScoringContext {
            variables: &variables,
            entity_counts: &entity_counts,
        };
        calculator.score_plan_node(&node, &ctx).unwrap()
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

    const ENTITY_COUNT_SCHEMA: &str =
        include_str!("./fixtures/federated_entity_count_schema.graphql");

    #[test]
    fn entity_fetch_follows_slicing_argument_variable() {
        // The entity fetch for `detail` extends the items returned by `itemsByIds`, whose size
        // comes from the length of the `ids` variable rather than `list_size` (10).
        let query = "query($ids: [ID!]!) { itemsByIds(ids: $ids) { detail { text } } }";
        for (ids, count) in [(r#"["a"]"#, 1.0), (r#"["a","b","c"]"#, 3.0)] {
            let variables = format!(r#"{{"ids": {ids}}}"#);
            assert_eq!(
                entity_count_at(
                    ENTITY_COUNT_SCHEMA,
                    query,
                    &variables,
                    &["itemsByIds", "@"],
                    10
                ),
                Some(count as i32)
            );
            let cost = planned_cost_by_subgraph(
                ENTITY_COUNT_SCHEMA,
                query,
                &variables,
                10,
                Default::default(),
            );
            // lists: itemsByIds[n] × Item(1)
            assert_eq!(cost.get("lists"), Some(count));
            // details: _entities[n] × (Item(1) + Detail(1))
            assert_eq!(cost.get("details"), Some(count * 2.0));
        }
    }

    #[test]
    fn entity_fetch_follows_aliased_response_key() {
        // Flatten paths use response keys, so an aliased field is matched by its alias.
        let query = r#"{ mine: itemsByIds(ids: ["a", "b"]) { detail { text } } }"#;
        assert_eq!(
            entity_count_at(ENTITY_COUNT_SCHEMA, query, "{}", &["mine", "@"], 10),
            Some(2)
        );
        assert_eq!(
            entity_count_at(ENTITY_COUNT_SCHEMA, query, "{}", &["itemsByIds", "@"], 10),
            None
        );
    }

    #[test]
    fn entity_fetch_follows_mutation_root_field() {
        let query = r#"mutation { createItems(ids: ["a", "b", "c"]) { detail { text } } }"#;
        assert_eq!(
            entity_count_at(ENTITY_COUNT_SCHEMA, query, "{}", &["createItems", "@"], 10),
            Some(3)
        );
    }

    #[test]
    fn entity_fetch_follows_sized_fields() {
        // `page` sizes its `items` field through `@listSize(assumedSize: 4, sizedFields:)`.
        let query = "{ page { items { detail { text } } } }";
        assert_eq!(
            entity_count_at(
                ENTITY_COUNT_SCHEMA,
                query,
                "{}",
                &["page", "items", "@"],
                10
            ),
            Some(4)
        );
    }

    #[test]
    fn entity_fetch_follows_nested_list_type() {
        // `grid: [[Item!]!]!` has one `@` per list level in the flatten path. Like cost scoring,
        // the whole nested list is sized as a single list.
        let query = "{ grid { detail { text } } }";
        assert_eq!(
            entity_count_at(ENTITY_COUNT_SCHEMA, query, "{}", &["grid", "@", "@"], 10),
            Some(10)
        );
    }

    #[test]
    fn entity_fetch_follows_lists_from_earlier_entity_fetch() {
        // `related` is resolved by an entity fetch at `items/@` and is itself a list (of 2), so the
        // entity fetch at `items/@/related/@` is sized as items[10] × related[2].
        let query = "{ items { related { name } } }";
        assert_eq!(
            entity_count_at(
                ENTITY_COUNT_SCHEMA,
                query,
                "{}",
                &["items", "@", "related", "@"],
                10
            ),
            Some(20)
        );
    }

    #[test]
    fn entity_fetch_follows_key_aliased_by_query_planner() {
        // In the `lists` subgraph, `content` has a different type in each implementation of `Box`
        // (`[Item!]!` and `[Item!]`), so the two selections can't share a response key. The query
        // planner aliases one of them in the subgraph operation and renames it back in the
        // response. The flatten path uses the original response key, so the larger of the two
        // lists (boxes[3] × content[5]) must be counted whichever selection gets the alias.
        let small = "... on SmallBox { content { detail { text } } }";
        let big = "... on BigBox { content { detail { text } } }";
        for query in [
            format!("{{ boxes {{ {small} {big} }} }}"),
            format!("{{ boxes {{ {big} {small} }} }}"),
        ] {
            let (node, _) =
                plan_and_calculator(ENTITY_COUNT_SCHEMA, &query, 10, Default::default());
            assert!(
                format!("{node:?}").contains("content__alias_"),
                "expected the query planner to alias `content`"
            );
            assert_eq!(
                entity_count_at(
                    ENTITY_COUNT_SCHEMA,
                    &query,
                    "{}",
                    &["boxes", "@", "content", "@"],
                    10
                ),
                Some(15)
            );
        }
    }

    #[test]
    fn negative_slicing_argument_does_not_produce_negative_entity_fetch_cost() {
        let query = "{ firstItems(first: -5) { detail { text } } }";
        assert_eq!(
            entity_count_at(ENTITY_COUNT_SCHEMA, query, "{}", &["firstItems", "@"], 10),
            Some(0)
        );
        let cost =
            planned_cost_by_subgraph(ENTITY_COUNT_SCHEMA, query, "{}", 10, Default::default());
        assert_eq!(cost.get("lists"), Some(0.0));
        assert_eq!(cost.get("details"), Some(0.0));
    }

    #[test]
    fn entity_fetch_uses_producing_subgraph_list_size() {
        // The items are produced by `lists`, so its configured list size (3) sizes the entity
        // fetch sent to `details`, not the `details` list size (7) or the global one (10).
        let subgraph_list_sizes = SubgraphConfiguration {
            all: None,
            subgraphs: [
                ("lists".to_string(), Some(3)),
                ("details".to_string(), Some(7)),
            ]
            .into_iter()
            .collect(),
        };
        let cost = planned_cost_by_subgraph(
            ENTITY_COUNT_SCHEMA,
            "{ items { detail { text } } }",
            "{}",
            10,
            subgraph_list_sizes,
        );
        assert_eq!(cost.get("lists"), Some(3.0));
        assert_eq!(cost.get("details"), Some(6.0));
    }

    /// Tests for `@cost` on types: objects, interface implementations, and union members.
    /// For interfaces, the max `@cost` across all implementing object types is used.
    /// For unions, the max `@cost` across all member types is used.
    mod type_cost_tests {
        use super::estimated_cost;

        const SCHEMA: &str = include_str!("./fixtures/type_cost_schema.graphql");

        #[test]
        fn object_with_cost_directive() {
            // CostlyObject has @cost(weight: 7)
            let cost = estimated_cost(SCHEMA, "query { objectWithCost { id } }", "{}");
            assert_eq!(cost, 7.0);
        }

        #[test]
        fn object_without_cost_directive_defaults_to_one() {
            // PlainObject has no @cost, defaults to 1.0
            let cost = estimated_cost(SCHEMA, "query { objectNoCost { id } }", "{}");
            assert_eq!(cost, 1.0);
        }

        #[test]
        fn interface_uses_max_cost_from_implementations() {
            // Animal: Cat @cost(3), Dog @cost(5) → max is 5
            let cost = estimated_cost(SCHEMA, "query { cheapAnimal { name } }", "{}");
            assert_eq!(cost, 5.0);
        }

        #[test]
        fn interface_without_cost_on_implementations_defaults_to_one() {
            // NoCostAnimal: Fish (no @cost), Bird (no @cost) → default 1.0
            let cost = estimated_cost(SCHEMA, "query { noCostInterface { name } }", "{}");
            assert_eq!(cost, 1.0);
        }

        #[test]
        fn union_uses_max_cost_from_members() {
            // SearchResult: Article @cost(2), Video @cost(8) → max is 8
            let cost = estimated_cost(
                SCHEMA,
                "query { searchResult { ... on Article { title } ... on Video { url } } }",
                "{}",
            );
            assert_eq!(cost, 8.0);
        }

        #[test]
        fn union_with_mixed_cost_uses_max() {
            // MixedUnion: CostlyMember @cost(10), PlainMember (no @cost) → max is 10
            let cost = estimated_cost(
                SCHEMA,
                "query { mixedUnion { ... on CostlyMember { value } ... on PlainMember { value } } }",
                "{}",
            );
            assert_eq!(cost, 10.0);
        }

        #[test]
        fn union_without_cost_on_members_defaults_to_one() {
            // NoCostUnion: PlainA (no @cost), PlainB (no @cost) → default 1.0
            let cost = estimated_cost(
                SCHEMA,
                "query { noCostUnion { ... on PlainA { id } ... on PlainB { id } } }",
                "{}",
            );
            assert_eq!(cost, 1.0);
        }
    }

    /// Tests for negative cost clamping per the cost spec.
    /// Field cost must never be negative — if `@cost` weights (especially negative argument
    /// weights) produce a negative total, it is clamped to zero.
    /// See: https://ibm.github.io/graphql-specs/cost-spec.html#sec-Field-Cost.Example-Negative-Weights
    mod negative_cost_tests {
        use super::actual_cost;
        use super::estimated_cost;

        const SCHEMA: &str = include_str!("./fixtures/negative_cost_schema.graphql");

        #[rstest::rstest]
        #[case::negative_arg_clamps_to_zero(
            "query { fieldWithNegativeArgCost(discount: 1) }",
            "{}",
            0.0  // field(5) + arg(-20) = -15 → clamped to 0
        )]
        #[case::exact_zero_stays_zero(
            "query { fieldWithExactZeroCost(offset: 1) }",
            "{}",
            0.0  // field(10) + arg(-10) = 0
        )]
        #[case::positive_result_not_clamped(
            "query { fieldWithPartialNegativeArg(offset: 1) }",
            "{}",
            7.0  // field(10) + arg(-3) = 7
        )]
        #[case::mixed_args_net_negative_clamps_to_zero(
            "query { fieldWithMixedArgs(a: 1, b: 1) }",
            "{}",
            0.0  // field(10) + arg_a(-30) + arg_b(5) = -15 → clamped to 0
        )]
        #[case::object_field_with_negative_arg_clamps_to_zero(
            "query { objectWithNegativeArgCost(discount: 1) { id } }",
            "{}",
            1.0  // field_call = max(0, 5 + (-50)) = 0, type_instance = 1 (NegCostObject) → 0 + 1*(1+0) = 1
        )]
        #[case::child_cost_preserved_when_field_cost_clamps(
            "query { nestedObjectWithNegativeArgCost(discount: 1) { inner { value } } }",
            "{}",
            2.0  // field_call = max(0, 5 + (-50)) = 0, type_instance = 1, child(inner) = 1 → 0 + 1*(1+1) = 2
        )]
        fn estimated_cost_clamps_negative_to_zero(
            #[case] query: &str,
            #[case] variables: &str,
            #[case] expected_cost: f64,
        ) {
            assert_eq!(estimated_cost(SCHEMA, query, variables), expected_cost);
        }

        #[rstest::rstest]
        #[case::scalar_response_clamps_to_zero(
            "query { fieldWithNegativeArgCost(discount: 1) }",
            "{}",
            br#"{"data": {"fieldWithNegativeArgCost": "hello"}}"#,
            0.0  // scalar weight(0) + arg(-20) = -20 → clamped to 0
        )]
        #[case::object_response_clamps_to_zero(
            "query { objectWithNegativeArgCost(discount: 1) { id } }",
            "{}",
            br#"{"data": {"objectWithNegativeArgCost": {"id": "1"}}}"#,
            0.0  // object weight(5) + arg(-50) = -45 → clamped to 0
        )]
        #[case::positive_response_cost_not_clamped(
            "query { fieldWithPartialNegativeArg(offset: 1) }",
            "{}",
            br#"{"data": {"fieldWithPartialNegativeArg": "hello"}}"#,
            7.0  // scalar with @cost(weight: 10) + arg(-3) = 7, not clamped
        )]
        fn actual_cost_clamps_negative_to_zero(
            #[case] query: &str,
            #[case] variables: &str,
            #[case] response_bytes: &'static [u8],
            #[case] expected_cost: f64,
        ) {
            assert_eq!(
                actual_cost(SCHEMA, query, variables, response_bytes),
                expected_cost
            );
        }
    }
}
