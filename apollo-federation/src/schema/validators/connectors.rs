use apollo_compiler::ast::Value;

use crate::connectors::ConnectSpec;
use crate::error::CompositionError;
use crate::error::ConnectorsCode;
use crate::error::SubgraphLocation;
use crate::link::Link;
use crate::schema::position::HasAppliedDirectives;
use crate::subgraph::typestate::HasMetadata;
use crate::subgraph::typestate::Subgraph;

/// Rejects `@override(from:)` pointing at a connector-enabled subgraph.
///
/// Overrides mess with the supergraph in ways that can be difficult to detect when expanding
/// connectors; the supergraph may omit overridden fields and other shenanigans. To allow for a
/// better developer experience, we check here if any connector-enabled subgraphs have fields
/// overridden.
///
/// # Ordering
///
/// This runs after merging so that connectors-related override errors are only reported once
/// merging succeeded, and before connector expansion, which these overrides would otherwise
/// corrupt. It looks at the subgraphs rather than the supergraph precisely because merging may
/// have dropped the overridden fields.
// PORT_NOTE: Corresponds to `validate_overrides` in the apollo-composition crate. That version
// worked on the unexpanded subgraph schemas and so had to match the `@override` directive by name
// against a hardcoded list, missing any subgraph that imported it under an alias. Here the name is
// resolved through the federation spec, so aliases are handled.
pub(crate) fn validate_override_on_connector<T: HasMetadata>(
    subgraphs: &[Subgraph<T>],
) -> Result<(), Vec<CompositionError>> {
    let connector_subgraphs: Vec<&str> = subgraphs
        .iter()
        .filter(|subgraph| has_connectors(subgraph))
        .map(|subgraph| subgraph.name.as_str())
        .collect();
    if connector_subgraphs.is_empty() {
        return Ok(());
    }

    let mut errors = vec![];
    for subgraph in subgraphs {
        let Some(override_directive_name) = subgraph.override_directive_name() else {
            continue;
        };
        let schema = subgraph.schema();
        // `@override` is only valid on object/interface fields, so those are the only referencers
        // worth looking at.
        let overridden_fields = schema
            .referencers()
            .get_directive(&override_directive_name)
            .object_or_interface_fields();

        for field in overridden_fields {
            for directive in field.get_applied_directives(schema, &override_directive_name) {
                // A missing or non-string `from` is not this validation's problem: the merger
                // reports it, and there is nothing to check against here.
                let Ok(Value::String(from)) = directive
                    .argument_by_name("from", schema.schema())
                    .map(|arg| arg.as_ref())
                else {
                    continue;
                };
                if !connector_subgraphs.contains(&from.as_str()) {
                    continue;
                }

                errors.push(CompositionError::ConnectorsValidationError {
                    code: ConnectorsCode::OverrideOnConnector,
                    message: format!(
                        r#"Field "{type_name}.{field_name}" on subgraph "{subgraph_name}" is trying to override connector-enabled subgraph "{from}", which is not yet supported. See https://go.apollo.dev/connectors/limitations#override-is-partially-unsupported"#,
                        type_name = field.type_name(),
                        field_name = field.field_name(),
                        subgraph_name = subgraph.name,
                    ),
                    locations: schema
                        .node_locations(directive)
                        .map(|range| SubgraphLocation {
                            // The `@override` application lives in this subgraph, not in the one
                            // it points at.
                            subgraph: subgraph.name.clone(),
                            range,
                        })
                        .collect(),
                });
            }
        }
    }

    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}

/// Whether the subgraph `@link`s the connect spec. Link expansion keeps the `@link` around, so this
/// is answerable at any point after the subgraph has been expanded.
fn has_connectors<T: HasMetadata>(subgraph: &Subgraph<T>) -> bool {
    Link::for_identity(subgraph.schema().schema(), &ConnectSpec::identity()).is_some()
}
