use apollo_compiler::executable::Directive;
use apollo_compiler::executable::Field;
use apollo_compiler::executable::Selection;
use apollo_compiler::executable::SelectionSet;
use apollo_compiler::name;
use apollo_compiler::schema::ExtendedType;
use apollo_compiler::schema::Value;
use apollo_compiler::validation::Valid;
use apollo_compiler::ExecutableDocument;
use apollo_compiler::Name;

use crate::error::FederationError;
use crate::query_plan::FetchDataPathElement;
use crate::query_plan::FetchNode;
use crate::query_plan::PlanNode;
use crate::query_plan::QueryPlan;
use crate::query_plan::TopLevelPlanNode;
use crate::schema::ValidFederationSchema;

// Questions that we may want to answer for correctness testing.
// - Does a query plan produce the same response shape as the supergraph query?
// - Does a query plan execute the same fields as the supergraph query?

// Things to consider:
// - Fields executed through @requires and @key etc, should be recorded but not end up in the
//   response shape
// - Subgraph jumps: I think it's enough to execute a field in only one subgraph.
// - Tolerance for extra fields being executed
// - Execution order (only in mutations)
// - Differences due to abstract types: executing a field on an interface is the same as executing
//   the field in inline fragments on all of its implementations.
//   This could be identifiable just in response shape: if an abstract type isn't 100% covered,
//   the response shape would record that a field may be absent.
//   Could also expand each interface selection to all its concrete types. Note some subgraphs may
//   not have all concrete types.

#[derive(Debug, Clone)]
pub struct SupergraphQueryElement {
    path: FetchDataPathElement,
    parent_type: Name,
    in_subgraphs: Vec<Name>,
}

#[derive(Debug, Clone)]
pub struct SubgraphQueryElement {
    path: FetchDataPathElement,
    parent_type: Name,
    in_subgraph: Name,
}

struct SupergraphQueryVisitor<'a> {
    supergraph: &'a ValidFederationSchema,
    document: &'a Valid<ExecutableDocument>,
    paths: Vec<Vec<SupergraphQueryElement>>,
}

// directive @join__field(graph: join__Graph, requires: join__FieldSet, provides: join__FieldSet, type: String, external: Boolean, override: String, usedOverridden: Boolean) repeatable on FIELD_DEFINITION | INPUT_FIELD_DEFINITION
fn join_field_is_queryable(directive: &Directive) -> bool {
    let is_external = directive
        .argument_by_name("external")
        .is_some_and(|arg| **arg == Value::Boolean(true));
    !is_external
}

