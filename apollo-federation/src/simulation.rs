use std::sync::Arc;

use apollo_compiler::ExecutableDocument;
use apollo_compiler::Name;
use apollo_compiler::executable::Field;
use apollo_compiler::executable::FieldSet;
use apollo_compiler::executable::Selection;
use apollo_compiler::executable::SelectionSet;
use apollo_compiler::executable::Directive;
use apollo_compiler::schema::ExtendedType;
use apollo_compiler::schema::Value;
use apollo_compiler::validation::Valid;

use crate::error::FederationError;
use crate::Supergraph;
use crate::query_plan::{QueryPlan, TopLevelPlanNode, FetchNode, PlanNode, FetchDataPathElement};

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

#[derive(Clone)]
struct SupergraphQueryElement {
    path: FetchDataPathElement,
    parent_type: Name,
    in_subgraphs: Vec<Name>,
}

#[derive(Clone)]
struct SubgraphQueryElement {
    path: FetchDataPathElement,
    parent_type: Name,
    in_subgraph: Name,
}

struct SupergraphQueryVisitor<'a> {
    supergraph: &'a Supergraph,
    document: &'a Valid<ExecutableDocument>,
    paths: Vec<Vec<SupergraphQueryElement>>,
}

// directive @join__field(graph: join__Graph, requires: join__FieldSet, provides: join__FieldSet, type: String, external: Boolean, override: String, usedOverridden: Boolean) repeatable on FIELD_DEFINITION | INPUT_FIELD_DEFINITION
fn join_field_is_queryable(directive: &Directive) -> bool {
    let is_external = directive.argument_by_name("external").is_some_and(|arg| **arg == Value::Boolean(true));
    !is_external
}

impl SupergraphQueryVisitor<'_> {
    fn visit_field(
        &mut self,
        path: &[SupergraphQueryElement],
        parent_type: &ExtendedType,
        field: &Field,
    ) -> Result<(), FederationError> {
        let schema = self.supergraph.schema.schema();
        let definition = schema.type_field(parent_type.name(), &field.name).expect("invalid query");

        let in_subgraphs = definition.directives.get_all("join__field")
            .filter(|directive| join_field_is_queryable(directive))
            .filter_map(|directive| directive.argument_by_name("graph"))
            .filter_map(|value| value.as_enum().cloned())
            .collect::<Vec<_>>();
        let in_subgraphs = if in_subgraphs.is_empty() {
            parent_type.directives()
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
        let schema = self.supergraph.schema.schema();
        for selection in &selection_set.selections {
            match selection {
                Selection::Field(field) => {
                    self.visit_field(path, parent_type, field)?;
                },
                Selection::InlineFragment(frag) => {
                    let type_condition = match &frag.type_condition {
                        Some(name) => schema.types.get(name).expect("invalid query"),
                        None => parent_type,
                    };
                    self.visit_selection_set(path, type_condition, &frag.selection_set)?;
                },
                Selection::FragmentSpread(frag) => {
                    let frag = self.document.fragments.get(&frag.fragment_name).expect("invalid query");
                    let type_condition =  schema.types.get(frag.type_condition()).expect("invalid query");
                    self.visit_selection_set(path, type_condition, &frag.selection_set)?;
                },
            }
        }

        Ok(())
    }
}

pub fn simulate_supergraph_query(
    supergraph: &Supergraph,
    operation_document: &Valid<ExecutableDocument>,
    operation_name: Option<&str>,
) -> Result<(), FederationError> {
    let Ok(operation) = operation_document.operations.get(operation_name) else {
        return Err(FederationError::internal("operation name not found"));
    };

    let schema = supergraph.schema.schema();
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

    for path in visitor.paths {
        for (index, element) in path.into_iter().enumerate() {
            if index > 0 {
                print!(" -> ");
            }
            print!("{} ({})", element.path, element.in_subgraphs.iter().map(ToString::to_string).collect::<Vec<_>>().join(", "));
        }
        println!();
    }

    Ok(())
}

struct QueryPlanVisitor<'a> {
    supergraph: &'a Supergraph,
    paths: Vec<Vec<SubgraphQueryElement>>,
}

