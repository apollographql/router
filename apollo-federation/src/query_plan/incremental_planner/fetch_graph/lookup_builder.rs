//! Materializing entity fetch groups into GraphQL Federation source schemas as lookup fetches.
//!
//! Planning treats a key hop into a source schema like any other: the fetch graph holds one entity
//! group per subgraph and response path, fed by `@key` inputs. Only materialization differs. An
//! `_entities` fetch resolves every entity type of the group in one field; a lookup operation calls
//! one lookup field, so the group becomes one fetch per lookup. Each fetch:
//!
//! - selects the group's selections under the lookup field (reached through the lookup's
//!   argumentless chain from the query root);
//! - declares one variable per lookup argument, computed per entity from its representation by
//!   the argument's `@is` map (the key alternative the edge used, per entity type);
//! - passes `@require` arguments of the fields it selects as variables computed the same way from
//!   the requirement data the representation carries;
//! - keeps `requires` (the representation selection) and the input rewrites, so the executor builds
//!   exactly the representations an `_entities` fetch would have sent.

use std::sync::Arc;

use apollo_compiler::Name;
use apollo_compiler::Node;
use apollo_compiler::collections::IndexMap;
use apollo_compiler::collections::IndexSet;
use apollo_compiler::executable;
use apollo_compiler::executable::VariableDefinition;
use petgraph::Direction;
use petgraph::stable_graph::NodeIndex;
use petgraph::visit::EdgeRef;

use super::FETCH_COST;
use super::FetchGraph;
use super::FetchGroupKind;
use super::InputContribution;
use super::plan_builder::PlanBuildContext;
use crate::composite_schemas::lookup_index::IndexedLookup;
use crate::composite_schemas::lookup_index::compile_alternative;
use crate::composite_schemas::lookup_index::compile_template;
use crate::error::FederationError;
use crate::link::federation_spec_definition::FEDERATION_REQUIRE_DIRECTIVE_NAME_IN_SPEC;
use crate::link::federation_spec_definition::get_federation_spec_definition_from_subgraph;
use crate::link::spec_definition::SpecDefinition;
use crate::operation::ArgumentList;
use crate::operation::Field;
use crate::operation::Operation;
use crate::operation::Selection;
use crate::operation::SelectionMap;
use crate::operation::SelectionSet;
use crate::query_graph::graph_path::operation::OpPathElement;
use crate::query_plan::FlattenNode;
use crate::query_plan::ParallelNode;
use crate::query_plan::PlanNode;
use crate::query_plan::QueryPlanCost;
use crate::query_plan::conditions::ConditionKind;
use crate::query_plan::conditions::Conditions;
use crate::query_plan::entity_lookup::EntityLookup;
use crate::query_plan::entity_lookup::LookupVariable;
use crate::query_plan::entity_lookup::TemplateAlternative;
use crate::query_plan::entity_lookup::ValueTemplate;
use crate::query_plan::fetch_dependency_graph_processor::to_valid_graphql_name;
use crate::query_plan::serializable_document::SerializableDocument;
use crate::schema::ValidFederationSchema;
use crate::schema::field_selection_map;
use crate::schema::field_selection_map::value::SelectionTree;
use crate::schema::position::CompositeTypeDefinitionPosition;
use crate::schema::position::FieldDefinitionPosition;
use crate::schema::position::SchemaRootDefinitionKind;
use crate::schema::position::TypeDefinitionPosition;

/// The canonical form of a key condition, to find the lookup recalling an entity by it.
pub(super) fn canonical_key(conditions: &SelectionSet) -> Result<String, FederationError> {
    let selection_set = executable::SelectionSet::try_from(conditions)?;
    Ok(SelectionTree::from_selection_set(&selection_set)
        .canonical()
        .to_string())
}

/// The fetches of one lookup within an entity group.
struct LookupGroup {
    lookup: Arc<IndexedLookup>,
    /// Entity types (as the destination subgraph knows them) entering through this lookup, with
    /// the canonical key each uses.
    entity_types: IndexMap<Name, String>,
    /// Incoming inputs (edge index within the node's incoming edges, input index) feeding it.
    inputs: Vec<(petgraph::stable_graph::EdgeIndex, usize)>,
}

