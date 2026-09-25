//! The planner's index of lookups: which `@lookup` field of a source-schema subgraph recalls an
//! entity type by a given key.
//!
//! Entity jumps are planned exactly as for federation subgraphs, over `KeyResolution` edges built
//! from `@key`. Composition makes every key a lookup recalls a resolvable `@key` of the extracted
//! subgraph, and no other key resolvable, so a key edge into a source schema always has a lookup
//! here. The index is consulted when the entity fetch is materialized, to call that lookup instead
//! of `_entities`.

use std::sync::Arc;

use apollo_compiler::Name;
use apollo_compiler::Schema;
use apollo_compiler::collections::IndexMap;

use super::lookups::Lookup;
use super::lookups::LookupKey;
use super::lookups::collect_lookups;
use crate::error::FederationError;
use crate::link::federation_spec_definition::FEDERATION_IS_DIRECTIVE_NAME_IN_SPEC;
use crate::link::federation_spec_definition::FEDERATION_LOOKUP_DIRECTIVE_NAME_IN_SPEC;
use crate::link::federation_spec_definition::get_federation_spec_definition_from_subgraph;
use crate::link::spec_definition::SpecDefinition;
use crate::query_plan::entity_lookup::TemplateAlternative;
use crate::query_plan::entity_lookup::ValueTemplate;
use crate::schema::ValidFederationSchema;
use crate::schema::field_selection_map::Path;
use crate::schema::field_selection_map::SelectedListValue;
use crate::schema::field_selection_map::SelectedObjectField;
use crate::schema::field_selection_map::SelectedObjectValue;
use crate::schema::field_selection_map::SelectedValue;
use crate::schema::field_selection_map::SelectedValueEntry;
use crate::schema::field_selection_map::validate::possible_types;

/// One lookup of one subgraph.
#[derive(Debug)]
pub struct IndexedLookup {
    pub(crate) subgraph: Arc<str>,
    pub(crate) lookup: Lookup,
    /// Field names from the query root to the lookup field, inclusive.
    pub(crate) path: Vec<Name>,
    /// The keys the lookup recalls entities by, per concrete type.
    pub(crate) keys: Vec<LookupKey>,
}

impl IndexedLookup {
    /// A stable identity for grouping fetches by lookup.
    pub(crate) fn coordinate(&self) -> String {
        format!(
            "{}:{}.{}",
            self.subgraph, self.lookup.parent_type, self.lookup.field
        )
    }
}

/// Lookups by subgraph, entity type and canonical key.
#[derive(Debug, Default)]
pub struct LookupIndex {
    by_key: IndexMap<(Arc<str>, Name, String), Arc<IndexedLookup>>,
    source_schemas: IndexMap<Arc<str>, ()>,
}

impl LookupIndex {
    /// Build the index from the (extracted) subgraph schemas the planner uses.
    pub fn from_subgraphs<'a>(
        subgraphs: impl IntoIterator<Item = (&'a Arc<str>, &'a ValidFederationSchema)>,
    ) -> Result<Self, FederationError> {
        let mut index = Self::default();
        for (subgraph_name, schema) in subgraphs {
            let Ok(federation_spec) = get_federation_spec_definition_from_subgraph(schema) else {
                continue;
            };
            let names = [
                FEDERATION_LOOKUP_DIRECTIVE_NAME_IN_SPEC,
                FEDERATION_IS_DIRECTIVE_NAME_IN_SPEC,
            ]
            .map(|n| {
                federation_spec
                    .directive_name_in_schema(schema, &n)
                    .filter(|n| schema.schema().directive_definitions.contains_key(n))
            });
            let [Some(lookup_name), Some(is_name)] = names else {
                continue;
            };
            index.source_schemas.insert(subgraph_name.clone(), ());
            let mut lookups: Vec<Arc<IndexedLookup>> = Vec::new();
            for lookup in collect_lookups(schema.schema(), &lookup_name, &is_name) {
                let Some(path) = lookup.path_from_root(schema.schema()) else {
                    continue;
                };
                let keys = lookup.keys(schema.schema());
                lookups.push(Arc::new(IndexedLookup {
                    subgraph: subgraph_name.clone(),
                    lookup,
                    path,
                    keys,
                }));
            }
            // Prefer lookups on the query root: fewer fields to traverse.
            lookups.sort_by_key(|l| l.path.len());
            for lookup in lookups {
                for key in &lookup.keys {
                    index
                        .by_key
                        .entry((
                            subgraph_name.clone(),
                            key.concrete_type.clone(),
                            key.key.to_string(),
                        ))
                        .or_insert_with(|| lookup.clone());
                }
                // A key on the lookup's abstract return type itself (an entity interface), when
                // every possible type is recalled by that same key.
                let concrete: Vec<&LookupKey> = lookup.keys.iter().collect();
                if let Some(first) = concrete.first()
                    && lookup.lookup.return_type != first.concrete_type
                {
                    let all_types = possible_types(schema.schema(), &lookup.lookup.return_type);
                    let common = first.key.to_string();
                    if all_types.iter().all(|t| {
                        concrete
                            .iter()
                            .any(|k| &k.concrete_type == t && k.key.to_string() == common)
                    }) {
                        index
                            .by_key
                            .entry((
                                subgraph_name.clone(),
                                lookup.lookup.return_type.clone(),
                                common,
                            ))
                            .or_insert_with(|| lookup.clone());
                    }
                }
            }
        }
        Ok(index)
    }

    /// Whether `subgraph` is a GraphQL Federation source schema (entities are resolved through
    /// lookups).
    pub fn is_source_schema(&self, subgraph: &str) -> bool {
        self.source_schemas.contains_key(subgraph)
    }

    pub fn is_empty(&self) -> bool {
        self.source_schemas.is_empty()
    }

    /// The lookup recalling `type_name` by `canonical_key` in `subgraph`.
    pub(crate) fn find(
        &self,
        subgraph: &Arc<str>,
        type_name: &Name,
        canonical_key: &str,
    ) -> Option<&Arc<IndexedLookup>> {
        self.by_key.get(&(
            subgraph.clone(),
            type_name.clone(),
            canonical_key.to_string(),
        ))
    }
}

