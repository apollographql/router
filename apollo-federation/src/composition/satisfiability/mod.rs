/*
- [ ] ValidationError (maybe unnecessary?)
- [ ] satisfiabilityError
    - [ ] displayReasons
    - dependencies:
        - [ ] operationToDocument
        - [ ] Operation
- [ ] subgraphNodes (maybe unnecessary?)
    - dependencies:
        - [ ] addSubgraphToASTNode
        - [ ] operationToDocument
        - [ ] Operation
- [ ] shareableFieldNonIntersectingRuntimeTypesError
- [ ] shareableFieldMismatchedRuntimeTypesHint
    - dependencies:
        - [ ] printHumanReadableList
        - [ ] printSubgraphNames
        - [ ] operationToDocument
        - [ ] Operation
- [ ] buildWitnessOperation
- [ ] buildWitnessNextStep
- [ ] buildWitnessField
- [ ] generateWitnessValue
- [x] validateGraphComposition
- [x] computeSubgraphPaths (unused)
- [ ] initialSubgraphPaths
    - dependencies:
        - [x] SchemaRootKind
        - [ ] federatedGraphRootTypeName
        - [ ] GraphPath.fromGraphRoot
- [ ] possibleRuntimeTypeNamesSorted
- [x] extractValidationError (unused)
- [ ] ValidationContext
    - [x] constructor
        - dependencies:
            - [x] validateSupergraph (metadata)
            - [x] joinSpec.typeDirective
            - [x] joinSpec.fieldDirective
    - [ ] isShareable
- [ ] ValidationState
    - [ ] initial
        - dependencies:
            - [ ] TransitionPathWithLazyIndirectPaths.initial
            - [ ] ConditionResolver
    - [ ] validateTransition
        - dependencies:
            - [ ] Edge
    - [ ] currentSubgraphNames
    - [ ] currentSubgraphs
    - [x] toString
- [x] isSupersetOrEqual
- [x] VertexVisit
- [ ] ValidationTraversal
    - [ ] constructor
    - [ ] validate
    - [ ] handleState
        - dependencies:
            - [ ] simpleValidationConditionResolver

- [x] QueryGraph
- [ ] RootPath<Transition> (replaced with GraphPath?)
- [ ] GraphPath
    - [ ] .fromGraphRoot
    - [ ] .tailPossibleRuntimeTypes
- [ ] TransitionPathWithLazyIndirectPaths
    - dependencies:
        - [x] IndirectPaths
        - [x] advancePathWithNonCollectingAndTypePreservingTransitions
- [ ] ConditionResolver
- [ ] Subgraph
- [ ] Schema (is this FederatedSchema?)
*/

use std::sync::Arc;

use apollo_compiler::{
    ast::{DirectiveDefinition, FieldDefinition},
    execution::GraphQLError,
    Node,
};

use crate::{
    composition::satisfiability::traversal::ValidationTraversal,
    link::{
        join_spec_definition::{
            JOIN_FIELD_DIRECTIVE_NAME_IN_SPEC, JOIN_TYPE_DIRECTIVE_NAME_IN_SPEC,
        },
        spec::Identity,
    },
    query_graph::QueryGraph,
    schema::ValidFederationSchema,
};

use self::diagnostics::CompositionHint;

mod dependencies;
mod diagnostics;
mod state;
mod traversal;
mod witness;

type Todo = usize;
static _TODO: Todo = 0;

pub(crate) fn validate_graph_composition(
    supergraph_schema: Arc<ValidFederationSchema>, // Schema
    supergraph_api: Arc<QueryGraph>,
    federated_query_graph: Arc<QueryGraph>,
) -> Result<Vec<CompositionHint>, (Vec<GraphQLError>, Vec<CompositionHint>)> {
    ValidationTraversal::new(supergraph_schema, supergraph_api, federated_query_graph).validate()
}

struct ValidationContext {
    supergraph_schema: Arc<ValidFederationSchema>,
    join_type_directive: Node<DirectiveDefinition>,
    join_field_directive: Node<DirectiveDefinition>,
}

impl ValidationContext {
    fn new(supergraph_schema: Arc<ValidFederationSchema>) -> Self {
        let Some(metadata) = supergraph_schema.metadata() else {
            panic!("Metadata not found in supergraph schema");
        };

        let Some(join_spec) = metadata.for_identity(&Identity::join_identity()) else {
            panic!("Join spec not found in supergraph schema");
        };

        let join_type_name = join_spec.directive_name_in_schema(&JOIN_TYPE_DIRECTIVE_NAME_IN_SPEC);
        let join_field_name =
            join_spec.directive_name_in_schema(&JOIN_FIELD_DIRECTIVE_NAME_IN_SPEC);

        let join_type_pos = supergraph_schema
            .get_directive_definition(&join_type_name)
            .expect("Join type directive not found in supergraph schema");
        let join_field_pos = supergraph_schema
            .get_directive_definition(&join_field_name)
            .expect("Join field directive not found in supergraph schema");

        let join_type_directive = join_type_pos
            .get(supergraph_schema.schema())
            .unwrap()
            .clone();
        let join_field_directive = join_field_pos
            .get(supergraph_schema.schema())
            .unwrap()
            .clone();

        Self {
            supergraph_schema,
            join_type_directive,
            join_field_directive,
        }
    }

    /// A field is shareable if either:
    ///     1) there is not join__field, but multiple join__type
    ///     2) there is more than one join__field where the field is neither external nor overriden.
    // PORT_NOTE: we need the field parent type, so this should be a different type
    fn is_shareable(&self, _field: FieldDefinition) -> bool {
        todo!()
    }
}

#[cfg(test)]
mod tests;