impl FetchGraph {
    /// Materialize an entity group whose subgraph is a GraphQL Federation source schema.
    pub(super) fn lookup_node_to_plan_node(
        &self,
        ctx: &mut PlanBuildContext<'_>,
        node_idx: NodeIndex,
    ) -> Result<Option<(PlanNode, QueryPlanCost)>, FederationError> {
        let node = &self.graph[node_idx];
        let FetchGroupKind::Entity { merge_at } = &node.kind else {
            return Err(FederationError::internal(
                "lookup materialization called for a non-entity fetch group",
            ));
        };
        let subgraph_schema = ctx.query_graph.schema_by_source(&node.subgraph)?.clone();

        // 1. Group the key inputs by the lookup recalling their entity type by their key.
        let mut groups: IndexMap<String, LookupGroup> = IndexMap::default();
        let mut requires_inputs: Vec<(petgraph::stable_graph::EdgeIndex, usize, Name)> = Vec::new();
        for edge in self.graph.edges_directed(node_idx, Direction::Incoming) {
            for (input_index, input) in edge.weight().inputs.iter().enumerate() {
                match input {
                    InputContribution::Key {
                        conditions,
                        rewrite_info,
                        ..
                    } => {
                        let dest_type = rewrite_info.dest_type.type_name().clone();
                        let key = canonical_key(conditions)?;
                        let Some(lookup) = ctx.lookup_index.find(&node.subgraph, &dest_type, &key)
                        else {
                            return Err(FederationError::internal(format!(
                                "no @lookup field in subgraph \"{}\" resolves \"{dest_type}\" by \
                                 key \"{key}\"",
                                node.subgraph
                            )));
                        };
                        let group =
                            groups
                                .entry(lookup.coordinate())
                                .or_insert_with(|| LookupGroup {
                                    lookup: lookup.clone(),
                                    entity_types: IndexMap::default(),
                                    inputs: Vec::new(),
                                });
                        group.entity_types.insert(dest_type, key);
                        group.inputs.push((edge.id(), input_index));
                    }
                    InputContribution::Requires {
                        source_type_name, ..
                    } => requires_inputs.push((edge.id(), input_index, source_type_name.clone())),
                }
            }
        }
        if groups.is_empty() {
            return Err(FederationError::internal(format!(
                "entity fetch group for source schema \"{}\" has no key inputs",
                node.subgraph
            )));
        }
        // Requirement inputs ride the same edges as the key inputs of their entity type.
        for (edge, input_index, source_type) in requires_inputs {
            let mut assigned = false;
            for group in groups.values_mut() {
                let same_source = group
                    .inputs
                    .iter()
                    .any(|(e, i)| self.graph[*e].inputs[*i].source_type_name() == &source_type);
                if same_source {
                    group.inputs.push((edge, input_index));
                    assigned = true;
                }
            }
            if !assigned {
                for group in groups.values_mut() {
                    group.inputs.push((edge, input_index));
                }
            }
        }

        // 2. Assign the group's selections to lookups by their top-level type condition.
        let entries = node.selection_builder.entries();
        let mut entries_by_group: IndexMap<String, Vec<usize>> = IndexMap::default();
        for (entry_index, entry) in entries.iter().enumerate() {
            let entry_type = entry
                .path()
                .to_vec()
                .first()
                .and_then(|element| match &**element {
                    OpPathElement::InlineFragment(fragment) => fragment
                        .type_condition_position
                        .as_ref()
                        .map(|t| t.type_name().clone()),
                    OpPathElement::Field(_) => None,
                });
            let coordinate = groups
                .iter()
                .find(|(_, group)| {
                    entry_type
                        .as_ref()
                        .is_some_and(|t| group.entity_types.contains_key(t))
                })
                .or_else(|| {
                    // A cast to a type the lookup's return type covers.
                    groups.iter().find(|(_, group)| {
                        entry_type.as_ref().is_some_and(|t| {
                            field_selection_map::validate::possible_types(
                                subgraph_schema.schema(),
                                &group.lookup.lookup.return_type,
                            )
                            .contains(t)
                                || *t == group.lookup.lookup.return_type
                        })
                    })
                })
                .map(|(coordinate, _)| coordinate.clone())
                .unwrap_or_else(|| groups.keys().next().cloned().unwrap_or_default());
            entries_by_group
                .entry(coordinate)
                .or_default()
                .push(entry_index);
        }

        // 3. One fetch per lookup.
        let mut plan_nodes = Vec::new();
        let mut total_cost = 0.0;
        for (coordinate, group) in &groups {
            let Some(entry_indexes) = entries_by_group.get(coordinate) else {
                continue;
            };
            let return_type: CompositeTypeDefinitionPosition = subgraph_schema
                .get_type(&group.lookup.lookup.return_type)?
                .try_into()?;
            let mut selection_set =
                SelectionSet::empty(subgraph_schema.clone(), return_type.clone());
            for index in entry_indexes {
                let entry = &entries[*index];
                selection_set.add_at_path_for_incremental_planner(
                    &entry.path().to_vec(),
                    entry.selections(),
                )?;
            }
            if selection_set.selections.is_empty() {
                continue;
            }
            let node_cost = FETCH_COST + selection_set.cost(1.0);
            let group_conditions = selection_set.conditions()?;
            if let Conditions::Boolean(false) = group_conditions {
                continue;
            }
            // Under an object return type the entity casts are redundant and are flattened;
            // under an abstract one they select per runtime type and must stay.
            let keep_entity_casts =
                !matches!(return_type, CompositeTypeDefinitionPosition::Object(_));
            let (finalized, output_rewrites) = Self::finalize_selection(
                &selection_set,
                &group_conditions,
                keep_entity_casts,
                &return_type,
                &subgraph_schema,
                ctx.variable_definitions,
                ctx.skip_validation,
            )?;

            let (requires_selection, input_rewrites) =
                self.materialize_entity_inputs_filtered(ctx, node_idx, &return_type, |edge, i| {
                    group.inputs.contains(&(edge, i))
                })?;

            let mut variables = VariableAllocator::new(ctx.variable_definitions);
            let (finalized, require_variables) =
                add_require_arguments(ctx, &subgraph_schema, &finalized, &mut variables)?;
            let (lookup_variables, lookup_arguments) =
                lookup_argument_variables(ctx, &subgraph_schema, group, &mut variables)?;

            let (mut variable_definitions, variable_usages) =
                Self::collect_used_variable_definitions(
                    &node.context_variables,
                    ctx.variable_definitions,
                    ctx.operation_directives,
                    &finalized,
                );
            let mut lookup_template_variables = Vec::new();
            for (definition, variable) in lookup_variables.into_iter().chain(require_variables) {
                variable_definitions.push(definition);
                lookup_template_variables.push(variable);
            }

            let op_name = ctx
                .operation_name
                .as_ref()
                .map(|name| {
                    let c = ctx.operation_counter;
                    ctx.operation_counter += 1;
                    let subgraph = to_valid_graphql_name(&node.subgraph).unwrap_or("".into());
                    Name::new(&format!("{name}__{subgraph}__{c}"))
                        .map_err(|e| FederationError::internal(e.to_string()))
                })
                .transpose()?;
            let operation = operation_for_lookup_fetch(
                &subgraph_schema,
                &group.lookup,
                lookup_arguments,
                finalized,
                variable_definitions,
                ctx.operation_directives,
                &op_name,
            )?;
            let operation_document = ctx
                .operation_compression
                .compress(operation, ctx.skip_validation)?;
            let requires = if requires_selection.selections.is_empty() {
                Vec::new()
            } else {
                super::plan_builder::trim_requires(&executable::SelectionSet::try_from(
                    &requires_selection,
                )?)
            };

            let fetch_node = PlanNode::Fetch(Box::new(crate::query_plan::FetchNode {
                subgraph_name: node.subgraph.clone(),
                protocol: Default::default(),
                id: None,
                variable_usages,
                requires,
                operation_document: SerializableDocument::from_parsed(operation_document),
                operation_name: op_name,
                operation_kind: executable::OperationType::Query,
                input_rewrites: Arc::new(input_rewrites),
                output_rewrites,
                context_rewrites: node
                    .context_rewrites
                    .iter()
                    .cloned()
                    .map(|r| Arc::new(r.into()))
                    .collect(),
                entity_lookup: Some(Arc::new(EntityLookup {
                    path: group.lookup.path.iter().map(|n| n.to_string()).collect(),
                    variables: lookup_template_variables,
                })),
            }));
            let mut plan_node = PlanNode::Flatten(FlattenNode {
                path: merge_at.clone(),
                node: Box::new(fetch_node),
            });
            if let Conditions::Variables(variables) = &group_conditions {
                for (name, kind) in variables.iter() {
                    let (if_clause, else_clause) = match kind {
                        ConditionKind::Skip => (None, Some(Box::new(plan_node))),
                        ConditionKind::Include => (Some(Box::new(plan_node)), None),
                    };
                    plan_node = PlanNode::Condition(Box::new(crate::query_plan::ConditionNode {
                        condition_variable: name.clone(),
                        if_clause,
                        else_clause,
                    }));
                }
            }
            total_cost += node_cost;
            plan_nodes.push(plan_node);
        }

        Ok(match plan_nodes.len() {
            0 => None,
            1 => plan_nodes.pop().map(|n| (n, total_cost)),
            _ => Some((
                PlanNode::Parallel(ParallelNode { nodes: plan_nodes }),
                total_cost,
            )),
        })
    }
}