impl SupergraphQueryVisitor<'_> {
    fn visit_field(
        &mut self,
        path: &[SupergraphQueryElement],
        parent_type: &ExtendedType,
        field: &Field,
    ) -> Result<(), FederationError> {
        let schema = self.supergraph.schema();
        let definition = schema
            .type_field(parent_type.name(), &field.name)
            .expect("invalid query");

        let in_subgraphs = definition
            .directives
            .get_all("join__field")
            .filter(|directive| join_field_is_queryable(directive))
            .filter_map(|directive| directive.argument_by_name("graph"))
            .filter_map(|value| value.as_enum().cloned())
            .collect::<Vec<_>>();
        let in_subgraphs = if in_subgraphs.is_empty() {
            parent_type
                .directives()
                .get_all("join__type")
                .filter_map(|directive| directive.argument_by_name("graph"))
                .filter_map(|value| value.as_enum().cloned())
                .collect::<Vec<_>>()
        } else {
            in_subgraphs
        };
        let in_subgraphs = if in_subgraphs.is_empty() {
            let graphs = schema.get_enum("join__Graph").expect("invalid supergraph");
            graphs.values.keys().cloned().collect()
        } else {
            in_subgraphs
        };

        // If we have different requires conditions in different graphs, does this need branching?
        /*
        let requires = definition.directives
            .get_all("join__field")
            .filter(|directive| join_field_is_queryable(directive))
            .find_map(|directive| directive.argument_by_name("requires"))
            .and_then(|requires| requires.as_str());

        if let Some(requires) = requires {
            let requires = FieldSet::parse(schema, parent_type.name().clone(), requires, "").unwrap();
            self.visit_selection_set(path, parent_type, &requires.selection_set)?;
        }
        */

        let mut path = path.to_vec();
        path.push(SupergraphQueryElement {
            path: FetchDataPathElement::Key(field.response_key().clone()),
            parent_type: parent_type.name().clone(),
            in_subgraphs,
        });

        if !field.selection_set.is_empty() {
            let output_type = schema
                .types
                .get(definition.ty.inner_named_type())
                .expect("invalid schema");
            self.visit_selection_set(&path, output_type, &field.selection_set)?;
        } else {
            self.paths.push(path);
        }

        Ok(())
    }

    fn visit_selection_set(
        &mut self,
        path: &[SupergraphQueryElement],
        parent_type: &ExtendedType,
        selection_set: &SelectionSet,
    ) -> Result<(), FederationError> {
        let schema = self.supergraph.schema();
        for selection in &selection_set.selections {
            match selection {
                Selection::Field(field) => {
                    self.visit_field(path, parent_type, field)?;
                }
                Selection::InlineFragment(frag) => {
                    let type_condition = match &frag.type_condition {
                        Some(name) => schema.types.get(name).expect("invalid query"),
                        None => parent_type,
                    };
                    self.visit_selection_set(path, type_condition, &frag.selection_set)?;
                }
                Selection::FragmentSpread(frag) => {
                    let frag = self
                        .document
                        .fragments
                        .get(&frag.fragment_name)
                        .expect("invalid query");
                    let type_condition = schema
                        .types
                        .get(frag.type_condition())
                        .expect("invalid query");
                    self.visit_selection_set(path, type_condition, &frag.selection_set)?;
                }
            }
        }

        Ok(())
    }
}

/// Returns the paths executed by a supergraph query.
pub fn simulate_supergraph_query(
    supergraph: &ValidFederationSchema,
    operation_document: &Valid<ExecutableDocument>,
    operation_name: Option<&str>,
) -> Result<Vec<Vec<SupergraphQueryElement>>, FederationError> {
    let Ok(operation) = operation_document.operations.get(operation_name) else {
        return Err(FederationError::internal("operation name not found"));
    };

    let schema = supergraph.schema();
    let Some(root_type) = schema.root_operation(operation.operation_type) else {
        return Err(FederationError::internal("root operation type not found"));
    };
    let Some(root_type) = schema.types.get(root_type) else {
        return Err(FederationError::internal("root operation type not found"));
    };

    let mut visitor = SupergraphQueryVisitor {
        supergraph,
        document: operation_document,
        paths: Default::default(),
    };

    visitor.visit_selection_set(&[], root_type, &operation.selection_set)?;

    for path in &visitor.paths {
        for (index, element) in path.iter().enumerate() {
            if index > 0 {
                print!(" -> ");
            }
            print!(
                "{} ({})",
                element.path,
                element
                    .in_subgraphs
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }
        println!();
    }

    Ok(visitor.paths)
}

struct QueryPlanVisitor<'a> {
    supergraph: &'a ValidFederationSchema,
    paths: Vec<Vec<SubgraphQueryElement>>,
}

