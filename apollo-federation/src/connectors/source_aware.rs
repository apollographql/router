//! Source-aware query planning — Phase 0, Spike A.
//!
//! The source-aware planner needs to know, for each connector, which of its
//! variable inputs must be satisfied by *parent data* in the query graph (and
//! therefore become **edge conditions**), versus those the client operation or
//! the runtime environment already provide. Today that information is only
//! expressed implicitly, by fabricating synthetic `@key` directives during
//! connector *expansion* (`Connector::resolvable_key` +
//! `expand::process_outputs`). This module derives the same information
//! *directly* from `Connector::variable_references()`, with no expanded schema
//! in the picture — the seam the source-aware planner will consume instead of
//! walking synthetic subgraphs.
//!
//! Two pieces:
//!
//! 1. [`classify`] partitions a connector's variable references into
//!    [`InputClass::OperationSatisfiable`] (`$args`),
//!    [`InputClass::ParentData`] (`$this` / `$batch`), and
//!    [`InputClass::Environment`] (everything else).
//! 2. [`derive_condition`] turns the parent-data partition into the
//!    planner-facing condition `FieldSet`, reproducing the two fabricated cases
//!    expansion handles today (the `__typename` singleton key for an implicit
//!    resolver with no `$this` inputs, and sibling-field key dependencies —
//!    which are representable here because the condition is validated against
//!    the *original* schema, which already has those fields).
//!
//! The `tests` module differentially checks [`derive_condition`] against the
//! existing [`Connector::resolvable_key`] over every connector in the expand
//! fixtures, so this derivation stays pinned to expansion's behavior.

// Spike A seam: the source-aware planner (Phase 1) is the consumer of these
// items and does not exist yet, so they are exercised only by the tests below.
#![allow(dead_code)]

use apollo_compiler::Name;
use apollo_compiler::Schema;
use apollo_compiler::executable::FieldSet;
use apollo_compiler::validation::Valid;

use crate::connectors::Connector;
use crate::connectors::EntityResolver;
use crate::connectors::Namespace;
use crate::connectors::json_selection::SelectionTrie;
use crate::connectors::variable::VariableReference;

/// How the source-aware planner must satisfy a connector variable input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum InputClass {
    /// Provided by the client operation itself, via field arguments (`$args`).
    /// The planner does not model these as edge conditions — they ride along on
    /// the operation.
    OperationSatisfiable,

    /// Requires data from the parent/sibling nodes in the query graph
    /// (`$this`, `$batch`). These become the **edge conditions** guarding the
    /// edge that enters the connector — the source-aware analogue of an
    /// `@key`/`@requires` condition.
    ParentData,

    /// Supplied by the runtime environment and never visible to the planner as
    /// a condition (`$config`, `$context`, `$status`, `$request`, `$response`,
    /// `$env`).
    Environment,
}

/// Classify a single variable [`Namespace`].
pub(crate) fn classify_namespace(namespace: Namespace) -> InputClass {
    match namespace {
        Namespace::Args => InputClass::OperationSatisfiable,
        Namespace::This | Namespace::Batch => InputClass::ParentData,
        Namespace::Config
        | Namespace::Context
        | Namespace::Status
        | Namespace::Request
        | Namespace::Response
        | Namespace::Env => InputClass::Environment,
    }
}

/// A connector's variable references, partitioned by [`InputClass`].
#[derive(Debug, Default)]
pub(crate) struct ConnectorInputClassification {
    pub(crate) operation_satisfiable: Vec<VariableReference<Namespace>>,
    pub(crate) parent_data: Vec<VariableReference<Namespace>>,
    pub(crate) environment: Vec<VariableReference<Namespace>>,
}

/// Partition every variable reference of `connector` (transport + response
/// selection, exactly what expansion's key derivation reads) by [`InputClass`].
///
/// Uses the deep [`SelectionTrie`] carried on each reference — not the
/// top-level-only `request_variable_keys` / `response_variable_keys` maps,
/// which are insufficient for deriving nested key conditions.
pub(crate) fn classify(connector: &Connector) -> ConnectorInputClassification {
    let mut classification = ConnectorInputClassification::default();
    for reference in connector.variable_references() {
        match classify_namespace(reference.namespace.namespace) {
            InputClass::OperationSatisfiable => {
                classification.operation_satisfiable.push(reference)
            }
            InputClass::ParentData => classification.parent_data.push(reference),
            InputClass::Environment => classification.environment.push(reference),
        }
    }
    classification
}