/// Allocates variable names that do not collide with the client operation's.
struct VariableAllocator {
    taken: IndexSet<Name>,
    next_lookup: usize,
    next_require: usize,
}

impl VariableAllocator {
    fn new(client_variables: &[Node<VariableDefinition>]) -> Self {
        Self {
            taken: client_variables.iter().map(|v| v.name.clone()).collect(),
            next_lookup: 0,
            next_require: 0,
        }
    }

    fn allocate(&mut self, prefix: &str, counter: fn(&mut Self) -> &mut usize) -> Name {
        loop {
            let index = *counter(self);
            *counter(self) += 1;
            let name = Name::new_unchecked(&format!("{prefix}_{index}"));
            if self.taken.insert(name.clone()) {
                return name;
            }
        }
    }

    fn lookup_argument(&mut self) -> Name {
        self.allocate("lookupArgument", |s| &mut s.next_lookup)
    }

    fn require_argument(&mut self) -> Name {
        self.allocate("requireArgument", |s| &mut s.next_require)
    }
}

fn variable_definition(
    name: &Name,
    ty: &Node<apollo_compiler::ast::Type>,
) -> Node<VariableDefinition> {
    Node::new(VariableDefinition {
        name: name.clone(),
        ty: ty.clone(),
        default_value: None,
        directives: Default::default(),
        description: None,
    })
}