impl QueryPlanVisitor<'_> {
    fn visit_top_level_node(&mut self, node: &TopLevelPlanNode) -> Result<(), FederationError> {
        match node {
            TopLevelPlanNode::Fetch(fetch) => {
                self.visit_fetch_node(&[], fetch)?;
            }
            TopLevelPlanNode::Sequence(node) => {
                for node in &node.nodes {
                    self.visit_node(&[], node)?;
                }
            }
            TopLevelPlanNode::Parallel(node) => {
                for node in &node.nodes {
                    self.visit_node(&[], node)?;
                }
            }
            _ => todo!(),
        }
        Ok(())
    }

    fn visit_node(
        &mut self,
        path: &[SubgraphQueryElement],
        node: &PlanNode,
    ) -> Result<(), FederationError> {
        match node {
            PlanNode::Fetch(fetch) => {
                self.visit_fetch_node(path, fetch)?;
            }
            PlanNode::Sequence(node) => {
                for node in &node.nodes {
                    self.visit_node(path, node)?;
                }
            }
            PlanNode::Parallel(node) => {
                for node in &node.nodes {
                    self.visit_node(path, node)?;
                }
            }
            PlanNode::Flatten(node) => {
                let path = self.map_response_path(&node.path);
                self.visit_node(&path, &node.node)?;
            }
            _ => todo!(),
        }
        Ok(())
    }

    fn map_response_path(&self, path: &[FetchDataPathElement]) -> Vec<SubgraphQueryElement> {
        self.paths
            .iter()
            .filter(|existing_path| existing_path.len() >= path.len())
            .find_map(|existing_path| {
                let matches = path
                    .iter()
                    .filter(|element| matches!(element, FetchDataPathElement::Key(_)))
                    .enumerate()
                    .all(|(index, element)| {
                        let existing_element = &existing_path[index];
                        existing_element.path == *element
                    });
                matches.then(|| {
                    let len = path
                        .iter()
                        .filter(|element| matches!(element, FetchDataPathElement::Key(_)))
                        .count();
                    &existing_path[0..len]
                })
            })
            .unwrap()
            .to_vec()
    }

    fn visit_selection_set(
        &mut self,
        path: &[SubgraphQueryElement],
        in_subgraph: &Name,
        parent_type: &ExtendedType,
        document: &Valid<ExecutableDocument>,
        selection_set: &SelectionSet,
    ) -> Result<(), FederationError> {
        let schema = self.supergraph.schema();
        for selection in &selection_set.selections {
            match selection {
                Selection::Field(field) => {
                    let mut path = path.to_vec();
                    path.push(SubgraphQueryElement {
                        path: FetchDataPathElement::Key(field.response_key().clone()),
                        parent_type: parent_type.name().clone(),
                        in_subgraph: in_subgraph.clone(),
                    });

                    if !field.selection_set.is_empty() {
                        let Ok(definition) = schema.type_field(parent_type.name(), &field.name)
                        else {
                            return Err(FederationError::internal(format!(
                                "Invalid subgraph query: field {}.{} does not exist in subgraph {}",
                                parent_type.name(),
                                field.name,
                                in_subgraph
                            )));
                        };
                        let output_type = schema
                            .types
                            .get(definition.ty.inner_named_type())
                            .expect("invalid schema");
                        self.visit_selection_set(
                            &path,
                            in_subgraph,
                            output_type,
                            document,
                            &field.selection_set,
                        )?;
                    } else {
                        self.paths.push(path);
                    }
                }
                Selection::InlineFragment(frag) => {
                    let type_condition = match &frag.type_condition {
                        Some(name) => schema.types.get(name).expect("invalid query"),
                        None => parent_type,
                    };
                    self.visit_selection_set(
                        path,
                        in_subgraph,
                        type_condition,
                        document,
                        &frag.selection_set,
                    )?;
                }
                Selection::FragmentSpread(frag) => {
                    let frag = document
                        .fragments
                        .get(&frag.fragment_name)
                        .expect("invalid query");
                    let type_condition = schema
                        .types
                        .get(frag.type_condition())
                        .expect("invalid query");
                    self.visit_selection_set(
                        path,
                        in_subgraph,
                        type_condition,
                        document,
                        &frag.selection_set,
                    )?;
                }
            }
        }

        Ok(())
    }

    fn visit_fetch_node(
        &mut self,
        path: &[SubgraphQueryElement],
        node: &FetchNode,
    ) -> Result<(), FederationError> {
        // TODO(@goto-bus-stop): use real translation
        let in_subgraph = Name::new_unchecked(&node.subgraph_name.to_uppercase().replace("-", "_"));
        let Ok(operation) = node
            .operation_document
            .operations
            .get(node.operation_name.as_deref())
        else {
            return Err(FederationError::internal("operation name not found"));
        };

        let schema = self.supergraph.schema();
        let Some(root_type) = schema.root_operation(operation.operation_type) else {
            return Err(FederationError::internal("root operation type not found"));
        };
        let Some(root_type) = schema.types.get(root_type) else {
            return Err(FederationError::internal("root operation type not found"));
        };

        // For entity queries, we want to mount the selection for each entity type onto `path`,
        // so we can in essence skip into the `_entities` field. For example if a fetch node is
        // flattened at `.user`, and it selects `{ _entities { ... on User { username } } }`,
        // our generated path should be `.user.username`.
        let entities = operation
            .selection_set
            .selections
            .iter()
            .find_map(|selection| {
                selection
                    .as_field()
                    .filter(|field| field.name == "_entities")
            });
        if let Some(entities) = entities {
            for selection in &entities.selection_set.selections {
                let Selection::InlineFragment(frag) = selection else {
                    return Err(FederationError::internal("Malformed entities query"));
                };

                let type_condition = frag
                    .type_condition
                    .as_ref()
                    .expect("malformed entities query");
                let type_condition = schema.types.get(type_condition).expect("invalid query");
                self.visit_selection_set(
                    path,
                    &in_subgraph,
                    type_condition,
                    &node.operation_document,
                    &frag.selection_set,
                )?;
            }
        } else {
            self.visit_selection_set(
                path,
                &in_subgraph,
                root_type,
                &node.operation_document,
                &operation.selection_set,
            )?;
        }

        Ok(())
    }
}

