//! Executing entity fetches through GraphQL Federation `@lookup` fields.
//!
//! The representations of the entities to fetch are built exactly as for an `_entities` fetch
//! (see [`super::fetch::Variables`]). A lookup fetch then runs its operation once per entity, with
//! variables computed from that entity's representation by the plan's templates
//! ([`EntityLookup`]), and places each result at the entity's paths.
//!
//! Results are always correlated by entity index, never by arrival order: batched transports
//! (graphql-over-http variable batching) may answer out of order.

use apollo_federation::query_plan::entity_lookup::EntityLookup;
use serde_json_bytes::Value;

use super::fetch::FetchNode;
use super::rewrites;
use crate::error::Error;
use crate::graphql;
use crate::json_ext::Object;
use crate::json_ext::Path;
use crate::json_ext::PathElement;
use crate::json_ext::ValueExt;
use crate::spec::Schema;

const REPRESENTATIONS: &str = "representations";

/// The per-entity variable sets of a lookup fetch.
pub(crate) struct LookupVariableSets {
    /// One variable set per entity that can be looked up (client variables plus the computed
    /// lookup and requirement variables).
    pub(crate) variable_sets: Vec<Object>,
    /// For each variable set, the index of its entity (into the fetch's inverted paths).
    pub(crate) entity_indexes: Vec<usize>,
}

/// Compute the variable sets from the variables an `_entities` fetch would have sent (client
/// variables plus a `representations` list).
pub(crate) fn lookup_variable_sets(
    entity_lookup: &EntityLookup,
    mut variables: Object,
) -> LookupVariableSets {
    let representations = match variables.remove(REPRESENTATIONS) {
        Some(Value::Array(representations)) => representations,
        _ => Vec::new(),
    };
    let mut variable_sets = Vec::with_capacity(representations.len());
    let mut entity_indexes = Vec::with_capacity(representations.len());
    for (index, representation) in representations.iter().enumerate() {
        // An entity whose non-null lookup arguments cannot be computed cannot be looked up; it
        // resolves to nothing, as it would if the subgraph returned null for it.
        let Some(entity_variables) = entity_lookup.variables_for(representation) else {
            continue;
        };
        let mut variable_set = variables.clone();
        variable_set.extend(entity_variables);
        variable_sets.push(variable_set);
        entity_indexes.push(index);
    }
    LookupVariableSets {
        variable_sets,
        entity_indexes,
    }
}

fn path_starts_with_keys(path: &Path, keys: &[String]) -> bool {
    path.0.len() >= keys.len()
        && path
            .0
            .iter()
            .zip(keys)
            .all(|(element, key)| matches!(element, PathElement::Key(k, _) if k == key))
}

/// Accumulates the results of a lookup fetch's per-entity responses into the value to merge at
/// the fetch's `current_dir`, like `FetchNode::response_at_path` does for `_entities`.
pub(crate) struct LookupResults<'a> {
    fetch_node: &'a FetchNode,
    entity_lookup: &'a EntityLookup,
    schema: &'a Schema,
    inverted_paths: &'a [Vec<Path>],
    error_dir: Path,
    value: Value,
    errors: Vec<Error>,
}

impl<'a> LookupResults<'a> {
    pub(crate) fn new(
        fetch_node: &'a FetchNode,
        entity_lookup: &'a EntityLookup,
        schema: &'a Schema,
        inverted_paths: &'a [Vec<Path>],
        current_dir: &Path,
        hoist_orphan_errors: bool,
    ) -> Self {
        let error_dir = if hoist_orphan_errors {
            match current_dir
                .0
                .iter()
                .position(|e| matches!(e, PathElement::Flatten(_)))
            {
                Some(i) => Path(current_dir.0[..i].to_vec()),
                None => current_dir.clone(),
            }
        } else {
            current_dir.clone()
        };
        Self {
            fetch_node,
            entity_lookup,
            schema,
            inverted_paths,
            error_dir,
            value: Value::default(),
            errors: Vec::new(),
        }
    }