/// The lookup's argument variables: definitions with their per-entity templates, and the
/// arguments passing them to the lookup field.
#[allow(clippy::type_complexity)]
fn lookup_argument_variables(
    ctx: &PlanBuildContext<'_>,
    subgraph_schema: &ValidFederationSchema,
    group: &LookupGroup,
    variables: &mut VariableAllocator,
) -> Result<
    (
        Vec<(Node<VariableDefinition>, LookupVariable)>,
        Vec<Node<executable::Argument>>,
    ),
    FederationError,
> {
    let lookup = &group.lookup;
    let supergraph_schema = ctx.supergraph_schema.schema();
    let mut definitions = Vec::new();
    let mut arguments = Vec::new();
    for (argument_index, argument) in lookup.lookup.arguments.iter().enumerate() {
        // Which alternative of the argument's map each entity type uses.
        let mut by_alternative: IndexMap<usize, Vec<String>> = IndexMap::default();
        for (entity_type, key) in &group.entity_types {
            let concrete_types: Vec<Name> = field_selection_map::validate::possible_types(
                subgraph_schema.schema(),
                entity_type,
            )
            .into_iter()
            .collect();
            for concrete in concrete_types {
                let alternative = lookup
                    .keys
                    .iter()
                    .find(|k| k.concrete_type == concrete && k.key.to_string() == *key)
                    .or_else(|| lookup.keys.iter().find(|k| k.concrete_type == concrete))
                    .and_then(|k| k.alternatives.get(argument_index).copied())
                    .unwrap_or(0);
                by_alternative
                    .entry(alternative)
                    .or_default()
                    .push(concrete.to_string());
            }
        }
        let template = match by_alternative.len() {
            0 | 1 => {
                let alternative = by_alternative.keys().next().copied().unwrap_or(0);
                match argument.map.alternatives.get(alternative) {
                    Some(entry) => compile_alternative(entry, supergraph_schema),
                    None => compile_template(&argument.map, supergraph_schema),
                }
            }
            _ => ValueTemplate::Alternatives(
                by_alternative
                    .into_iter()
                    .filter_map(|(alternative, types)| {
                        argument.map.alternatives.get(alternative).map(|entry| {
                            TemplateAlternative {
                                types: Some(types),
                                value: compile_alternative(entry, supergraph_schema),
                            }
                        })
                    })
                    .collect(),
            ),
        };
        let name = variables.lookup_argument();
        definitions.push((
            variable_definition(&name, &argument.ty),
            LookupVariable {
                name: name.to_string(),
                non_null: argument.ty.is_non_null(),
                value: template,
            },
        ));
        arguments.push(Node::new(executable::Argument {
            name: argument.name.clone(),
            value: Node::new(executable::Value::Variable(name)),
        }));
    }
    Ok((definitions, arguments))
}