/// Derive the planner-facing condition `FieldSet` for the edge entering
/// `connector` — the data the parent must supply before the connector can run.
///
/// This mirrors [`Connector::resolvable_key`] (same namespace + key type per
/// [`EntityResolver`] variant, same trie merge), then folds in the fallbacks
/// that expansion's `process_outputs` applies on top of a `None` key:
///
/// * An [`EntityResolver::Implicit`] resolver with no `$this` inputs yields the
///   `__typename` **singleton** condition (the "namespace container" pattern);
///   expansion fabricates `@key(fields: "__typename")` for exactly this case.
/// * Any other resolver with no key contributes no condition (`Ok(None)`). The
///   interface-object key copying expansion does in this case is an
///   output-schema concern, not an input condition.
///
/// Returns `Ok(None)` for a connector that is not an entity resolver at all.
pub(crate) fn derive_condition(
    connector: &Connector,
    schema: &Schema,
) -> Result<Option<Valid<FieldSet>>, String> {
    let Some(resolver) = &connector.entity_resolver else {
        return Ok(None);
    };

    // Namespace whose references carry the key, and the type the key is rooted
    // on — identical to the mapping in `Connector::resolvable_key`.
    let (namespace, key_type_name) = match resolver {
        EntityResolver::Explicit => (Namespace::Args, connector.id.directive.base_type_name(schema)),
        EntityResolver::Implicit => (Namespace::This, connector.id.directive.parent_type_name()),
        EntityResolver::TypeSingle => (Namespace::This, connector.id.directive.base_type_name(schema)),
        EntityResolver::TypeBatch => (Namespace::Batch, connector.id.directive.base_type_name(schema)),
    };
    let key_type_name = key_type_name.ok_or_else(|| {
        format!(
            "missing key type for connector {}",
            connector.id.directive.coordinate()
        )
    })?;

    match merge_condition(connector, schema, &key_type_name, namespace)? {
        Some(field_set) => Ok(Some(field_set)),
        // Fabricated fallback: implicit resolver, no `$this` → __typename singleton.
        None if matches!(resolver, EntityResolver::Implicit) => {
            Ok(Some(parse_field_set(schema, &key_type_name, "__typename")?))
        }
        None => Ok(None),
    }
}

/// Merge the [`SelectionTrie`]s of every reference in `namespace` into one
/// `FieldSet` rooted on `key_type_name`. Returns `Ok(None)` when the connector
/// has no reference in that namespace (mirrors `make_key_field_set_from_variables`).
fn merge_condition(
    connector: &Connector,
    schema: &Schema,
    key_type_name: &Name,
    namespace: Namespace,
) -> Result<Option<Valid<FieldSet>>, String> {
    let mut merged = SelectionTrie::new();
    let mut matched = false;
    for reference in connector.variable_references() {
        if reference.namespace.namespace == namespace {
            merged.extend(&reference.selection);
            matched = true;
        }
    }

    if !matched {
        return Ok(None);
    }

    parse_field_set(schema, key_type_name, &merged.to_string()).map(Some)
}

fn parse_field_set(
    schema: &Schema,
    type_name: &Name,
    fields: &str,
) -> Result<Valid<FieldSet>, String> {
    FieldSet::parse_and_validate(
        Valid::assume_valid_ref(schema),
        type_name.clone(),
        fields,
        "",
    )
    .map_err(|errors| format!("failed to parse condition field set `{fields}`: {errors}"))
}

#[cfg(test)]
mod tests {
    use apollo_compiler::Schema;
    use insta::glob;

    use super::InputClass;
    use super::classify;
    use super::classify_namespace;
    use super::derive_condition;
    use crate::connectors::Connector;
    use crate::connectors::EntityResolver;
    use crate::connectors::Namespace;
    use crate::schema::FederationSchema;
    use crate::supergraph::extract_subgraphs_from_supergraph;

    fn serialize(field_set: &apollo_compiler::validation::Valid<apollo_compiler::executable::FieldSet>) -> String {
        field_set.serialize().no_indent().to_string()
    }

    #[test]
    fn namespace_classification_matches_the_plan() {
        assert_eq!(
            classify_namespace(Namespace::Args),
            InputClass::OperationSatisfiable
        );
        for parent in [Namespace::This, Namespace::Batch] {
            assert_eq!(classify_namespace(parent), InputClass::ParentData);
        }
        for env in [
            Namespace::Config,
            Namespace::Context,
            Namespace::Status,
            Namespace::Request,
            Namespace::Response,
            Namespace::Env,
        ] {
            assert_eq!(classify_namespace(env), InputClass::Environment);
        }
    }