impl QueryPlanVisitor<'_> {
    fn visit_top_level_node(&mut self, node: &TopLevelPlanNode) -> Result<(), FederationError> {
        match node {
            TopLevelPlanNode::Fetch(fetch) => {
                self.visit_fetch_node(&[], fetch)?;
            },
            TopLevelPlanNode::Sequence(node) => {
                for node in &node.nodes {
                    self.visit_node(&[], node)?;
                }
            },
            TopLevelPlanNode::Parallel(node) => {
                for node in &node.nodes {
                    self.visit_node(&[], node)?;
                }
            },
            _ => todo!(),
        }
        Ok(())
    }

    fn visit_node(&mut self, path: &[SubgraphQueryElement], node: &PlanNode) -> Result<(), FederationError> {
        match node {
            PlanNode::Fetch(fetch) => {
                self.visit_fetch_node(path, fetch)?;
            },
            PlanNode::Sequence(node) => {
                for node in &node.nodes {
                    self.visit_node(path, node)?;
                }
            },
            PlanNode::Parallel(node) => {
                for node in &node.nodes {
                    self.visit_node(path, node)?;
                }
            },
            PlanNode::Flatten(node) => {
                // TODO(@goto-bus-stop): this needs to be matched to an existing path.
                let path = node.path
                    .iter()
                    .map(|element| SubgraphQueryElement {
                        path: element.clone(),
                        parent_type: Name::new_static_unchecked("(Unknown)"),
                        in_subgraph: Name::new_static_unchecked("(UNKNOWN)"),
                    })
                    .collect::<Vec<_>>();
                self.visit_node(&path, &node.node)?;
            }
            _ => todo!(),
        }
        Ok(())
    }

    fn visit_selection_set(
        &mut self, 
        path: &[SubgraphQueryElement],
        in_subgraph: &Name,
        parent_type: &ExtendedType,
        document: &Valid<ExecutableDocument>,
        selection_set: &SelectionSet,
    ) -> Result<(), FederationError> {
        let schema = self.supergraph.schema.schema();
        for selection in &selection_set.selections {
            match selection {
                Selection::Field(field) => {
                    let mut path = path.to_vec();
                    path.push(SubgraphQueryElement {
                        path: FetchDataPathElement::Key(field.name.clone()),
                        parent_type: parent_type.name().clone(),
                        in_subgraph: in_subgraph.clone(),
                    });

                    if !field.selection_set.is_empty() {
                        let Ok(definition) = schema.type_field(parent_type.name(), &field.name) else {
                            return Err(FederationError::internal(format!("Invalid subgraph query: field {}.{} does not exist in subgraph {}", parent_type.name(), field.name, in_subgraph)));
                        };
                        let output_type = schema
                            .types
                            .get(definition.ty.inner_named_type())
                            .expect("invalid schema");
                        self.visit_selection_set(&path, in_subgraph, output_type, document, &field.selection_set)?;
                    } else {
                        self.paths.push(path);
                    }
                },
                Selection::InlineFragment(frag) => {
                    let type_condition = match &frag.type_condition {
                        Some(name) => schema.types.get(name).expect("invalid query"),
                        None => parent_type,
                    };
                    self.visit_selection_set(path, in_subgraph, type_condition, document, &frag.selection_set)?;
                },
                Selection::FragmentSpread(frag) => {
                    let frag = document.fragments.get(&frag.fragment_name).expect("invalid query");
                    let type_condition =  schema.types.get(frag.type_condition()).expect("invalid query");
                    self.visit_selection_set(path, in_subgraph, type_condition, document, &frag.selection_set)?;
                },
            }
        }

        Ok(())
    }

    fn visit_fetch_node(&mut self, path: &[SubgraphQueryElement], node: &FetchNode) -> Result<(), FederationError> {
        // TODO(@goto-bus-stop): use real translation
        let in_subgraph = Name::new_unchecked(&node.subgraph_name.to_uppercase());
        let Ok(operation) = node.operation_document.operations.get(node.operation_name.as_deref()) else {
            return Err(FederationError::internal("operation name not found"));
        };

        let schema = self.supergraph.schema.schema();
        let Some(root_type) = schema.root_operation(operation.operation_type) else {
            return Err(FederationError::internal("root operation type not found"));
        };
        let Some(root_type) = schema.types.get(root_type) else {
            return Err(FederationError::internal("root operation type not found"));
        };

        let entities = operation
            .selection_set
            .selections
            .iter()
            .find_map(|selection| selection.as_field().filter(|field| field.name == "_entities"));
        if let Some(entities) = entities {
            for selection in &entities.selection_set.selections {
                let Selection::InlineFragment(frag) = selection else {
                    return Err(FederationError::internal("Malformed entities query"));
                };

                let type_condition = frag.type_condition.as_ref().expect("malformed entities query");
                let type_condition = schema.types.get(type_condition).expect("invalid query");
                self.visit_selection_set(path, &in_subgraph, type_condition, &node.operation_document, &frag.selection_set)?;
            }
        } else {
            self.visit_selection_set(path, &in_subgraph, root_type, &node.operation_document, &operation.selection_set)?;
        }

        Ok(())
    }
}

pub fn simulate_query_plan(supergraph: &Supergraph, plan: &QueryPlan) -> Result<(), FederationError> {
    let mut visitor = QueryPlanVisitor { supergraph ,paths:Default::default()};

    if let Some(node) = &plan.node {
        visitor.visit_top_level_node(node)?;
    }

    for path in visitor.paths {
        for (index, element) in path.into_iter().enumerate() {
            if index > 0 {
                print!(" -> ");
            }
            print!("{} ({})", element.path, element.in_subgraph);
        }
        println!();
    }

    Ok(())
}