/// Determine the paths executed by a query plan.
pub fn simulate_query_plan(
    supergraph: &ValidFederationSchema,
    plan: &QueryPlan,
) -> Result<Vec<Vec<SubgraphQueryElement>>, FederationError> {
    let mut visitor = QueryPlanVisitor {
        supergraph,
        paths: Default::default(),
    };

    if let Some(node) = &plan.node {
        visitor.visit_top_level_node(node)?;
    }

    for path in &visitor.paths {
        for (index, element) in path.iter().enumerate() {
            if index > 0 {
                print!(" -> ");
            }
            print!("{} ({})", element.path, element.in_subgraph);
        }
        println!();
    }

    Ok(visitor.paths)
}

/// Compare that the supergraph execution paths are correctly fulfilled by the query plan execution
/// paths.
pub fn compare_paths(
    supergraph_paths: &[Vec<SupergraphQueryElement>],
    plan_paths: &[Vec<SubgraphQueryElement>],
) -> Result<(), FederationError> {
    for supergraph_path in supergraph_paths {
        // If the path is just resolving __typename at the root of the operation, it's not
        // passed on to subgraph fetches, but resolved in the router. So there will not be a
        // path for it.
        let is_root_typename = matches!(
            &supergraph_path[..],
            [single_item] if single_item.path == FetchDataPathElement::Key(name!(__typename)),
        );
        if is_root_typename {
            continue;
        }

        let Some(plan_path) = plan_paths.iter().find(|path| {
            supergraph_path
                .iter()
                .zip(path.iter())
                .all(|(supergraph_element, plan_element)| {
                    supergraph_element.path == plan_element.path
                })
        }) else {
            return Err(FederationError::internal(format!(
                "missing plan path for {supergraph_path:?}"
            )));
        };

        if !supergraph_path.iter().zip(plan_path.iter()).all(
            |(supergraph_element, plan_element)| {
                supergraph_element
                    .in_subgraphs
                    .contains(&plan_element.in_subgraph)
            },
        ) {
            return Err(FederationError::internal("plan path takes impossible path"));
        }
    }

    Ok(())
}
