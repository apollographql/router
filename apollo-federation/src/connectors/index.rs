use std::borrow::Cow;
use std::collections::HashMap;
use std::collections::HashSet;
use std::sync::Arc;

use apollo_compiler::Name;
use apollo_compiler::Node;
use apollo_compiler::Schema;
use apollo_compiler::ast::Argument;
use apollo_compiler::ast::Value;
use apollo_compiler::executable::FieldSet;
use apollo_compiler::name;
use apollo_compiler::schema::ExtendedType;
use apollo_compiler::validation::Valid;
use indexmap::IndexMap;
use shape::Shape;
use shape::ShapeCase;

use super::id::ConnectorPosition;
use super::json_selection::SelectionAnalysis;
use super::models::Connector;
use super::models::EntityResolver;
use super::spec::ConnectSpec;
use crate::error::FederationError;
use crate::operation::FragmentSpreadCache;
use crate::operation::Selection;
use crate::operation::SelectionMap;
use crate::operation::SelectionSet;
use crate::operation::TYPENAME_FIELD;
use crate::schema::FederationSchema;
use crate::schema::ValidFederationSchema;
use crate::supergraph::extract_subgraphs_from_supergraph;

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
    /// connector coordinate -> the field an `Implicit` (field-level) entity
    /// resolver provides. Its `selection` describes that field's value, not
    /// the entity.
    implicit_fields: HashMap<Arc<str>, Name>,
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
                if connector.entity_resolver.is_some()
                    && let Some(field_set) = connector_entity_key(connector, schema.schema())?
                {
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

                // Entity resolvers, keyed by the entity type they resolve.
                if let Some(entity_type) = resolver_entity_type(connector, schema.schema()) {
                    index
                        .entity_resolvers
                        .entry(entity_type)
                        .or_default()
                        .push(entry.clone());
                }

                if let ConnectorPosition::Field(pos) = &connector.id.directive {
                    if matches!(connector.entity_resolver, Some(EntityResolver::Implicit)) {
                        index
                            .implicit_fields
                            .insert(entry.coordinate.clone(), pos.field.field_name().clone());
                    }
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

    /// The shape an entity-resolver connector returns for the entity's field
    /// `field_name`, when it returns that field.
    pub(crate) fn resolver_field_shape(&self, coordinate: &str, field_name: &str) -> Option<Shape> {
        let shape = self.output_shapes.get(coordinate)?;
        if let Some(own_field) = self.implicit_fields.get(coordinate) {
            return (own_field == field_name).then(|| shape.clone());
        }
        object_fields(shape)?.get(field_name).cloned()
    }

    /// Whether an entity-resolver connector returns every field of the
    /// entity's `selection`.
    pub(crate) fn resolver_provides(&self, coordinate: &str, selection: &Selection) -> bool {
        self.resolver_returns(coordinate, selection, shape_provides)
    }

    /// Whether an entity-resolver connector returns some field of the
    /// entity's `selection`; see [`shape_provides_any`].
    pub(crate) fn resolver_provides_any(&self, coordinate: &str, selection: &Selection) -> bool {
        self.resolver_returns(coordinate, selection, shape_provides_any)
    }

    /// `__typename` is always available. A field-level (`Implicit`) resolver
    /// returns only its own field, whose value its `selection` describes.
    /// Otherwise the `selection` describes the entity. A resolver whose
    /// output isn't an object returns nothing of it.
    fn resolver_returns(
        &self,
        coordinate: &str,
        selection: &Selection,
        returns: fn(&Shape, &Selection) -> bool,
    ) -> bool {
        if selection.is_typename_field() {
            return true;
        }
        let Some(shape) = self.output_shapes.get(coordinate) else {
            return false;
        };
        if let Some(own_field) = self.implicit_fields.get(coordinate) {
            let entity = Shape::closed_record(
                IndexMap::from_iter([(own_field.to_string(), shape.clone())]),
                [],
            );
            return returns(&entity, selection);
        }
        object_fields(shape).is_some() && returns(shape, selection)
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

/// Strip array wrappers off a shape: a selection set on a list-typed field
/// is checked against the element shape.
pub(crate) fn unwrap_list_shape(shape: &Shape) -> &Shape {
    let mut shape = shape;
    while let ShapeCase::Array { tail, .. } = shape.case() {
        shape = tail;
    }
    shape
}

/// Split `selections` into the top-level selections `provides` accepts and
/// the rest; `None` stands for an empty part.
pub(crate) fn split_selections(
    selections: &SelectionSet,
    provides: impl Fn(&Selection) -> bool,
) -> (Option<SelectionSet>, Option<SelectionSet>) {
    let mut provided = SelectionMap::new();
    let mut missing = SelectionMap::new();
    for selection in selections.selections.values() {
        if provides(selection) {
            provided.insert(selection.clone());
        } else {
            missing.insert(selection.clone());
        }
    }
    let part = |map: SelectionMap| {
        (!map.is_empty()).then(|| SelectionSet {
            schema: selections.schema.clone(),
            type_position: selections.type_position.clone(),
            selections: Arc::new(map),
        })
    };
    (part(provided), part(missing))
}

/// The fields `shape` returns as an object, list wrappers stripped. A union
/// of shapes (e.g. a `->match`) returns the fields every object member
/// returns, each shaped as the union of the members' shapes for it; null
/// and non-object members don't constrain it. `None` for shapes the planner
/// can't inspect (scalars, unknown JSON), which count as returning
/// everything.
pub(crate) fn object_fields(shape: &Shape) -> Option<Cow<'_, IndexMap<String, Shape>>> {
    match unwrap_list_shape(shape).case() {
        ShapeCase::Object { fields, .. } => Some(Cow::Borrowed(fields)),
        ShapeCase::One(members) => {
            let mut objects = members.iter().filter_map(object_fields);
            let first = objects.next()?;
            let others: Vec<_> = objects.collect();
            let fields = first
                .iter()
                .filter_map(|(name, shape)| {
                    let mut shapes = vec![shape.clone()];
                    for other in &others {
                        shapes.push(other.get(name)?.clone());
                    }
                    Some((name.clone(), Shape::one(shapes, [])))
                })
                .collect();
            Some(Cow::Owned(fields))
        }
        _ => None,
    }
}

/// Whether `shape` returns every field of `selection`.
pub(crate) fn shape_provides(shape: &Shape, selection: &Selection) -> bool {
    let Some(fields) = object_fields(shape) else {
        return true;
    };
    let provides_all = |shape: &Shape, selections: &SelectionSet| {
        selections
            .selections
            .values()
            .all(|selection| shape_provides(shape, selection))
    };
    match selection {
        Selection::Field(field_sel) => {
            let name = field_sel.field.name();
            if *name == TYPENAME_FIELD {
                return true;
            }
            let Some(child) = fields.get(name.as_str()) else {
                return false;
            };
            field_sel
                .selection_set
                .as_ref()
                .is_none_or(|child_ss| provides_all(child, child_ss))
        }
        Selection::InlineFragment(fragment_sel) => provides_all(shape, &fragment_sel.selection_set),
    }
}

/// Whether `shape` returns some field of `selection` other than
/// `__typename`: a leaf field itself, or some field below a composite one.
/// A sub-selection of only `__typename` counts as returned with its field.
pub(crate) fn shape_provides_any(shape: &Shape, selection: &Selection) -> bool {
    let Some(fields) = object_fields(shape) else {
        return true;
    };
    let provides_any = |shape: &Shape, selections: &SelectionSet| {
        let mut selections = selections
            .selections
            .values()
            .filter(|selection| !selection.is_typename_field())
            .peekable();
        selections.peek().is_none()
            || selections.any(|selection| shape_provides_any(shape, selection))
    };
    match selection {
        Selection::Field(field_sel) => {
            let name = field_sel.field.name();
            if *name == TYPENAME_FIELD {
                return true;
            }
            let Some(child) = fields.get(name.as_str()) else {
                return false;
            };
            field_sel
                .selection_set
                .as_ref()
                .is_none_or(|child_ss| provides_any(child, child_ss))
        }
        Selection::InlineFragment(fragment_sel) => provides_any(shape, &fragment_sel.selection_set),
    }
}

/// The entity type an entity-resolver connector produces: the type itself for
/// type-level connectors, the field's type for `Explicit` resolvers, and the
/// parent type for `Implicit` resolvers.
fn resolver_entity_type(connector: &Connector, schema: &Schema) -> Option<Name> {
    match (&connector.id.directive, &connector.entity_resolver) {
        (ConnectorPosition::Type(pos), _) => Some(pos.type_name.clone()),
        (ConnectorPosition::Field(_), Some(EntityResolver::Explicit)) => {
            connector.id.directive.base_type_name(schema)
        }
        (ConnectorPosition::Field(_), Some(_)) => connector.id.directive.parent_type_name(),
        (ConnectorPosition::Field(_), None) => None,
    }
}

/// The key guarding entry into an entity-resolver connector. A field
/// connector that references no `$this` (only `$args` or constants) gets the
/// singleton key `__typename` (see [`has_singleton_key`]).
fn connector_entity_key(
    connector: &Connector,
    schema: &Schema,
) -> Result<Option<Valid<FieldSet>>, FederationError> {
    let invalid_key = |e: String| {
        FederationError::internal(format!(
            "invalid key for connector {}: {e}",
            connector.id.coordinate()
        ))
    };
    if let Some(field_set) = connector.resolvable_key(schema).map_err(invalid_key)? {
        return Ok(Some(field_set));
    }
    if !has_singleton_key(connector) {
        return Ok(None);
    }
    let parent_type = connector
        .id
        .directive
        .parent_type_name()
        .ok_or_else(|| invalid_key("missing parent type".to_string()))?;
    FieldSet::parse_and_validate(
        Valid::assume_valid_ref(schema),
        parent_type,
        TYPENAME_FIELD.as_str(),
        "",
    )
    .map(Some)
    .map_err(|e| invalid_key(e.to_string()))
}

/// Whether a field connector without a resolvable key is entered through the
/// singleton key `__typename`. Connector expansion gives that key only from
/// connect v0.4 on; before v0.4 such a connector has no key.
fn has_singleton_key(connector: &Connector) -> bool {
    connector.spec >= ConnectSpec::V0_4
        && matches!(connector.entity_resolver, Some(EntityResolver::Implicit))
}

/// Adds to the supergraph the `@key` that connector expansion synthesizes for
/// each entity-resolver connector whose type has no key in its subgraph. The
/// connector's subgraph then defines the `_Entity` union and `_entities` field
/// that entity fetches through the connector are written against.
pub(crate) fn add_connector_entity_keys(
    supergraph: &ValidFederationSchema,
) -> Result<ValidFederationSchema, FederationError> {
    // The connect spec is linked either directly or, in older supergraphs,
    // through a `@join__directive` on the schema definition.
    let connect_identity = ConnectSpec::identity();
    let connect_url = format!("{}/{}/v", connect_identity.domain, connect_identity.name);
    let uses_connectors = supergraph
        .schema()
        .schema_definition
        .directives
        .iter()
        .any(|directive| directive.to_string().contains(&connect_url));
    if !uses_connectors {
        return Ok(supergraph.clone());
    }
    let (_, join_spec, _) = crate::validate_supergraph_for_query_planning(supergraph)?;
    let graph_directive = join_spec.graph_directive_definition(supergraph)?;
    let mut graph_enum_values: HashMap<String, Name> = HashMap::new();
    for (value_name, value) in join_spec.graph_enum_definition(supergraph)?.values.iter() {
        if let Some(application) = value.directives.get(&graph_directive.name) {
            let arguments = join_spec.graph_directive_arguments(application)?;
            graph_enum_values.insert(arguments.name.to_owned(), value_name.clone());
        }
    }

    let mut keys: Vec<(Name, Name, String)> = Vec::new();
    // Singleton `__typename` keys apply only to types no other connector
    // gives a key.
    let mut typename_keys: Vec<(Name, Name, String)> = Vec::new();
    for (subgraph_name, subgraph) in extract_subgraphs_from_supergraph(supergraph, Some(false))? {
        let schema = subgraph.schema.schema();
        let Some(graph) = graph_enum_values.get(subgraph_name.as_ref()) else {
            continue;
        };
        for connector in Connector::from_schema(schema, &subgraph_name)? {
            let Some(entity_type) = resolver_entity_type(&connector, schema) else {
                continue;
            };
            let field_set = match connector.resolvable_key(schema) {
                Ok(Some(field_set)) => field_set,
                Ok(None) if has_singleton_key(&connector) => {
                    typename_keys.push((graph.clone(), entity_type, TYPENAME_FIELD.to_string()));
                    continue;
                }
                _ => continue,
            };
            let fields = field_set.serialize().no_indent().to_string();
            // Extraction rejects a key whose fields the subgraph doesn't define.
            let fields_defined = field_set.selection_set.selections.iter().all(|selection| {
                selection
                    .as_field()
                    .is_some_and(|field| schema.type_field(&entity_type, &field.name).is_ok())
            });
            let known = keys.iter().any(|(g, t, _)| g == graph && *t == entity_type);
            if fields_defined && !known {
                keys.push((graph.clone(), entity_type, fields));
            }
        }
    }
    for (graph, entity_type, fields) in typename_keys {
        if !keys
            .iter()
            .any(|(g, t, _)| *g == graph && *t == entity_type)
        {
            keys.push((graph, entity_type, fields));
        }
    }
    if keys.is_empty() {
        return Ok(supergraph.clone());
    }

    let type_directive_name = join_spec
        .type_directive_definition(supergraph)?
        .name
        .clone();
    let mut schema = supergraph.schema().clone().into_inner();
    for (graph, type_name, fields) in keys {
        let directives = match schema.types.get_mut(&type_name) {
            Some(ExtendedType::Object(object)) => &mut object.make_mut().directives,
            Some(ExtendedType::Interface(interface)) => &mut interface.make_mut().directives,
            _ => continue,
        };
        let Some(application) = directives.iter_mut().find(|directive| {
            directive.name == type_directive_name
                && directive
                    .specified_argument_by_name("graph")
                    .and_then(|value| value.as_enum())
                    == Some(&graph)
        }) else {
            continue;
        };
        if application.specified_argument_by_name("key").is_none() {
            application.make_mut().arguments.push(Node::new(Argument {
                name: name!("key"),
                value: Node::new(Value::String(fields)),
            }));
        }
    }
    FederationSchema::new(schema)?.assume_valid()
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

    /// `field_set` on `type_name`, parsed against `subgraph`'s schema.
    fn subgraph_selection(
        sdl: &str,
        subgraph: &str,
        type_name: &str,
        field_set: &str,
    ) -> Selection {
        let supergraph = Supergraph::new_with_router_specs(sdl).expect("supergraph parses");
        let subgraphs = supergraph.extract_subgraphs().expect("subgraphs extract");
        let subgraph = subgraphs.get(subgraph).expect("subgraph exists");
        crate::schema::field_set::parse_field_set(
            &subgraph.schema,
            Name::new(type_name).expect("valid type name"),
            field_set,
            true,
        )
        .expect("field set parses")
        .selections
        .values()
        .next()
        .expect("one selection")
        .clone()
    }

    #[test]
    fn resolver_provides_selected_fields_and_typename() {
        let index = index_from_supergraph(ENTITY_RESOLVER_SCHEMA).expect("index builds");
        let coordinate = "connectors:Query.user[0]";
        let provides = |subgraph, field| {
            index.resolver_provides(
                coordinate,
                &subgraph_selection(ENTITY_RESOLVER_SCHEMA, subgraph, "User", field),
            )
        };
        assert!(provides("connectors", "name"));
        assert!(provides("connectors", "id"));
        assert!(provides("connectors", "__typename"));
        assert!(!provides("graphql", "email"));
        assert!(index.output_shape(coordinate).is_some());
    }

    /// A field-level resolver's selection describes its field's value
    /// (`User.posts` returns `id title body` per post), so it provides only
    /// its own field of the entity.
    #[test]
    fn field_resolver_provides_only_its_own_field() {
        let index = index_from_supergraph(include_str!(
            "../query_plan/incremental_planner/fixtures/connector_join_directive_link.graphql"
        ))
        .expect("index builds");
        let coordinate = "connectors:User.posts[0]";
        let provides = |field| {
            index.resolver_provides(
                coordinate,
                &subgraph_selection(
                    include_str!(
                        "../query_plan/incremental_planner/fixtures/connector_join_directive_link.graphql"
                    ),
                    "connectors",
                    "User",
                    field,
                ),
            )
        };
        assert!(provides("posts { id title }"));
        assert!(!provides("posts { author { id } }"));
        assert!(provides("__typename"));
        assert!(!provides("id"));
    }

    /// A field connector that references no `$this` is keyed on
    /// `__typename`, as connector expansion keys it from connect v0.4 on.
    #[test]
    fn field_resolver_without_this_keyed_on_typename() {
        let index = index_from_supergraph(include_str!(
            "../query_plan/incremental_planner/fixtures/connector_field_without_this.graphql"
        ))
        .expect("index builds");
        for coordinate in ["products:Product.banner[0]", "products:Product.related[0]"] {
            let key = index
                .key_conditions(coordinate)
                .expect("singleton key indexed");
            assert_eq!(
                key.to_string().split_whitespace().collect::<String>(),
                "{__typename}",
                "{coordinate}"
            );
        }
    }

    /// Before connect v0.4, expansion gives a field connector that references
    /// no `$this` no key, so the index doesn't either.
    #[test]
    fn field_resolver_without_this_before_v0_4_has_no_key() {
        let index = index_from_supergraph(include_str!(
            "../query_plan/incremental_planner/fixtures/connector_output_shape.graphql"
        ))
        .expect("index builds");
        assert!(
            index
                .key_conditions("connectors:User.staticAvatar[0]")
                .is_none()
        );
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
