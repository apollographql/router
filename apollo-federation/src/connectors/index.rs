use std::collections::HashMap;
use std::collections::HashSet;
use std::sync::Arc;

use apollo_compiler::Name;
use shape::Shape;
use shape::ShapeCase;

use super::id::ConnectorPosition;
use super::json_selection::SelectionAnalysis;
use super::models::Connector;
use super::models::EntityResolver;
use crate::operation::SelectionSet;
use crate::schema::ValidFederationSchema;
use crate::schema::field_set::parse_field_set;

/// Lookup table from (type, field) to connectors that can resolve it.
///
/// Built once from the subgraph schemas at planner construction time.
/// Used by the incremental planner to route fields to connectors without
/// expanding them into virtual subgraphs.
///
/// Beyond dispatch identity, the index precomputes the two things expansion
/// used to encode structurally (see the source-aware design notes ported from
/// the `benjamn/source-aware-query-planner` branch):
///
/// * **Field availability** — the static output [`Shape`] of each connector's
///   `selection`, so routing/commit can tell which fields a connector
///   actually returns instead of trusting the (collapsed) subgraph schema.
/// * **Entity keys** — the parent-data condition guarding entry into each
///   entity-resolver connector, derived from its variable references
///   (`$args`/`$this`/`$batch`) exactly as expansion fabricated `@key`s.
#[derive(Debug, Clone, Default)]
pub struct ConnectorIndex {
    /// (parent_type_name, field_name) -> connectors that resolve this field.
    by_field: HashMap<(Name, Name), Vec<Arc<Connector>>>,
    /// entity type name -> connectors that can resolve that entity. Keyed by
    /// the type the resolver *produces* (`base_type_name` for `Explicit` and
    /// type-level resolvers, the parent type for `Implicit`), so a lookup by
    /// entity type finds `Query.user(id:) @connect(entity: true)` under
    /// `User`, not under `Query`.
    entity_resolvers: HashMap<Name, Vec<Arc<Connector>>>,
    /// connector coordinate -> static output shape of its `selection`.
    output_shapes: HashMap<String, Shape>,
    /// connector coordinate -> parent-data key condition for entering the
    /// connector, parsed against its subgraph schema. Present only for
    /// entity-resolver connectors with a resolvable key.
    key_conditions: HashMap<String, Arc<SelectionSet>>,
    /// Subgraphs that contain at least one connector. There is no GraphQL
    /// endpoint behind these subgraph names: their fields are reachable only
    /// through connectors, so subgraph-edge routes into them are invalid.
    connector_subgraphs: HashSet<Arc<str>>,
}

#[allow(dead_code)]
impl ConnectorIndex {
    /// Build an index from each subgraph's schema and parsed connectors.
    /// Subgraphs without connectors contribute nothing and may be skipped.
    pub fn from_subgraphs<'a>(
        subgraphs: impl IntoIterator<Item = (&'a ValidFederationSchema, Vec<Connector>)>,
    ) -> Self {
        let mut index = Self::default();

        for (schema, connectors) in subgraphs {
            for connector in connectors {
                let connector = Arc::new(connector);
                let coordinate = connector.id.coordinate();

                index
                    .connector_subgraphs
                    .insert(Arc::from(connector.id.subgraph_name.as_str()));

                index.output_shapes.insert(
                    coordinate.clone(),
                    SelectionAnalysis::new(connector.selection.clone()).output_shape(),
                );

                // The parent-data condition guarding entry into an
                // entity-resolver connector — what expansion fabricated as a
                // synthetic @key. resolvable_key derives it from the
                // connector's variable references per resolver kind
                // ($args / $this / $batch).
                if connector.entity_resolver.is_some()
                    && let Ok(Some(field_set)) = connector.resolvable_key(schema.schema())
                {
                    let key_type = field_set.selection_set.ty.clone();
                    let fields = field_set.selection_set.serialize().no_indent().to_string();
                    if let Ok(selection_set) = parse_field_set(schema, key_type, &fields, true) {
                        index
                            .key_conditions
                            .insert(coordinate.clone(), Arc::new(selection_set));
                    }
                }

                // Entity resolvers, keyed by the entity type they resolve.
                let entity_type = match (&connector.id.directive, &connector.entity_resolver) {
                    // Type-level connectors are always entity resolvers.
                    (ConnectorPosition::Type(pos), _) => Some(pos.type_name.clone()),
                    (ConnectorPosition::Field(_), Some(EntityResolver::Explicit)) => {
                        connector.id.directive.base_type_name(schema.schema())
                    }
                    (ConnectorPosition::Field(_), Some(_)) => {
                        connector.id.directive.parent_type_name()
                    }
                    (ConnectorPosition::Field(_), None) => None,
                };
                if let Some(entity_type) = entity_type {
                    index
                        .entity_resolvers
                        .entry(entity_type)
                        .or_default()
                        .push(connector.clone());
                }

                if let ConnectorPosition::Field(pos) = &connector.id.directive {
                    index
                        .by_field
                        .entry((
                            pos.field.type_name().clone(),
                            pos.field.field_name().clone(),
                        ))
                        .or_default()
                        .push(connector);
                }
            }
        }

        index
    }

    /// Look up connectors that can resolve a specific field on a type.
    pub fn by_field(&self, type_name: &Name, field_name: &Name) -> Option<&[Arc<Connector>]> {
        self.by_field
            .get(&(type_name.clone(), field_name.clone()))
            .map(|v| v.as_slice())
    }

    /// Look up connectors that provide entity resolution for a type.
    pub fn entity_resolvers(&self, type_name: &Name) -> Option<&[Arc<Connector>]> {
        self.entity_resolvers.get(type_name).map(|v| v.as_slice())
    }

    /// The static output shape of a connector's `selection`, when indexed.
    pub(crate) fn output_shape(&self, coordinate: &str) -> Option<&Shape> {
        self.output_shapes.get(coordinate)
    }

    /// The parent-data key condition for entering an entity-resolver
    /// connector, when it has a resolvable key.
    pub(crate) fn key_conditions(&self, coordinate: &str) -> Option<&Arc<SelectionSet>> {
        self.key_conditions.get(coordinate)
    }

    /// Whether a connector's `selection` returns `field` at the top level of
    /// its (object-shaped) output. `__typename` is always available. False
    /// for non-object output shapes — a scalar connector resolves no entity
    /// fields beyond its own.
    pub(crate) fn resolver_provides(&self, coordinate: &str, field: &str) -> bool {
        if field == "__typename" {
            return true;
        }
        match self.output_shapes.get(coordinate).map(|s| s.case()) {
            Some(ShapeCase::Object { fields, .. }) => fields.contains_key(field),
            _ => false,
        }
    }

    /// Whether `subgraph` is backed by connectors (and therefore has no
    /// GraphQL endpoint behind its service name).
    pub(crate) fn is_connector_subgraph(&self, subgraph: &str) -> bool {
        self.connector_subgraphs.contains(subgraph)
    }

    /// Returns true if this index contains no connectors.
    pub fn is_empty(&self) -> bool {
        self.by_field.is_empty() && self.entity_resolvers.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use apollo_compiler::name;

    use super::*;

    #[test]
    fn empty_index() {
        let index = ConnectorIndex::from_subgraphs(std::iter::empty::<(
            &ValidFederationSchema,
            Vec<Connector>,
        )>());
        assert!(index.is_empty());
        assert!(index.by_field(&name!("Product"), &name!("name")).is_none());
        assert!(index.entity_resolvers(&name!("Product")).is_none());
        assert!(!index.is_connector_subgraph("connectors"));
    }
}
