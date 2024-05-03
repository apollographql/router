use std::{
    fmt::{self, Display},
    str::FromStr,
};

use apollo_compiler::{
    ast::{FieldDefinition, Name},
    execution::GraphQLError,
    NodeStr,
};
use indexmap::IndexMap;
use itertools::Itertools;
use lazy_static::lazy_static;

use crate::{
    composition::satisfiability::dependencies::print_human_readable_list,
    query_graph::graph_path::Unadvanceables,
};

use super::{
    dependencies::print_subgraph_names, state::ValidationState, witness::build_witness_operation,
};

// PORT_NOTE: this is assigned to the `originalError` field of the GraphQLError
// which is not a concept in apollo-compiler. Can we just ignore it?
pub(super) struct ValidationError {
    message: String,
    supergraph_unsatisfiable_path: Todo, // RootPath<Transition>
    subgraph_paths: Todo,                // RootPath<Transition>[]
    witness: Todo,                       // Operation
}

type Todo = usize;

pub(super) fn satisfiability_error(
    unsatisfiable_path: Todo,                      // RootPath<Transition>
    _subgraph_paths: Todo,                         // RootPath<Transition>[]
    subgraph_paths_unadvanceables: Unadvanceables, // Unadvanceables[]
) -> GraphQLError {
    fn display_reasons(_reasons: Unadvanceables) -> String {
        todo!() // can port directly
    }

    let _witness = build_witness_operation(unsatisfiable_path);
    let operation = "{\n  TODO\n}"; // print(operationToDocument(witness))
    let reasons = display_reasons(subgraph_paths_unadvanceables);

    let message = format!(
        r#"The following supergraph API query:
{operation}
cannot be satisfied by the subgraphs because:
{reasons}"#
    );

    // PORT_NOTE: apollo-compiler doesn't have a notion of `originalError`
    // let original_error = ValidationError {};

    SATISFIABILITY_ERROR.to_error(&message)
}

pub(super) fn shareable_field_non_intersecting_runtime_types_error(
    invalid_state: &ValidationState,
    // PORT_NOTE: we need the parent type of the field, so this should be a different type
    field: FieldDefinition,
    runtime_types_to_subgraphs: IndexMap<Name, Vec<NodeStr>>,
) -> GraphQLError {
    let _witness = build_witness_operation(invalid_state.supergraph_path);
    let operation = "{\n  TODO\n}"; // print(operationToDocument(witness))
    let type_strings = runtime_types_to_subgraphs
        .iter()
        .map(|(ts, subgraphs)| {
            let subgraph_names = print_subgraph_names(subgraphs);
            format!(" - in {subgraph_names}, ${ts}")
        })
        .join(";\n");

    let field_coordinate = "TODO"; // field.coordinate
    let field_type = field.ty.inner_named_type();

    let message = format!(
        r###"For the following supergraph API query:
${operation}
Shared field "{field_coordinate}" return type "{field_type}" has a non-intersecting set of possible runtime types across subgraphs. Runtime types in subgraphs are:`
{type_strings}.`
This is not allowed as shared fields must resolve the same way in all subgraphs, and that imply at least some common runtime types between the subgraphs."###
    );

    // PORT_NOTE: apollo-compiler's GraphQLError doesn't have a notion of `nodes`
    let _nodes = subgraph_nodes(invalid_state, |_schema| todo!());

    // PORT_NOTE: apollo-compiler doesn't have a notion of `originalError`
    // let original_error = ValidationError {};

    SHAREABLE_HAS_MISMATCHED_RUNTIME_TYPES.to_error(&message)
}

