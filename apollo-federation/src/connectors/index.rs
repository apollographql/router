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
use crate::error::FederationError;
use crate::operation::FragmentSpreadCache;
use crate::operation::SelectionSet;
use crate::schema::ValidFederationSchema;

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
    by_field: HashMap<(Name, Name), Vec<Arc<IndexedConnector>>>,
    /// entity type name -> connectors that can resolve that entity. Keyed by
    /// the type the resolver *produces* (`base_type_name` for `Explicit` and
    /// type-level resolvers, the parent type for `Implicit`), so a lookup by
    /// entity type finds `Query.user(id:) @connect(entity: true)` under
    /// `User`, not under `Query`.
    entity_resolvers: HashMap<Name, Vec<Arc<IndexedConnector>>>,
    /// connector coordinate -> static output shape of its `selection`.
    output_shapes: HashMap<Arc<str>, Shape>,
    /// connector coordinate -> parent-data key condition for entering the
    /// connector, parsed against its subgraph schema. Present only for
    /// entity-resolver connectors with a resolvable key.
    key_conditions: HashMap<Arc<str>, Arc<SelectionSet>>,
    /// Subgraphs that contain at least one connector. There is no GraphQL
    /// endpoint behind these subgraph names: their fields are reachable only
    /// through connectors, so subgraph-edge routes into them are invalid.
    connector_subgraphs: HashSet<Arc<str>>,
    /// Synthetic service name -> source subgraph, one entry per connector.
    /// Plans emit connector fetches under these service names; consumers
    /// keyed by subgraph name use this to alias them back.
    service_subgraphs: HashMap<String, Arc<str>>,
}

/// A connector plus its identity strings, computed once at index build time
/// so routing enumeration allocates nothing per lookup.
#[derive(Debug)]
pub struct IndexedConnector {
    pub connector: Arc<Connector>,
    pub coordinate: Arc<str>,
    pub source_subgraph: Arc<str>,
}

