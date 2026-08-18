mod conditions_validation;
mod satisfiability_error;
mod validation_context;
mod validation_state;
mod validation_traversal;

use std::sync::Arc;

use tracing::instrument;
use tracing::trace;

use crate::api_schema;
use crate::composition::CompositionFailure;
use crate::composition::CompositionOptions;
use crate::composition::satisfiability::validation_traversal::ValidationTraversal;
use crate::connectors::Connector;
use crate::connectors::expand::Connectors;
use crate::connectors::expand::ExpansionResult;
use crate::connectors::expand::expand_connectors;
use crate::error::CompositionError;
use crate::error::FederationError;
use crate::query_graph::QueryGraph;
use crate::query_graph::build_federated_query_graph;
use crate::query_graph::build_supergraph_api_query_graph;
use crate::schema::ValidFederationSchema;
use crate::supergraph::CompositionHint;
use crate::supergraph::Merged;
use crate::supergraph::Satisfiable;
use crate::supergraph::Supergraph;

/// Validates that all the queries expressible on the supergraph's API schema can be executed
/// against the subgraphs it was composed from.
///
/// Connectors, if the supergraph uses any, are first expanded into their own synthetic subgraphs,
/// which is the only shape satisfiability can reason about them in. That expansion is an
/// implementation detail of the check: the supergraph returned here is always the merged one that
/// was passed in, and messages naming a synthetic subgraph are rewritten to name the real one.
// PORT_NOTE: The connectors handling mirrors the tail of
// `HybridComposition::experimental_compose` in the apollo-composition crate.
#[instrument(skip(supergraph, options))]
pub fn validate_satisfiability(
    supergraph: Supergraph<Merged>,
    options: &CompositionOptions,
) -> Result<Supergraph<Satisfiable>, CompositionFailure> {
    let merged_schema = supergraph.schema().clone();
    // Hints carried over from merging. Taken from the merged supergraph rather than from whatever
    // we end up checking, because the expanded copy is parsed from SDL and so has none of them.
    let mut hints = supergraph.hints().to_vec();

    let expansion = match expand_connectors(
        &merged_schema.schema().to_string(),
        &Default::default(),
    ) {
        Ok(expansion) => expansion,
        Err(e) => {
            return Err(CompositionFailure {
                errors: vec![CompositionError::InternalError {
                    message: format!(
                        "Composition failed due to an internal error when expanding connectors, please report this: {e}"
                    ),
                }],
                hints,
            });
        }
    };
    let (to_check, connectors) = match expansion {
        ExpansionResult::Expanded {
            raw_sdl,
            connectors,
            ..
        } => match Supergraph::parse(&raw_sdl) {
            Ok(expanded) => (expanded, Some(connectors)),
            Err(e) => {
                return Err(CompositionFailure {
                    errors: vec![CompositionError::InternalError {
                        message: e.to_string(),
                    }],
                    hints,
                });
            }
        },
        ExpansionResult::Unchanged => (supergraph, None),
    };

    let mut errors = vec![];
    let result = validate_satisfiability_inner(to_check, options, &mut errors, &mut hints);

    if let Some(Connectors {
        by_service_name, ..
    }) = &connectors
    {
        for error in errors.iter_mut() {
            sanitize_connectors_error(error, by_service_name.iter());
        }
        for hint in hints.iter_mut() {
            sanitize_connectors_message(&mut hint.message, by_service_name.iter());
        }
    }

    if let Err(e) = result {
        return Err(CompositionFailure {
            errors: vec![CompositionError::InternalError {
                message: e.to_string(),
            }],
            hints,
        });
    }
    if !errors.is_empty() {
        return Err(CompositionFailure { errors, hints });
    }
    Ok(Supergraph::<Satisfiable>::new(merged_schema, hints))
}

fn validate_satisfiability_inner(
    supergraph: Supergraph<Merged>,
    options: &CompositionOptions,
    errors: &mut Vec<CompositionError>,
    hints: &mut Vec<CompositionHint>,
) -> Result<(), FederationError> {
    let supergraph_schema = supergraph.schema();
    let api_schema = api_schema::to_api_schema(supergraph_schema.clone(), Default::default())?;

    trace!("Building API query graph");
    let api_schema_query_graph =
        build_supergraph_api_query_graph(supergraph_schema.clone(), api_schema.clone())?;
    trace!("Building federated query graph");
    let federated_query_graph = build_federated_query_graph(
        supergraph_schema.clone(),
        api_schema.clone(),
        Some(true),
        Some(false),
    )?;
    trace!("Validating graph composition");
    validate_graph_composition(
        supergraph_schema.clone(),
        Arc::new(api_schema_query_graph),
        Arc::new(federated_query_graph),
        options,
        errors,
        hints,
    )?;
    Ok(())
}