    /// The heart of Spike A: over every connector in every expand fixture, the
    /// condition derived directly from `variable_references()` must reproduce
    /// what expansion fabricates via `resolvable_key` + `process_outputs`:
    ///
    /// * `resolvable_key == Some(fs)` → `derive_condition == Some(fs)`.
    /// * `resolvable_key == None`, implicit resolver → derived `__typename`
    ///   singleton (the fabricated "namespace container" key).
    /// * `resolvable_key == None`, otherwise → no condition.
    #[test]
    fn derived_condition_matches_resolvable_key_over_expand_fixtures() {
        let mut connectors_checked = 0usize;
        let mut parent_data_conditions = 0usize;
        let mut singleton_conditions = 0usize;

        insta::with_settings!({prepend_module_to_snapshot => false}, {
            glob!("expand/tests/schemas/expand", "*.graphql", |path| {
                let sdl = std::fs::read_to_string(path).unwrap();
                let supergraph_schema =
                    FederationSchema::new(Schema::parse(&sdl, "supergraph.graphql").unwrap())
                        .unwrap();
                let Ok(subgraphs) =
                    extract_subgraphs_from_supergraph(&supergraph_schema, Some(true))
                else {
                    return;
                };

                for (_, subgraph) in subgraphs.subgraphs.iter() {
                    let schema = subgraph.schema.schema();
                    let Ok(connectors) = Connector::from_schema(schema, &subgraph.name) else {
                        continue;
                    };

                    for connector in &connectors {
                        // `resolvable_key` returning `Err` is a fixture problem,
                        // not a derivation mismatch — skip those connectors.
                        let Ok(expected) = connector.resolvable_key(schema) else {
                            continue;
                        };
                        let derived = derive_condition(connector, schema).unwrap_or_else(|e| {
                            panic!(
                                "derive_condition failed for {} in {path:?}: {e}",
                                connector.id.coordinate()
                            )
                        });

                        connectors_checked += 1;

                        match (expected, &connector.entity_resolver) {
                            (Some(expected_key), _) => {
                                let derived = derived.unwrap_or_else(|| {
                                    panic!(
                                        "expansion produced a key for {} in {path:?} but derivation produced none",
                                        connector.id.coordinate()
                                    )
                                });
                                assert_eq!(
                                    serialize(&derived),
                                    serialize(&expected_key),
                                    "condition mismatch for {} in {path:?}",
                                    connector.id.coordinate()
                                );
                                parent_data_conditions += 1;
                            }
                            (None, Some(EntityResolver::Implicit)) => {
                                let derived = derived.unwrap_or_else(|| {
                                    panic!(
                                        "implicit connector {} in {path:?} should derive a __typename singleton",
                                        connector.id.coordinate()
                                    )
                                });
                                assert_eq!(
                                    serialize(&derived),
                                    "__typename",
                                    "expected __typename singleton for {} in {path:?}",
                                    connector.id.coordinate()
                                );
                                singleton_conditions += 1;
                            }
                            (None, _) => {
                                assert!(
                                    derived.is_none(),
                                    "expected no condition for {} in {path:?}, got {:?}",
                                    connector.id.coordinate(),
                                    derived.map(|d| serialize(&d))
                                );
                            }
                        }
                    }
                }
            });
        });

        // Guard against the glob silently matching nothing / the fixtures
        // losing their entity connectors.
        assert!(
            connectors_checked > 0,
            "no connectors were checked — did the fixture glob match?"
        );
        assert!(
            parent_data_conditions + singleton_conditions > 0,
            "no entity conditions exercised — the differential test is vacuous"
        );
    }

    #[test]
    fn classification_partitions_every_reference() {
        // Sanity: classification is total — no reference is dropped.
        insta::with_settings!({prepend_module_to_snapshot => false}, {
            glob!("expand/tests/schemas/expand", "*.graphql", |path| {
                let sdl = std::fs::read_to_string(path).unwrap();
                let supergraph_schema =
                    FederationSchema::new(Schema::parse(&sdl, "supergraph.graphql").unwrap())
                        .unwrap();
                let Ok(subgraphs) =
                    extract_subgraphs_from_supergraph(&supergraph_schema, Some(true))
                else {
                    return;
                };
                for (_, subgraph) in subgraphs.subgraphs.iter() {
                    let Ok(connectors) =
                        Connector::from_schema(subgraph.schema.schema(), &subgraph.name)
                    else {
                        continue;
                    };
                    for connector in &connectors {
                        let total = connector.variable_references().count();
                        let classification = classify(connector);
                        let partitioned = classification.operation_satisfiable.len()
                            + classification.parent_data.len()
                            + classification.environment.len();
                        assert_eq!(
                            total, partitioned,
                            "classification dropped references for {}",
                            connector.id.coordinate()
                        );
                    }
                }
            });
        });
    }
}