impl ConnectorIndex {
    /// Build an index from each subgraph's schema and parsed connectors.
    /// Subgraphs without connectors contribute nothing and may be skipped.
    pub fn from_subgraphs<'a>(
        subgraphs: impl IntoIterator<Item = (&'a ValidFederationSchema, Vec<Connector>)>,
    ) -> Result<Self, FederationError> {
        let mut index = Self::default();

        for (schema, connectors) in subgraphs {
            for connector in connectors {
                let entry = Arc::new(IndexedConnector {
                    coordinate: Arc::from(connector.id.coordinate()),
                    source_subgraph: Arc::from(connector.id.subgraph_name.as_str()),
                    connector: Arc::new(connector),
                });
                let connector = &entry.connector;

                index
                    .connector_subgraphs
                    .insert(entry.source_subgraph.clone());

                index
                    .service_subgraphs
                    .insert(connector.id.synthetic_name(), entry.source_subgraph.clone());

                index.output_shapes.insert(
                    entry.coordinate.clone(),
                    SelectionAnalysis::new(connector.selection.clone()).output_shape(),
                );

                // The parent-data condition guarding entry into an
                // entity-resolver connector — what expansion fabricated as a
                // synthetic @key. resolvable_key derives it from the
                // connector's variable references per resolver kind
                // ($args / $this / $batch).
                if connector.entity_resolver.is_some() {
                    let field_set = connector.resolvable_key(schema.schema()).map_err(|e| {
                        FederationError::internal(format!(
                            "invalid key for connector {}: {e}",
                            entry.coordinate
                        ))
                    })?;
                    if let Some(field_set) = field_set {
                        let selection_set = SelectionSet::from_selection_set(
                            &field_set.selection_set,
                            &FragmentSpreadCache::default(),
                            schema,
                            &|| Ok(()),
                        )?;
                        index
                            .key_conditions
                            .insert(entry.coordinate.clone(), Arc::new(selection_set));
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
                        .push(entry.clone());
                }

                if let ConnectorPosition::Field(pos) = &connector.id.directive {
                    index
                        .by_field
                        .entry((
                            pos.field.type_name().clone(),
                            pos.field.field_name().clone(),
                        ))
                        .or_default()
                        .push(entry.clone());
                }
            }
        }

        Ok(index)
    }

    /// Look up connectors that can resolve a specific field on a type.
    pub fn by_field(
        &self,
        type_name: &Name,
        field_name: &Name,
    ) -> Option<&[Arc<IndexedConnector>]> {
        self.by_field
            .get(&(type_name.clone(), field_name.clone()))
            .map(|v| v.as_slice())
    }

    /// Look up connectors that provide entity resolution for a type.
    pub fn entity_resolvers(&self, type_name: &Name) -> Option<&[Arc<IndexedConnector>]> {
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

    /// Every connector's synthetic service name with its source subgraph.
    pub fn service_subgraphs(&self) -> impl Iterator<Item = (&str, &str)> {
        self.service_subgraphs
            .iter()
            .map(|(service, subgraph)| (service.as_str(), subgraph.as_ref()))
    }
}

#[cfg(test)]
mod tests {
    use apollo_compiler::name;

    use super::*;
    use crate::Supergraph;

    const ENTITY_RESOLVER_SCHEMA: &str = include_str!(
        "../query_plan/incremental_planner/fixtures/connector_entity_resolver.graphql"
    );

    fn index_from_supergraph(sdl: &str) -> Result<ConnectorIndex, FederationError> {
        let supergraph = Supergraph::new_with_router_specs(sdl).expect("supergraph parses");
        let subgraphs: Vec<_> = supergraph
            .extract_subgraphs()
            .expect("subgraphs extract")
            .into_iter()
            .map(|(_, subgraph)| subgraph)
            .collect();
        let pairs: Vec<_> = subgraphs
            .iter()
            .map(|subgraph| {
                let connectors = Connector::from_schema(subgraph.schema.schema(), &subgraph.name)
                    .expect("connectors parse");
                (&subgraph.schema, connectors)
            })
            .filter(|(_, connectors)| !connectors.is_empty())
            .collect();
        ConnectorIndex::from_subgraphs(pairs)
    }

    #[test]
    fn entity_resolver_indexed_by_produced_type() {
        let index = index_from_supergraph(ENTITY_RESOLVER_SCHEMA).expect("index builds");
        let resolvers = index
            .entity_resolvers(&name!("User"))
            .expect("User has an entity resolver");
        assert_eq!(resolvers.len(), 1);
        assert_eq!(resolvers[0].coordinate.as_ref(), "connectors:Query.user[0]");
        assert_eq!(resolvers[0].source_subgraph.as_ref(), "connectors");
        // The resolver produces User; it must not be indexed under its
        // parent type.
        assert!(index.entity_resolvers(&name!("Query")).is_none());
    }

    #[test]
    fn key_conditions_parsed_for_entity_resolver() {
        let index = index_from_supergraph(ENTITY_RESOLVER_SCHEMA).expect("index builds");
        let key = index
            .key_conditions("connectors:Query.user[0]")
            .expect("resolvable key indexed");
        assert_eq!(
            key.to_string().split_whitespace().collect::<String>(),
            "{id}"
        );
    }

    #[test]
    fn resolver_provides_selected_fields_and_typename() {
        let index = index_from_supergraph(ENTITY_RESOLVER_SCHEMA).expect("index builds");
        let coordinate = "connectors:Query.user[0]";
        assert!(index.resolver_provides(coordinate, "name"));
        assert!(index.resolver_provides(coordinate, "id"));
        assert!(index.resolver_provides(coordinate, "__typename"));
        assert!(!index.resolver_provides(coordinate, "email"));
        assert!(index.output_shape(coordinate).is_some());
    }

    #[test]
    fn connector_subgraphs_tracked() {
        let index = index_from_supergraph(ENTITY_RESOLVER_SCHEMA).expect("index builds");
        assert!(index.is_connector_subgraph("connectors"));
        assert!(!index.is_connector_subgraph("graphql"));
        assert!(!index.is_empty());
    }

    /// Expansion strips @connect from the virtual subgraphs it fabricates,
    /// so a planner built from an expanded supergraph must see an empty
    /// index and route through the virtual subgraphs, never both. If this
    /// fails, expansion started preserving connect applications and the
    /// index build must be gated on the expansion mode instead.
    #[test]
    fn expanded_supergraph_yields_empty_index() {
        use crate::connectors::expand::ExpansionResult;
        use crate::connectors::expand::expand_connectors;

        let expanded = match expand_connectors(ENTITY_RESOLVER_SCHEMA, &Default::default())
            .expect("expansion runs")
        {
            ExpansionResult::Expanded { raw_sdl, .. } => raw_sdl,
            ExpansionResult::Unchanged => panic!("fixture has connectors to expand"),
        };
        let index = index_from_supergraph(&expanded).expect("index builds");
        assert!(
            index.is_empty(),
            "virtual subgraphs must not carry connect applications"
        );
    }

    #[test]
    fn key_condition_failure_propagates() {
        // The avatar connector's $this.missing does not exist on User, so
        // its resolvable key cannot be built. The index must surface the
        // error instead of silently indexing the connector without a key.
        let broken = ENTITY_RESOLVER_SCHEMA.replace(
            "  name: String @join__field(graph: CONNECTORS)\n",
            "  name: String @join__field(graph: CONNECTORS)\n  avatar: String @join__field(graph: CONNECTORS) @join__directive(graphs: [CONNECTORS], name: \"connect\", args: {source: \"api\", http: {GET: \"/avatars/{$this.missing}\"}, selection: \"$.url\"})\n",
        );
        let result = index_from_supergraph(&broken);
        assert!(
            result.is_err(),
            "unresolvable connector key must error, got {result:?}"
        );
    }

    #[test]
    fn empty_index() {
        let index = ConnectorIndex::from_subgraphs(std::iter::empty::<(
            &ValidFederationSchema,
            Vec<Connector>,
        )>())
        .expect("empty index builds");
        assert!(index.is_empty());
        assert!(index.by_field(&name!("Product"), &name!("name")).is_none());
        assert!(index.entity_resolvers(&name!("Product")).is_none());
        assert!(!index.is_connector_subgraph("connectors"));
    }
}