pub(super) fn shareable_field_mismatched_runtime_types_hint(
    state: &ValidationState,
    // PORT_NOTE: we need the parent type of the field, so this should be a different type
    field: FieldDefinition,
    common_runtime_types: Vec<Name>,
    _runtime_types_per_subgraphs: IndexMap<Name, Vec<NodeStr>>,
) -> CompositionHint {
    let _witness = build_witness_operation(state.supergraph_path);
    let operation = "{\n  TODO\n}"; // print(operationToDocument(witness))

    let field_coordinate = "TODO"; // field.coordinate
    let field_type = field.ty.inner_named_type();

    let all_subgraph_names = state.current_subgraph_names();
    let subgraph_names = print_subgraph_names(&all_subgraph_names.iter().cloned().collect_vec());
    let common_runtime_types = print_human_readable_list(
        common_runtime_types.iter().map(|n| n.to_string()).collect(),
        None,
        Some("type".into()),
        Some("types".into()),
        None,
        None,
    );

    let subgraphs_with_type_not_in_intersection = all_subgraph_names
        .iter()
        .filter_map(|_| Some("TODO"))
        .collect_vec();
    let subgraphs_with_type_not_in_intersection_string =
        subgraphs_with_type_not_in_intersection.join(";\n");

    let message = format!(
        r#"For the following supergraph API query:
{operation}
Shared field "{field_coordinate}" return type "{field_type}" has different sets of possible runtime types across subgraphs.
Since a shared field must be resolved the same way in all subgraphs, make sure that {subgraph_names} only resolve "{field_coordinate}" to objects of {common_runtime_types}. In particular:
{subgraphs_with_type_not_in_intersection_string}.
Otherwise the @shareable contract will be broken."#
    );

    // PORT_NOTE: apollo-compiler's GraphQLError doesn't have a notion of `nodes`,
    // do we need it for hints?
    let _nodes = subgraph_nodes(state, |_schema| todo!());

    CompositionHint {
        definition: &INCONSISTENT_RUNTIME_TYPES_FOR_SHAREABLE_RETURN,
        message,
        element: None,
        nodes: None,
    }
}

fn subgraph_nodes(
    _state: &ValidationState,
    _extract_node: fn(_schema: Todo /* Schema */) -> Option<Todo>, /* ASTNode | undefined */
) -> Todo /* SubgraphASTNode[] */ {
    todo!()
}

// --- Errors and hints --------------------------------------------------------

#[cfg_attr(test, derive(Debug))]
struct CodeDefinition {
    code: &'static str,
    description: &'static str,
    added_in: &'static str,
}

impl CodeDefinition {
    fn to_error(
        &self,
        message: &str,
        // options: GraphQLErrorOptions (nodes, source, positions, path, originalError, extensions)
    ) -> GraphQLError {
        GraphQLError {
            message: message.to_string(),
            locations: Default::default(),
            path: Default::default(),
            extensions: {
                let mut extensions = serde_json_bytes::Map::new();
                extensions.insert(
                    "code".to_string(),
                    serde_json_bytes::Value::from_str(self.code).unwrap(),
                );
                extensions
            },
        }
    }
}

#[cfg_attr(test, derive(Debug))]
enum HintLevel {
    Warn,
    Info,
    Debug,
}

#[cfg_attr(test, derive(Debug))]
struct HintCodeDefinition {
    code: &'static str,
    level: HintLevel,
    description: &'static str,
}

#[cfg_attr(test, derive(Debug))]
pub(crate) struct CompositionHint {
    definition: &'static HintCodeDefinition,
    message: String,
    element: Option<Todo>,    // NamedSchemaElement<any, any, any> | undefined
    nodes: Option<Vec<Todo>>, // SubgraphASTNode[] | SubgraphASTNode
}

impl Display for CompositionHint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "[{}]: {}", self.definition.code, self.message)
    }
}

lazy_static! {
    // ERRORS
    static ref SATISFIABILITY_ERROR: CodeDefinition = CodeDefinition {
        code: "SATISFIABILITY_ERROR",
        description: "Subgraphs can be merged, but the resulting supergraph API would have queries that cannot be satisfied by those subgraphs.",
        added_in: "2.0.0",
    };

    static ref SHAREABLE_HAS_MISMATCHED_RUNTIME_TYPES: CodeDefinition = CodeDefinition {
        code: "SHAREABLE_HAS_MISMATCHED_RUNTIME_TYPES",
        description: "A shareable field return type has mismatched possible runtime types in the subgraphs in which the field is declared. As shared fields must resolve the same way in all subgraphs, this is almost surely a mistake.",
        added_in: "2.0.0",
    };

    // HINTS
    static ref INCONSISTENT_RUNTIME_TYPES_FOR_SHAREABLE_RETURN: HintCodeDefinition = HintCodeDefinition {
        code: "INCONSISTENT_RUNTIME_TYPES_FOR_SHAREABLE_RETURN",
        level: HintLevel::Warn,
        description: "Indicates that a @shareable field returns different sets of runtime types in the different subgraphs in which it is defined."
    };
}