/// Add the `@require` arguments of the selected fields, as variables computed per entity. The
/// requirement selection maps are rooted at the entity (the fields are selected directly under
/// the lookup, possibly within a type condition).
#[allow(clippy::type_complexity)]
fn add_require_arguments(
    ctx: &PlanBuildContext<'_>,
    subgraph_schema: &ValidFederationSchema,
    selection_set: &SelectionSet,
    variables: &mut VariableAllocator,
) -> Result<
    (
        SelectionSet,
        Vec<(Node<VariableDefinition>, LookupVariable)>,
    ),
    FederationError,
> {
    let Some(require_name) = get_federation_spec_definition_from_subgraph(subgraph_schema)
        .ok()
        .and_then(|spec| {
            spec.directive_name_in_schema(
                subgraph_schema,
                &FEDERATION_REQUIRE_DIRECTIVE_NAME_IN_SPEC,
            )
        })
        .filter(|name| {
            subgraph_schema
                .schema()
                .directive_definitions
                .contains_key(name)
        })
    else {
        return Ok((selection_set.clone(), Vec::new()));
    };
    let mut definitions = Vec::new();
    let rewritten = rewrite_level(
        ctx,
        subgraph_schema,
        selection_set,
        &require_name,
        variables,
        &mut definitions,
        None,
        0,
    )?;
    Ok((rewritten, definitions))
}

#[allow(clippy::too_many_arguments)]
fn rewrite_level(
    ctx: &PlanBuildContext<'_>,
    subgraph_schema: &ValidFederationSchema,
    selection_set: &SelectionSet,
    require_name: &Name,
    variables: &mut VariableAllocator,
    definitions: &mut Vec<(Node<VariableDefinition>, LookupVariable)>,
    type_condition: Option<&Name>,
    depth: usize,
) -> Result<SelectionSet, FederationError> {
    let mut map = SelectionMap::new();
    for selection in selection_set.selections.values() {
        match selection {
            Selection::Field(field_selection) => {
                let definition = field_selection
                    .field
                    .field_position
                    .get(subgraph_schema.schema())?;
                let requirements: Vec<_> = definition
                    .arguments
                    .iter()
                    .filter_map(|argument| {
                        let map = argument
                            .directives
                            .get(require_name)?
                            .specified_argument_by_name("field")?
                            .as_str()
                            .and_then(|text| field_selection_map::parse(text).ok())?;
                        Some((argument, map))
                    })
                    .collect();
                if requirements.is_empty() {
                    map.insert(selection.clone());
                    continue;
                }
                if depth > 0 {
                    return Err(FederationError::internal(format!(
                        "field \"{}\" has @require arguments but is not selected directly on the \
                         entity",
                        field_selection.field.field_position
                    )));
                }
                let mut arguments: Vec<Node<executable::Argument>> =
                    field_selection.field.arguments.to_vec();
                for (argument, requirement) in requirements {
                    let name = variables.require_argument();
                    let mut template =
                        compile_template(&requirement, ctx.supergraph_schema.schema());
                    // Under a type condition, only entities of that type select the field; others
                    // get null for this variable (declared nullable in that case).
                    let mut ty = argument.ty.clone();
                    let mut non_null = argument.ty.is_non_null();
                    if let Some(type_condition) = type_condition {
                        template = ValueTemplate::Alternatives(vec![TemplateAlternative {
                            types: Some(
                                field_selection_map::validate::possible_types(
                                    subgraph_schema.schema(),
                                    type_condition,
                                )
                                .into_iter()
                                .map(|t| t.to_string())
                                .collect(),
                            ),
                            value: template,
                        }]);
                        if non_null {
                            ty = Node::new(ty.as_ref().clone().nullable());
                            non_null = false;
                        }
                    }
                    definitions.push((
                        variable_definition(&name, &ty),
                        LookupVariable {
                            name: name.to_string(),
                            non_null,
                            value: template,
                        },
                    ));
                    arguments.push(Node::new(executable::Argument {
                        name: argument.name.clone(),
                        value: Node::new(executable::Value::Variable(name)),
                    }));
                }
                let field = Field {
                    arguments: ArgumentList::from(arguments),
                    ..field_selection.field.clone()
                };
                map.insert(Selection::from_field(
                    field,
                    field_selection.selection_set.clone(),
                ));
            }
            Selection::InlineFragment(fragment) => {
                let condition = fragment
                    .inline_fragment
                    .type_condition_position
                    .as_ref()
                    .map(|t| t.type_name().clone());
                let rewritten = rewrite_level(
                    ctx,
                    subgraph_schema,
                    &fragment.selection_set,
                    require_name,
                    variables,
                    definitions,
                    condition.as_ref().or(type_condition),
                    depth,
                )?;
                map.insert(Selection::InlineFragment(Arc::new(
                    crate::operation::InlineFragmentSelection::new(
                        fragment.inline_fragment.clone(),
                        rewritten,
                    ),
                )));
            }
        }
    }
    Ok(SelectionSet {
        schema: selection_set.schema.clone(),
        type_position: selection_set.type_position.clone(),
        selections: Arc::new(map),
    })
}