/// Compile a selection map into a template evaluated against an entity representation, given the
/// schema used to expand type conditions to concrete types.
pub(crate) fn compile_template(value: &SelectedValue, schema: &Schema) -> ValueTemplate {
    compile_value(value, &[], schema)
}

/// Compile one top-level alternative of a map.
pub(crate) fn compile_alternative(entry: &SelectedValueEntry, schema: &Schema) -> ValueTemplate {
    compile_entry(entry, &[], schema)
}

fn path_names(prefix: &[String], path: &Path) -> Vec<String> {
    prefix
        .iter()
        .cloned()
        .chain(path.segments.iter().map(|s| s.field.to_string()))
        .collect()
}

fn entry_types(entry: &SelectedValueEntry, schema: &Schema) -> Option<Vec<String>> {
    let path = match entry {
        SelectedValueEntry::Path(path)
        | SelectedValueEntry::PathObject(path, _)
        | SelectedValueEntry::PathList(path, _) => path,
        SelectedValueEntry::Object(_) => return None,
    };
    let type_condition = path.type_condition.as_ref()?;
    Some(
        possible_types(schema, type_condition)
            .into_iter()
            .map(|t| t.to_string())
            .collect(),
    )
}

fn compile_value(value: &SelectedValue, prefix: &[String], schema: &Schema) -> ValueTemplate {
    match value.alternatives.as_slice() {
        [single] if entry_types(single, schema).is_none() => compile_entry(single, prefix, schema),
        alternatives => ValueTemplate::Alternatives(
            alternatives
                .iter()
                .map(|entry| TemplateAlternative {
                    types: entry_types(entry, schema),
                    value: compile_entry(entry, prefix, schema),
                })
                .collect(),
        ),
    }
}

fn compile_entry(entry: &SelectedValueEntry, prefix: &[String], schema: &Schema) -> ValueTemplate {
    match entry {
        SelectedValueEntry::Path(path) => ValueTemplate::Path(path_names(prefix, path)),
        SelectedValueEntry::PathObject(path, object) => {
            compile_object(object, &path_names(prefix, path), schema)
        }
        SelectedValueEntry::PathList(path, list) => ValueTemplate::List {
            path: path_names(prefix, path),
            item: Box::new(compile_list_item(list, schema)),
        },
        SelectedValueEntry::Object(object) => compile_object(object, prefix, schema),
    }
}

fn compile_object(
    object: &SelectedObjectValue,
    prefix: &[String],
    schema: &Schema,
) -> ValueTemplate {
    ValueTemplate::Object(
        object
            .fields
            .iter()
            .map(|field| match field {
                SelectedObjectField::Labeled(name, value) => {
                    (name.to_string(), compile_value(value, prefix, schema))
                }
                SelectedObjectField::Shorthand(name, _) => (
                    name.to_string(),
                    ValueTemplate::Path(
                        prefix
                            .iter()
                            .cloned()
                            .chain(std::iter::once(name.to_string()))
                            .collect(),
                    ),
                ),
            })
            .collect(),
    )
}

fn compile_list_item(list: &SelectedListValue, schema: &Schema) -> ValueTemplate {
    match list {
        SelectedListValue::Value(value) => compile_value(value, &[], schema),
        SelectedListValue::List(inner) => ValueTemplate::List {
            path: Vec::new(),
            item: Box::new(compile_list_item(inner, schema)),
        },
    }
}

#[cfg(test)]
mod tests {
    use serde_json_bytes::json;

    use super::*;
    use crate::schema::field_selection_map::parse;

    const SCHEMA: &str = r#"
        type Query { a: Int }
        interface Media { id: ID! }
        type Book implements Media { id: ID! isbn: String! }
        type Movie implements Media { id: ID! upc: String! }
    "#;

    #[test]
    fn compiles_and_evaluates_maps() {
        let schema = Schema::parse(SCHEMA, "s.graphql").unwrap();
        let cases = [
            ("id", json!({"id": "1"}), json!("1")),
            (
                "dimension.{ w: weight, size }",
                json!({"dimension": {"weight": 1, "size": 2}}),
                json!({"w": 1, "size": 2}),
            ),
            (
                "parts[{ id }]",
                json!({"parts": [{"id": "a"}, {"id": "b"}]}),
                json!([{"id": "a"}, {"id": "b"}]),
            ),
            (
                "{ isbn: <Book>.isbn } | { upc: <Movie>.upc }",
                json!({"__typename": "Movie", "upc": "u"}),
                json!({"upc": "u"}),
            ),
            (
                "{ nested: { a: <Book>.isbn } | { b: <Movie>.upc } }",
                json!({"__typename": "Book", "isbn": "i"}),
                json!({"nested": {"a": "i"}}),
            ),
        ];
        for (map, representation, expected) in cases {
            let template = compile_template(&parse(map).unwrap(), &schema);
            assert_eq!(template.evaluate(&representation), expected, "{map}");
        }
    }
}