    /// Record the response for the entity at `entity_index`.
    pub(crate) fn add_response(&mut self, entity_index: usize, response: graphql::Response) {
        let entity_paths = self
            .inverted_paths
            .get(entity_index)
            .map(Vec::as_slice)
            .unwrap_or_default();
        for mut error in response.errors {
            // Locations refer to the subgraph operation, not to the client's.
            error.locations = Vec::new();
            match &error.path {
                // An error under the lookup field belongs to the entity.
                Some(path) if path_starts_with_keys(path, &self.entity_lookup.path) => {
                    let rest = &path.0[self.entity_lookup.path.len()..];
                    for entity_path in entity_paths {
                        let mut entity_error = error.clone();
                        entity_error.path =
                            Some(Path::from_iter(entity_path.0.iter().chain(rest).cloned()));
                        self.errors.push(entity_error);
                    }
                }
                _ => {
                    // Not attributable to a field of the entity: attach it to the entity itself.
                    if entity_paths.is_empty() {
                        error.path = Some(self.error_dir.clone());
                        self.errors.push(error);
                    } else {
                        for entity_path in entity_paths {
                            let mut entity_error = error.clone();
                            entity_error.path = Some(entity_path.clone());
                            self.errors.push(entity_error);
                        }
                    }
                }
            }
        }
        let Some(data) = response.data else {
            return;
        };
        let mut entity = self
            .entity_lookup
            .result_in(&data)
            .cloned()
            .unwrap_or(Value::Null);
        rewrites::apply_rewrites(self.schema, &mut entity, &self.fetch_node.output_rewrites);
        if let Some((last, rest)) = entity_paths.split_last() {
            for path in rest {
                let _ = self.value.insert(path, entity.clone());
            }
            let _ = self.value.insert(last, entity);
        }
    }

    /// Record a failure to fetch the entity at `entity_index` (transport or service error).
    pub(crate) fn add_error(&mut self, entity_index: usize, error: Error) {
        match self.inverted_paths.get(entity_index) {
            Some(paths) if !paths.is_empty() => {
                for path in paths {
                    let mut entity_error = error.clone();
                    entity_error.path = Some(path.clone());
                    self.errors.push(entity_error);
                }
            }
            _ => {
                let mut error = error;
                error.path = Some(self.error_dir.clone());
                self.errors.push(error);
            }
        }
    }

    pub(crate) fn finish(self) -> (Value, Vec<Error>) {
        (self.value, self.errors)
    }
}

#[cfg(test)]
mod tests {
    use apollo_federation::query_plan::entity_lookup::LookupVariable;
    use apollo_federation::query_plan::entity_lookup::ValueTemplate;
    use serde_json_bytes::json;

    use super::*;

    fn lookup() -> EntityLookup {
        EntityLookup {
            path: vec!["lookups".to_string(), "productById".to_string()],
            variables: vec![LookupVariable {
                name: "lookupArgument_0".to_string(),
                non_null: true,
                value: ValueTemplate::Path(vec!["id".to_string()]),
            }],
        }
    }

    #[test]
    fn computes_one_variable_set_per_entity() {
        let variables = json!({
            "locale": "en",
            "representations": [
                { "__typename": "Product", "id": "1" },
                { "__typename": "Product", "id": null },
                { "__typename": "Product", "id": "3" },
            ],
        });
        let Value::Object(variables) = variables else {
            unreachable!()
        };
        let sets = lookup_variable_sets(&lookup(), variables);
        assert_eq!(sets.entity_indexes, [0, 2]);
        assert_eq!(
            Value::Object(sets.variable_sets[0].clone()),
            json!({ "locale": "en", "lookupArgument_0": "1" })
        );
        assert_eq!(
            Value::Object(sets.variable_sets[1].clone()),
            json!({ "locale": "en", "lookupArgument_0": "3" })
        );
    }
}