/// Build the lookup operation: the lookup's argumentless chain from the query root, then the
/// lookup field called with the argument variables, selecting `selection_set`.
fn operation_for_lookup_fetch(
    subgraph_schema: &ValidFederationSchema,
    lookup: &IndexedLookup,
    lookup_arguments: Vec<Node<executable::Argument>>,
    selection_set: SelectionSet,
    variable_definitions: Vec<Node<VariableDefinition>>,
    operation_directives: &crate::operation::DirectiveList,
    operation_name: &Option<Name>,
) -> Result<Operation, FederationError> {
    let query_type_name = subgraph_schema
        .schema()
        .root_operation(executable::OperationType::Query)
        .ok_or_else(|| FederationError::internal("source schema has no query root"))?
        .clone();

    // Walk the chain to find each segment's parent type.
    let mut parents = Vec::with_capacity(lookup.path.len());
    let mut current = query_type_name.clone();
    for (index, field_name) in lookup.path.iter().enumerate() {
        parents.push(current.clone());
        if index + 1 < lookup.path.len() {
            let field = field_selection_map::validate::field_in_schema(
                subgraph_schema.schema(),
                &current,
                field_name,
            )
            .ok_or_else(|| {
                FederationError::internal(format!(
                    "lookup chain field \"{current}.{field_name}\" not found"
                ))
            })?;
            current = field.ty.inner_named_type().clone();
        }
    }

    let field_position =
        |parent: &Name, field: &Name| -> Result<FieldDefinitionPosition, FederationError> {
            Ok(match subgraph_schema.get_type(parent)? {
                TypeDefinitionPosition::Object(object) => {
                    FieldDefinitionPosition::Object(object.field(field.clone()))
                }
                TypeDefinitionPosition::Interface(interface) => {
                    FieldDefinitionPosition::Interface(interface.field(field.clone()))
                }
                _ => {
                    return Err(FederationError::internal(format!(
                        "lookup chain parent \"{parent}\" is not an object or interface type"
                    )));
                }
            })
        };

    // Innermost first: the lookup field, then each enclosing chain field.
    let mut inner = selection_set;
    for index in (0..lookup.path.len()).rev() {
        let parent = &parents[index];
        let field = Field {
            schema: subgraph_schema.clone(),
            field_position: field_position(parent, &lookup.path[index])?,
            alias: None,
            arguments: if index + 1 == lookup.path.len() {
                ArgumentList::from(lookup_arguments.clone())
            } else {
                ArgumentList::new()
            },
            directives: Default::default(),
            sibling_typename: None,
        };
        let selection = Selection::from_element(OpPathElement::Field(field), Some(inner))?;
        let parent_type: CompositeTypeDefinitionPosition =
            subgraph_schema.get_type(parent)?.try_into()?;
        let mut map = SelectionMap::new();
        map.insert(selection);
        inner = SelectionSet {
            schema: subgraph_schema.clone(),
            type_position: parent_type,
            selections: Arc::new(map),
        };
    }

    Ok(Operation {
        schema: subgraph_schema.clone(),
        root_kind: SchemaRootDefinitionKind::Query,
        name: operation_name.clone(),
        variables: Arc::new(variable_definitions),
        directives: operation_directives.clone(),
        description: None,
        selection_set: inner,
    })
}