/// Validates that all the queries expressible on the API schema resulting from the composition of
/// a set of subgraphs can be executed on those subgraphs.
fn validate_graph_composition(
    // The supergraph schema generated by composition of the subgraph schemas.
    supergraph_schema: ValidFederationSchema,
    // The query graph of the API schema generated by the supergraph schema.
    api_schema_query_graph: Arc<QueryGraph>,
    // The federated query graph corresponding to the composed subgraphs.
    federated_query_graph: Arc<QueryGraph>,
    composition_options: &CompositionOptions,
    errors: &mut Vec<CompositionError>,
    hints: &mut Vec<CompositionHint>,
) -> Result<(), FederationError> {
    ValidationTraversal::new(
        supergraph_schema,
        api_schema_query_graph,
        federated_query_graph,
        composition_options,
    )?
    .validate(errors, hints)
}

fn sanitize_connectors_error<'a>(
    issue: &mut CompositionError,
    connector_subgraphs: impl Iterator<Item = (&'a Arc<str>, &'a Connector)>,
) {
    match issue {
        CompositionError::SatisfiabilityError { message } => {
            sanitize_connectors_message(message, connector_subgraphs);
        }
        CompositionError::ShareableHasMismatchedRuntimeTypes { message } => {
            sanitize_connectors_message(message, connector_subgraphs);
        }
        _ => {}
    }
}

fn sanitize_connectors_message<'a>(
    message: &mut String,
    connector_subgraphs: impl Iterator<Item = (&'a Arc<str>, &'a Connector)>,
) {
    for (service_name, connector) in connector_subgraphs {
        *message = message.replace(&**service_name, connector.id.subgraph_name.as_str());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEST_SUPERGRAPH: &str = r#"
schema
  @link(url: "https://specs.apollo.dev/link/v1.0")
  @link(url: "https://specs.apollo.dev/join/v0.5", for: EXECUTION)
  @link(url: "https://specs.apollo.dev/context/v0.1", for: SECURITY)
{
  query: Query
}

directive @context(name: String!) repeatable on INTERFACE | OBJECT | UNION

directive @context__fromContext(field: context__ContextFieldValue) on ARGUMENT_DEFINITION

directive @join__directive(graphs: [join__Graph!], name: String!, args: join__DirectiveArguments) repeatable on SCHEMA | OBJECT | INTERFACE | FIELD_DEFINITION

directive @join__enumValue(graph: join__Graph!) repeatable on ENUM_VALUE

directive @join__field(graph: join__Graph, requires: join__FieldSet, provides: join__FieldSet, type: String, external: Boolean, override: String, usedOverridden: Boolean, overrideLabel: String, contextArguments: [join__ContextArgument!]) repeatable on FIELD_DEFINITION | INPUT_FIELD_DEFINITION

directive @join__graph(name: String!, url: String!) on ENUM_VALUE

directive @join__implements(graph: join__Graph!, interface: String!) repeatable on OBJECT | INTERFACE

directive @join__type(graph: join__Graph!, key: join__FieldSet, extension: Boolean! = false, resolvable: Boolean! = true, isInterfaceObject: Boolean! = false) repeatable on OBJECT | INTERFACE | UNION | ENUM | INPUT_OBJECT | SCALAR

directive @join__unionMember(graph: join__Graph!, member: String!) repeatable on UNION

directive @link(url: String, as: String, for: link__Purpose, import: [link__Import]) repeatable on SCHEMA

scalar context__ContextFieldValue

interface I
  @join__type(graph: A, key: "id")
  @join__type(graph: B, key: "id")
  @context(name: "A__contextI")
{
  id: ID!
  value: Int! @join__field(graph: A)
}

input join__ContextArgument {
  name: String!
  type: String!
  context: String!
  selection: join__FieldValue!
}

scalar join__DirectiveArguments

scalar join__FieldSet

scalar join__FieldValue

enum join__Graph {
  A @join__graph(name: "A", url: "http://A")
  B @join__graph(name: "B", url: "http://B")
}

scalar link__Import

enum link__Purpose {
  """
  `SECURITY` features provide metadata necessary to securely resolve fields.
  """
  SECURITY

  """
  `EXECUTION` features provide metadata necessary for operation execution.
  """
  EXECUTION
}

type P
  @join__type(graph: A, key: "id")
  @join__type(graph: B, key: "id")
{
  id: ID!
  data: String! @join__field(graph: A, contextArguments: [{context: "A__contextI", name: "onlyInA", type: "Int", selection: " { value }"}])
}

type Query
  @join__type(graph: A)
  @join__type(graph: B)
{
  start: I! @join__field(graph: B)
}

type T implements I
  @join__implements(graph: A, interface: "I")
  @join__implements(graph: B, interface: "I")
  @join__type(graph: A, key: "id")
  @join__type(graph: B, key: "id")
{
  id: ID!
  value: Int! @join__field(graph: A)
  onlyInA: Int! @join__field(graph: A)
  p: P! @join__field(graph: A)
  sharedField: Int!
  onlyInB: Int! @join__field(graph: B)
}
    "#;

    #[test]
    fn test_satisfiability_basic() {
        let supergraph = Supergraph::parse(TEST_SUPERGRAPH).unwrap();
        _ = validate_satisfiability(supergraph, &CompositionOptions::default())
            .expect("Supergraph should be satisfiable");
    }
}
