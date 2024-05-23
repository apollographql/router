#![allow(dead_code)]

use std::collections::HashMap;
use std::sync::Arc;

use apollo_compiler::ast::Directive;
use apollo_compiler::name;
use apollo_compiler::schema::Component;
use apollo_compiler::schema::ComponentOrigin;
// use apollo_compiler::schema::ComponentName;
use apollo_compiler::schema::DirectiveList;
use apollo_compiler::schema::ExtendedType;
use apollo_compiler::schema::FieldDefinition;
use apollo_compiler::schema::Name;
// use apollo_compiler::schema::ScalarType;
use apollo_compiler::schema::UnionType;
use apollo_compiler::schema::Value;
use apollo_compiler::validation::Valid;
use apollo_compiler::Node;
use apollo_compiler::Schema;
use apollo_federation::sources::connect::JSONSelection;
use apollo_federation::sources::connect::URLPathTemplate;
use http::Method;
use indexmap::IndexMap;
use itertools::Itertools;
use serde::Serialize;

use super::join_spec_helpers::*;
use super::supergraph::*;
use super::Connector;
use crate::error::ConnectorDirectiveError;

const SOURCE_API_DIRECTIVE_NAME: &str = "sourceAPI";
const HTTP_ARGUMENT_NAME: &str = "http";
const SOURCE_TYPE_DIRECTIVE_NAME: &str = "sourceType";
const SOURCE_FIELD_DIRECTIVE_NAME: &str = "sourceField";
const SOURCE_DIRECTIVE_URL: &str = "https://specs.apollo.dev/source/v0.1";

const JOIN_DIRECTIVE_DIRECTIVE_NAME: &str = "join__directive";
const JOIN_TYPE_DIRECTIVE_NAME: &str = "join__type";
const JOIN_FIELD_DIRECTIVE_NAME: &str = "join__field";
const JOIN_GRAPH_ENUM_NAME: &str = "join__Graph";
const JOIN_GRAPH_DIRECTIVE_NAME: &str = "join__graph";

// --- @join__* ----------------------------------------------------------------

fn graph_enum_map(schema: &apollo_compiler::Schema) -> Option<HashMap<String, String>> {
    schema.get_enum(JOIN_GRAPH_ENUM_NAME).map(|e| {
        e.values
            .iter()
            .map(|(name, node)| {
                (
                    name.to_string(),
                    node.directives
                        .iter()
                        .find_map(|d| {
                            if d.name == JOIN_GRAPH_DIRECTIVE_NAME {
                                d.argument_by_name("name")
                                    .as_ref()
                                    .and_then(|name| name.as_str())
                                    .map(|app_name| app_name.to_string())
                            } else {
                                None
                            }
                        })
                        .unwrap_or_default(),
                )
            })
            .collect::<HashMap<_, _>>()
    })
}

#[derive(Clone, Debug)]
struct JoinedDirective {
    graphs: Vec<String>,
    directive: DirectiveAsObject,
}

impl JoinedDirective {
    fn from_schema_directive_list(
        // Unfortunately we use a_c::schema::DL in some places and a_c::ast::DL
        // in others, so this method handles the apollo_compiler::schema case.
        list: &apollo_compiler::schema::DirectiveList,
    ) -> Result<Vec<Self>, ConnectorDirectiveError> {
        let ast_directive_list = apollo_compiler::ast::DirectiveList(
            list.iter()
                .map(|component| component.node.clone())
                .collect::<Vec<_>>(),
        );
        // Delegate to the main implementation below.
        Self::from_directive_list(&ast_directive_list)
    }

    fn from_directive_list(
        list: &apollo_compiler::ast::DirectiveList,
    ) -> Result<Vec<Self>, ConnectorDirectiveError> {
        let mut joins = vec![];

        for join_directive in list
            .iter()
            .filter(|d| d.name == JOIN_DIRECTIVE_DIRECTIVE_NAME)
        {
            let directive_name = join_directive
                .argument_by_name("name")
                .and_then(|name| name.as_str())
                .map(|name| name.to_string())
                .ok_or_else(|| {
                    ConnectorDirectiveError::InvalidJoinDirective(
                        "Expected name to be a string".to_string(),
                    )
                })?;

            let directive_args =
                join_directive
                    .argument_by_name("args")
                    .map_or_else(HashMap::new, |args| {
                        args.as_object()
                            .map(|args| {
                                args.iter()
                                    .map(|(name, value)| (name.clone(), value.clone()))
                                    .collect::<HashMap<_, _>>()
                            })
                            .unwrap_or_default()
                    });

            if let Some(graphs_node) = join_directive.argument_by_name("graphs") {
                if let Some(graphs_list) = graphs_node.as_list() {
                    // Convert the graphs enum list to a list of String.
                    let graphs = graphs_list
                        .iter()
                        .map(|graph| {
                            Ok(graph
                                .as_enum()
                                .ok_or_else(|| {
                                    ConnectorDirectiveError::InvalidJoinDirective(
                                        "Expected graphs to be an enum".to_string(),
                                    )
                                })?
                                .to_string())
                        })
                        .collect::<Result<Vec<_>, _>>()?;

                    let directive = DirectiveAsObject {
                        name: directive_name,
                        args: directive_args,
                    };

                    joins.push(Self { graphs, directive });
                }
            }
        }

        Ok(joins)
    }
}

#[derive(Clone, Debug)]
struct DirectiveAsObject {
    name: String,
    args: HashMap<Name, Node<apollo_compiler::ast::Value>>,
}

impl TryFrom<&Node<apollo_compiler::ast::Value>> for DirectiveAsObject {
    type Error = ConnectorDirectiveError;

    fn try_from(directive: &Node<apollo_compiler::ast::Value>) -> Result<Self, Self::Error> {
        let directive = directive
            .as_object()
            .ok_or_else(|| {
                ConnectorDirectiveError::InvalidJoinDirective(
                    "Expected directive to be an object".to_string(),
                )
            })?
            .iter()
            .map(|(name, value)| (name.clone(), value.clone()))
            .collect::<HashMap<_, _>>();

        let name = directive
            .get(&name!("name"))
            .ok_or_else(|| {
                ConnectorDirectiveError::InvalidJoinDirective(
                    "Expected directive to have a name".to_string(),
                )
            })?
            .as_str()
            .ok_or_else(|| {
                ConnectorDirectiveError::InvalidJoinDirective(
                    "Expected name to be a string".to_string(),
                )
            })?
            .to_string();

        let args = directive
            .get(&name!("args"))
            .ok_or_else(|| {
                ConnectorDirectiveError::InvalidJoinDirective(
                    "Expected directive to have args".to_string(),
                )
            })?
            .as_object()
            .ok_or_else(|| {
                ConnectorDirectiveError::InvalidJoinDirective(
                    "Expected args to be an object".to_string(),
                )
            })?
            .iter()
            .map(|(name, value)| (name.clone(), value.clone()))
            .collect::<HashMap<_, _>>();

        Ok(Self { name, args })
    }
}

// --- Source wrapper struct ---------------------------------------------------

#[derive(Debug, Clone)]
pub(crate) struct Source {
    connectors: Arc<HashMap<Arc<String>, Connector>>,
    supergraph: Arc<Valid<Schema>>,
}

impl Source {
    pub(crate) fn new(schema: &Schema) -> Result<Option<Self>, ConnectorDirectiveError> {
        let graph_names = if let Some(graph_names) = graph_enum_map(schema) {
            graph_names
        } else {
            return Ok(None);
        };

        let apis = SourceAPI::from_schema_and_graph_names(schema, &graph_names)?;
        // No connector
        if apis.is_empty() {
            return Ok(None);
        }

        let types = SourceType::from_schema_and_graph_names(schema, &graph_names)?;
        let fields = SourceField::from_schema_and_graph_names(schema, &graph_names)?;

        let connectors = Arc::new(Self::generate_connectors(apis, types, fields)?);
        let supergraph = Arc::new(
            Self::generate_connector_supergraph(schema, &connectors)
                .map_err(ConnectorDirectiveError::InconsistentSchema)?,
        );
        Ok(Some(Self {
            connectors,
            supergraph,
        }))
    }

    pub(crate) fn connectors(&self) -> Arc<HashMap<Arc<String>, Connector>> {
        Arc::clone(&self.connectors)
    }

    pub(crate) fn supergraph(&self) -> Arc<Valid<Schema>> {
        Arc::clone(&self.supergraph)
    }

    fn generate_connectors(
        apis: HashMap<String, SourceAPI>,
        types: Vec<SourceType>,
        fields: Vec<SourceField>,
    ) -> Result<HashMap<Arc<String>, Connector>, ConnectorDirectiveError> {
        if apis.is_empty() || (types.is_empty() && fields.is_empty()) {
            return Ok(Default::default());
        }

        let default_api = apis
            .values()
            .find(|api| api.is_default())
            .or_else(|| apis.values().next())
            .ok_or(ConnectorDirectiveError::NoSourceAPIDefined)?;

        let mut connectors = HashMap::new();

        for (i, directive) in types.into_iter().enumerate() {
            let connector_name =
                Arc::new(format!("CONNECTOR_{}_{}", directive.type_name, i).to_uppercase());
            let api = apis.get(&directive.api_name()).unwrap_or(default_api);

            connectors.insert(
                Arc::clone(&connector_name),
                Connector::new_from_source_type(connector_name, api.clone(), directive)?,
            );
        }

        for (i, directive) in fields.into_iter().enumerate() {
            let connector_name = Arc::new(
                format!(
                    "CONNECTOR_{}_{}_{}",
                    directive.parent_type_name, directive.field_name, i
                )
                .to_uppercase(),
            );

            let api = apis.get(&directive.api_name()).unwrap_or(default_api);
            connectors.insert(
                connector_name.clone(),
                Connector::new_from_source_field(connector_name, api, directive)?,
            );
        }

        Ok(connectors)
    }

    /// Generates a new supergraph schema with one subgraph per connector. Copies
    /// types and fields from the original schema and adds directives to associate
    /// them with the appropriate connector.
    fn generate_connector_supergraph(
        supergraph_schema: &Schema,
        connectors: &HashMap<Arc<String>, Connector>,
    ) -> Result<Valid<Schema>, ConnectorSupergraphError> {
        let mut inner_supergraph_schema = Schema::new();
        copy_definitions(supergraph_schema, &mut inner_supergraph_schema);

        /* enum name -> subgraph name  */
        let origin_subgraph_map = graph_enum_map(supergraph_schema)
            .ok_or_else(|| {
                ConnectorSupergraphError::InvalidOuterSupergraph("missing join__Graph enum".into())
            })?
            .into_iter()
            .map(|(k, v)| (v, k))
            .collect::<HashMap<_, _>>();

        let mut changes = Vec::new();

        // let mut entity_changes = Vec::new();
        // sorted for stable SDL generation
        for connector in connectors.values().sorted_by_key(|c| c.name.clone()) {
            changes.extend(make_changes(
                connector,
                supergraph_schema,
                &origin_subgraph_map,
            )?);
        }

        for change in changes {
            change.apply_to(supergraph_schema, &mut inner_supergraph_schema)?;
        }

        let connector_graph_names = connectors
            .values()
            // sorted for stable SDL generation
            .sorted_by_key(|c| c.name.clone())
            .map(|c| c.name.as_str())
            .collect::<Vec<_>>();
        inner_supergraph_schema.types.insert(
            name!("join__Graph"),
            join_graph_enum(&connector_graph_names),
        );

        add_fake_entity(&mut inner_supergraph_schema)?;
        update_any_scalar(&mut inner_supergraph_schema)?;

        inner_supergraph_schema
            .validate()
            .map_err(ConnectorSupergraphError::InvalidInnerSupergraph)
    }
}

fn update_any_scalar(inner_supergraph_schema: &mut Schema) -> Result<(), ConnectorSupergraphError> {
    let all_graphs = inner_supergraph_schema
        .types
        .get(&name!("join__Graph"))
        .map(|join_graph| {
            if let ExtendedType::Enum(jge) = join_graph {
                jge.values
                    .iter()
                    .map(|(k, _)| k.clone())
                    .collect::<Vec<_>>()
            } else {
                Default::default()
            }
        })
        .unwrap_or_default();

    if let Some(ExtendedType::Scalar(s)) = inner_supergraph_schema.types.get_mut(&name!("_Any")) {
        let scalar = s.make_mut();
        let mut new_directives = scalar
            .directives
            .clone()
            .into_iter()
            .filter(|d| d.name != name!("join__type"))
            .collect::<Vec<_>>();
        new_directives.extend(all_graphs.into_iter().map(|graph| {
            Component {
                origin: ComponentOrigin::Definition, // ?
                node: Directive {
                    name: name!("join__type"),
                    arguments: vec![Node::new(apollo_compiler::ast::Argument {
                        name: name!("graph"),
                        value: Value::Enum(graph.clone()).into(),
                    })],
                }
                .into(),
            }
        }));

        scalar.directives = DirectiveList(new_directives);
    }
    Ok(())
}

fn add_fake_entity(inner_supergraph_schema: &mut Schema) -> Result<(), ConnectorSupergraphError> {
    inner_supergraph_schema
        .types
        .insert(name!("_Entity"), entity_union(inner_supergraph_schema));

    let query_type = inner_supergraph_schema.types.get_mut("Query").unwrap();

    if let ExtendedType::Object(q) = query_type {
        q.make_mut().fields.insert(
            name!("_entities"),
            FieldDefinition {
                name: name!("_entities"),
                arguments: vec![apollo_compiler::schema::InputValueDefinition {
                    description: Default::default(),
                    directives: Default::default(),
                    default_value: Default::default(),
                    name: name!("representations"),
                    ty: apollo_compiler::ty!([_Any!]!).into(),
                }
                .into()],
                directives: apollo_compiler::ast::DirectiveList(vec![]),
                description: None,
                ty: apollo_compiler::schema::Type::Named(name!("_Entity"))
                    .non_null()
                    .list()
                    .non_null(),
            }
            .into(),
        );
    }

    Ok(())
    // Change::FakeEntities { graph_name }.apply_to(supergraph_schema, inner_supergraph_schema)
}

fn entity_union_members(schema: &Schema) -> Vec<(Name, ExtendedType)> {
    schema
        .types
        .iter()
        .filter(|(_, ty)| {
            ty.is_object()
                && !ty.is_built_in()
                && ty
                    .directives()
                    .iter()
                    .any(|arg| arg.name.to_string().starts_with("join__"))
        })
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect()
}

fn entity_union(schema: &Schema) -> ExtendedType {
    let join_union_member = name!("join__unionMember");
    let graph = name!("graph");
    let join_type = name!("join__type");
    let member = name!("member");
    let (names, join_directives): (Vec<_>, Vec<_>) = schema
        .types
        .iter()
        .filter(|(_, ty)| {
            ty.is_object()
                && !ty.is_built_in()
                && ty
                    .directives()
                    .iter()
                    .any(|arg| arg.name.to_string().starts_with("join__"))
        })
        .map(|(key, value)| {
            (
                key.into(),
                value
                    .directives()
                    .clone()
                    .into_iter()
                    // Only add resolvable types to the entity union
                    .filter(|d| {
                        d.name == join_type
                        // && d.arguments.iter().any(|arg| arg.name == name!("key"))
                    })
                    .flat_map(|d| {
                        let Component { origin, mut node } = d;
                        let directive = node.make_mut();
                        let Directive { arguments, .. } = directive.clone();

                        let graph_argument = arguments.into_iter().find(|a| a.name == graph);

                        if let Some(g) = graph_argument {
                            vec![
                                Component {
                                    origin: origin.clone(),
                                    node: Directive {
                                        name: join_type.clone(),
                                        arguments: vec![Node::new(
                                            apollo_compiler::ast::Argument {
                                                name: graph.clone(),
                                                value: g.value.clone(),
                                            },
                                        )],
                                    }
                                    .into(),
                                },
                                Component {
                                    origin,
                                    node: Directive {
                                        name: join_union_member.clone(),
                                        arguments: vec![
                                            Node::new(apollo_compiler::ast::Argument {
                                                name: graph.clone(),
                                                value: g.value.clone(),
                                            }),
                                            Node::new(apollo_compiler::ast::Argument {
                                                name: member.clone(),
                                                value: Node::new(Value::String(
                                                    key.to_string().into(),
                                                )),
                                            }),
                                        ],
                                    }
                                    .into(),
                                },
                            ]
                        } else {
                            vec![]
                        }
                    })
                    .collect::<Vec<Component<Directive>>>(),
            )
        })
        .unzip();

    let mut directives = join_directives.into_iter().flatten().collect::<Vec<_>>();
    directives.dedup();

    let directives = DirectiveList(directives);

    ExtendedType::Union(Node::new(UnionType {
        description: None,
        name: name!("_Entity"),
        directives,
        members: names.into_iter().collect(),
    }))
}

// --- @sourceAPI --------------------------------------------------------------

#[derive(Debug, Serialize, Clone)]
pub(super) struct SourceAPI {
    pub(crate) graph: String,
    pub(crate) name: Arc<String>,
    pub(crate) http: Option<HTTPSourceAPI>,
}

impl SourceAPI {
    pub(super) fn is_default(&self) -> bool {
        self.http
            .as_ref()
            .map(|http| http.default)
            .unwrap_or_default()
    }

    fn from_schema_and_graph_names(
        schema: &Schema,
        graph_names: &HashMap<String, String>,
    ) -> Result<HashMap<String, Self>, ConnectorDirectiveError> {
        let mut result = HashMap::new();

        let joins =
            JoinedDirective::from_schema_directive_list(&schema.schema_definition.directives)?;

        let mut source_apis = vec![];

        let source_api_directive_name = crate::spec::Schema::directive_name(
            schema,
            SOURCE_DIRECTIVE_URL,
            SOURCE_API_DIRECTIVE_NAME,
            "sourceAPI",
        )
        .unwrap_or_else(|| SOURCE_API_DIRECTIVE_NAME.to_string());

        for joined in joins.iter() {
            if joined.directive.name == source_api_directive_name {
                source_apis.extend(
                    joined
                        .graphs
                        .iter()
                        .map(|graph| (graph, &joined.directive.args))
                        .collect::<Vec<_>>(),
                );
            }
        }

        for (graph, args) in source_apis {
            let graph_name = graph_names.get(graph).ok_or_else(|| {
                ConnectorDirectiveError::InvalidJoinDirective(
                    format!("Missing graph {} in join__Graph enum", graph).to_string(),
                )
            })?;
            let source_api =
                Self::from_schema_directive(graph_name, source_api_directive_name.as_ref(), args)?;
            let name = format!("{}_{}", source_api.graph, source_api.name);
            result.insert(name, source_api);
        }

        Ok(result)
    }

    fn from_schema_directive(
        graph: &str,
        directive_name: &str,
        args: &HashMap<Name, Node<apollo_compiler::ast::Value>>,
    ) -> Result<Self, ConnectorDirectiveError> {
        let name = Arc::new(
            args.get(&name!("name"))
                .ok_or_else(|| {
                    ConnectorDirectiveError::MissingAttributeForType(
                        "name".to_string(),
                        directive_name.to_string(),
                    )
                })?
                .as_str()
                .ok_or_else(|| {
                    ConnectorDirectiveError::MissingAttributeForType(
                        "name".to_string(),
                        directive_name.to_string(),
                    )
                })?
                .to_string(),
        );

        let http = args
            .get(&name!("http"))
            .map(HTTPSourceAPI::from_directive)
            .transpose()?;

        Ok(Self {
            graph: graph.to_owned(),
            name,
            http,
        })
    }
}

#[derive(Clone, Debug, Serialize)]
pub(super) struct HTTPSourceAPI {
    pub(crate) base_url: String,
    pub(crate) default: bool,
    pub(crate) headers: Vec<HTTPHeaderMapping>,
}

impl HTTPSourceAPI {
    pub(super) fn from_directive(
        directive: &Node<apollo_compiler::ast::Value>,
    ) -> Result<Self, ConnectorDirectiveError> {
        let directive = directive
            .as_object()
            .ok_or_else(|| {
                ConnectorDirectiveError::InvalidTypeForAttribute(
                    "object".to_string(),
                    "http".to_string(),
                )
            })?
            .iter()
            .map(|(name, value)| (name.clone(), value.clone()))
            .collect::<HashMap<_, _>>();

        let base_url = directive
            .get(&name!("baseURL"))
            .ok_or_else(|| {
                ConnectorDirectiveError::MissingAttributeForType(
                    "baseURL".to_string(),
                    "http".to_string(),
                )
            })?
            .as_str()
            .ok_or_else(|| {
                ConnectorDirectiveError::InvalidTypeForAttribute(
                    "string".to_string(),
                    "baseURL".to_string(),
                )
            })?
            .to_string();

        let default = directive
            .get(&name!("default"))
            .and_then(|v| v.to_bool())
            .unwrap_or_default();

        let headers = directive
            .get(&name!("headers"))
            .map(HTTPHeaderMapping::from_header_arguments)
            .transpose()?
            .unwrap_or_default();

        Ok(Self {
            base_url,
            default,
            headers,
        })
    }
}

#[derive(Clone, Debug, Serialize)]
pub(super) struct HTTPHeaderMapping {
    pub(crate) name: String,
    pub(crate) r#as: Option<String>,
    pub(crate) value: Option<String>,
}

impl HTTPHeaderMapping {
    pub(super) fn from_header_arguments(
        argument: &Node<Value>,
    ) -> Result<Vec<Self>, ConnectorDirectiveError> {
        Ok(argument
            .as_list()
            .map(|arguments| {
                arguments
                    .iter()
                    .map(Self::from_value)
                    .collect::<Result<Vec<_>, _>>()
            })
            .transpose()?
            .unwrap_or_default())
    }
    fn from_value(argument: &Node<Value>) -> Result<Self, ConnectorDirectiveError> {
        let header_arguments = argument.as_object().ok_or_else(|| {
            ConnectorDirectiveError::InvalidTypeForAttribute(
                "Object".to_string(),
                "headers".to_string(),
            )
        })?;
        let mut name = None;
        let mut r#as = Default::default();
        let mut value = Default::default();

        for (node_name, arg) in header_arguments.iter() {
            let as_string = arg.as_str().map(|s| s.to_string());
            match node_name.as_str() {
                "name" => {
                    name = as_string;
                }
                "as" => r#as = as_string,
                "value" => value = as_string,
                other => {
                    return Err(ConnectorDirectiveError::UnknownAttributeForType(
                        other.to_string(),
                        "header".to_string(),
                    ));
                }
            }
        }

        Ok(Self {
            name: name.ok_or_else(|| {
                ConnectorDirectiveError::MissingAttributeForType(
                    "name".to_string(),
                    "header".to_string(),
                )
            })?,
            r#as,
            value,
        })
    }
}

// --- @sourceType -------------------------------------------------------------

#[derive(Debug, Serialize, Clone)]
pub(super) struct SourceType {
    pub(super) graph: Arc<String>,
    pub(super) type_name: Name,
    pub(super) api: String,
    pub(super) http: Option<HTTPSource>,
    pub(super) selection: JSONSelection,
    pub(super) key_type_map: Option<KeyTypeMap>,
    pub(super) is_interface_object: bool,
}

impl SourceType {
    fn from_schema_and_graph_names(
        schema: &Schema,
        graph_names: &HashMap<String, String>,
    ) -> Result<Vec<Self>, ConnectorDirectiveError> {
        let mut result: Vec<Self> = Vec::new();

        let source_type_directive_name = crate::spec::Schema::directive_name(
            schema,
            SOURCE_DIRECTIVE_URL,
            SOURCE_TYPE_DIRECTIVE_NAME,
            "sourceType",
        )
        .unwrap_or_else(|| SOURCE_TYPE_DIRECTIVE_NAME.to_string());

        for (type_name, ty) in &schema.types {
            let is_interface_object_map = Self::get_is_interface_object_map(ty);
            let joins = JoinedDirective::from_schema_directive_list(ty.directives())?;
            let source_types = joins
                .iter()
                .flat_map(|joined| {
                    if joined.directive.name == source_type_directive_name {
                        joined
                            .graphs
                            .iter()
                            .map(|graph| (graph, &joined.directive.args))
                            .collect::<Vec<_>>()
                    } else {
                        vec![]
                    }
                })
                .collect::<Vec<_>>();

            if !source_types.is_empty() {
                result.extend(
                    source_types
                        .into_iter()
                        .map(|(graph, args)| {
                            let graph_name = graph_names.get(graph).ok_or_else(|| {
                                ConnectorDirectiveError::InvalidJoinDirective(
                                    format!("Missing graph {} in join__Graph enum", graph)
                                        .to_string(),
                                )
                            })?;
                            let is_interface_object =
                                is_interface_object_map.get(graph).copied().unwrap_or(false);
                            Self::from_directive(
                                source_type_directive_name.as_ref(),
                                graph_name.clone(),
                                type_name.clone(),
                                args,
                                is_interface_object,
                            )
                        })
                        .collect::<Result<Vec<_>, _>>()?,
                );
            }
        }

        Ok(result)
    }

    fn get_is_interface_object_map(from_type: &ExtendedType) -> HashMap<String, bool> {
        let mut is_iface_obj_by_graph_name = HashMap::new();
        from_type
            .directives()
            .iter()
            .filter(|d| d.name == JOIN_TYPE_DIRECTIVE_NAME)
            .for_each(|d| {
                if let Some(graph_name) = d.argument_by_name("graph").and_then(|g| g.as_enum()) {
                    let is_interface_object = d
                        .argument_by_name("isInterfaceObject")
                        .and_then(|is_interface| is_interface.to_bool())
                        .unwrap_or(false);
                    is_iface_obj_by_graph_name.insert(graph_name.to_string(), is_interface_object);
                }
            });
        is_iface_obj_by_graph_name
    }

    fn from_directive(
        source_type_directive_name: &str,
        graph: String,
        type_name: Name,
        directive: &HashMap<Name, Node<apollo_compiler::ast::Value>>,
        is_interface_object: bool,
    ) -> Result<Self, ConnectorDirectiveError> {
        let api = directive
            .get(&name!("api"))
            .ok_or_else(|| {
                ConnectorDirectiveError::MissingAttributeForType(
                    "api".to_string(),
                    source_type_directive_name.to_string(),
                )
            })?
            .as_str()
            .ok_or_else(|| {
                ConnectorDirectiveError::InvalidTypeForAttribute(
                    "string".to_string(),
                    "api".to_string(),
                )
            })?
            .to_string();

        let selection = directive
            .get(&name!("selection"))
            .ok_or_else(|| {
                ConnectorDirectiveError::MissingAttributeForType(
                    "selection".to_string(),
                    source_type_directive_name.to_string(),
                )
            })?
            .as_str()
            .ok_or_else(|| {
                ConnectorDirectiveError::InvalidTypeForAttribute(
                    "string".to_string(),
                    "selection".to_string(),
                )
            })
            .map(|v| {
                JSONSelection::parse(v).map_err(|_| {
                    ConnectorDirectiveError::ParseError(
                        "Failed to parse selection".to_string(),
                        "selection".to_string(),
                    )
                })
            })??
            .1;

        let http = directive
            .get(&name!("http"))
            .map(HTTPSource::from_argument)
            .transpose()?;

        let key_type_map = directive
            .get(&name!("keyTypeMap"))
            .map(KeyTypeMap::from_argument)
            .transpose()?;

        Ok(Self {
            graph: Arc::new(graph),
            type_name,
            api,
            http,
            selection,
            key_type_map,
            is_interface_object,
        })
    }

    pub(super) fn api_name(&self) -> String {
        format!("{}_{}", self.graph, self.api)
    }
}

#[derive(Debug, Serialize, Clone)]
pub(super) struct KeyTypeMap {
    pub(super) key: String,
    // Dictionary mapping possible __typename strings to values of the JSON
    // property named by key.
    pub(super) type_map: HashMap<Name, String>,
}

impl KeyTypeMap {
    pub(super) fn from_argument(argument: &Node<Value>) -> Result<Self, ConnectorDirectiveError> {
        use ConnectorDirectiveError::*;

        let object = argument
            .as_object()
            .ok_or_else(|| InvalidTypeForAttribute("keyTypeMap".into(), "Object".into()))?
            .iter()
            .map(|(name, value)| (name.clone(), value.clone()))
            .collect::<HashMap<Name, Node<Value>>>();

        let key = object
            .get("key")
            .ok_or_else(|| MissingAttributeForType("key".into(), "keyTypeMap".into()))?
            .as_str()
            .ok_or_else(|| InvalidTypeForAttribute("key".into(), "String".into()))?
            .to_string();

        let type_map = object
            .get("typeMap")
            .ok_or_else(|| MissingAttributeForType("typeMap".into(), "keyTypeMap".into()))?
            .as_object()
            .ok_or_else(|| InvalidTypeForAttribute("typeMap".into(), "Object".into()))?
            .iter()
            .map(|(name, value)| {
                let value = value
                    .as_str()
                    .ok_or_else(|| InvalidTypeForAttribute(name.to_string(), "String".into()))?;
                Ok((name.clone(), value.to_string()))
            })
            .collect::<Result<HashMap<Name, String>, _>>()?;

        Ok(Self { key, type_map })
    }
}

// --- @sourceField ------------------------------------------------------------

#[derive(Debug, Serialize, Clone)]
pub(super) struct SourceField {
    pub(super) graph: Arc<String>,
    pub(super) parent_type_name: Name,
    pub(super) field_name: Name,
    pub(super) output_type_name: Name,
    pub(super) api: String,
    pub(super) http: Option<HTTPSource>,
    pub(super) selection: JSONSelection,
    pub(super) on_interface_object: bool,
}

impl SourceField {
    pub(super) fn from_schema_and_graph_names(
        schema: &Schema,
        graph_names: &HashMap<String, String>,
    ) -> Result<Vec<Self>, ConnectorDirectiveError> {
        let source_field_directive_name = crate::spec::Schema::directive_name(
            schema,
            SOURCE_DIRECTIVE_URL,
            SOURCE_FIELD_DIRECTIVE_NAME,
            "sourceField",
        )
        .unwrap_or_else(|| SOURCE_FIELD_DIRECTIVE_NAME.to_string());

        let mut source_fields = vec![];
        for (_, ty) in schema.types.iter() {
            source_fields.extend(Self::from_type(
                graph_names,
                ty,
                source_field_directive_name.as_ref(),
            )?);
        }
        Ok(source_fields)
    }

    fn from_type(
        graph_names: &HashMap<String, String>,
        ty: &ExtendedType,
        source_field_directive_name: &str,
    ) -> Result<Vec<Self>, ConnectorDirectiveError> {
        Ok(match ty {
            ExtendedType::Object(obj) => {
                Self::from_fields(graph_names, ty, &obj.fields, source_field_directive_name)?
            }
            ExtendedType::Interface(obj) => {
                Self::from_fields(graph_names, ty, &obj.fields, source_field_directive_name)?
            }
            _ => vec![],
        })
    }

    fn from_fields(
        graph_names: &HashMap<String, String>,
        parent_type: &ExtendedType,
        fields: &IndexMap<Name, Component<FieldDefinition>>,
        source_field_directive_name: &str,
    ) -> Result<Vec<Self>, ConnectorDirectiveError> {
        let mut result: Vec<Self> = vec![];

        let parent_type_name = parent_type.name().clone();

        let is_interface_object_map = SourceType::get_is_interface_object_map(parent_type);

        for (field_name, field_def) in fields {
            let joins = JoinedDirective::from_directive_list(&field_def.directives)?;

            let source_fields = joins
                .iter()
                .flat_map(|joined| {
                    if joined.directive.name == source_field_directive_name {
                        joined
                            .graphs
                            .iter()
                            .map(|graph| (graph, &joined.directive.args))
                            .collect::<Vec<_>>()
                    } else {
                        vec![]
                    }
                })
                .collect::<Vec<_>>();

            result.extend(
                source_fields
                    .into_iter()
                    .map(|(graph, args)| {
                        let graph_name = graph_names.get(graph).ok_or_else(|| {
                            ConnectorDirectiveError::InvalidJoinDirective(
                                format!("Missing graph {} in join__Graph enum", graph).to_string(),
                            )
                        })?;

                        let on_interface_object =
                            is_interface_object_map.get(graph).copied().unwrap_or(false);

                        Self::from_directive(
                            source_field_directive_name,
                            graph_name.clone(),
                            parent_type_name.clone(),
                            field_name.clone(),
                            field_def.ty.inner_named_type().clone(),
                            args,
                            on_interface_object,
                        )
                    })
                    .collect::<Result<Vec<_>, _>>()?,
            );
        }

        Ok(result)
    }

    fn from_directive(
        source_field_directive_name: &str,
        graph: String,
        parent_type_name: Name,
        field_name: Name,
        output_type_name: Name,
        directive: &HashMap<Name, Node<apollo_compiler::ast::Value>>,
        on_interface_object: bool,
    ) -> Result<Self, ConnectorDirectiveError> {
        let api = directive
            .get(&name!("api"))
            .ok_or_else(|| {
                ConnectorDirectiveError::MissingAttributeForType(
                    "api".to_string(),
                    source_field_directive_name.to_string(),
                )
            })?
            .as_str()
            .ok_or_else(|| {
                ConnectorDirectiveError::InvalidTypeForAttribute(
                    "string".to_string(),
                    "api".to_string(),
                )
            })?
            .to_string();

        let selection = directive
            .get(&name!("selection"))
            .ok_or_else(|| {
                ConnectorDirectiveError::MissingAttributeForType(
                    "selection".to_string(),
                    source_field_directive_name.to_string(),
                )
            })?
            .as_str()
            .ok_or_else(|| {
                ConnectorDirectiveError::InvalidTypeForAttribute(
                    "string".to_string(),
                    "selection".to_string(),
                )
            })
            .map(|v| {
                JSONSelection::parse(v).map_err(|_| {
                    ConnectorDirectiveError::ParseError(
                        "Failed to parse selection".to_string(),
                        "selection".to_string(),
                    )
                })
            })??
            .1;

        let http = directive
            .get(&name!("http"))
            .map(HTTPSource::from_argument)
            .transpose()?;

        Ok(Self {
            graph: Arc::new(graph),
            parent_type_name,
            field_name,
            output_type_name,
            api,
            http,
            selection,
            on_interface_object,
        })
    }

    pub(super) fn api_name(&self) -> String {
        format!("{}_{}", self.graph, self.api)
    }
}

#[derive(Debug, Serialize, Clone)]
pub(super) struct HTTPSource {
    pub(super) path_template: URLPathTemplate,
    #[serde(with = "http_serde::method")]
    pub(super) method: http::Method,
    pub(super) headers: Vec<HTTPHeaderMapping>,
    pub(super) body: Option<JSONSelection>,
}

impl HTTPSource {
    fn from_argument(argument: &Node<Value>) -> Result<Self, ConnectorDirectiveError> {
        let argument = argument.as_object().ok_or_else(|| {
            ConnectorDirectiveError::InvalidTypeForAttribute(
                "object".to_string(),
                HTTP_ARGUMENT_NAME.to_string(),
            )
        })?;

        let mut headers = Default::default();
        let mut body = Default::default();
        let mut path_template_and_method: Option<(URLPathTemplate, Method)> = Default::default();
        for (name, value) in argument {
            match name.as_str() {
                "headers" => {
                    headers = HTTPHeaderMapping::from_header_arguments(value)?;
                }
                "body" => {
                    let v = value.as_str().ok_or_else(|| {
                        ConnectorDirectiveError::InvalidTypeForAttribute(
                            "string".to_string(),
                            "body".to_string(),
                        )
                    })?;
                    body = Some(
                        JSONSelection::parse(v)
                            .map_err(|_| {
                                ConnectorDirectiveError::ParseError(
                                    "Failed to parse selection".to_string(),
                                    "body".to_string(),
                                )
                            })?
                            .1,
                    )
                }
                // there should only be one more argument, the method.
                _ => {
                    if path_template_and_method.is_some() {
                        Err(ConnectorDirectiveError::RequiresExactlyOne(
                            "GET, PATCH, POST, PUT, DELETE".to_string(),
                            "HTTPSourceField".to_string(),
                        ))?;
                    }
                    path_template_and_method = Some(
                        http::Method::from_bytes(name.as_bytes())
                            .map_err(|_| {
                                ConnectorDirectiveError::UnknownAttributeForType(
                                    "HTTPSourceField".to_string(),
                                    name.to_string(),
                                )
                            })
                            .and_then(|method| {
                                URLPathTemplate::parse(value.as_str().ok_or_else(|| {
                                    ConnectorDirectiveError::InvalidTypeForAttribute(
                                        method.to_string(),
                                        "String".to_string(),
                                    )
                                })?)
                                .map(|t| (t, method.clone()))
                                .map_err(|e| {
                                    ConnectorDirectiveError::ParseError(method.to_string(), e)
                                })
                            })?,
                    )
                }
            }
        }

        let (path_template, method) = path_template_and_method.ok_or_else(|| {
            ConnectorDirectiveError::RequiresExactlyOne(
                "GET, PATCH, POST, PUT, DELETE".to_string(),
                "HTTPSourceField".to_string(),
            )
        })?;

        Ok(Self {
            path_template,
            method,
            headers,
            body,
        })
    }
}

#[cfg(test)]
mod tests {
    use insta::assert_json_snapshot;

    use super::*;

    #[test]
    fn test_source_api_directive_has_no_errors() {
        let partial_sdl = r#"
            enum join__Graph {
              CONTACTS @join__graph(name: "contacts")
            }

            schema
              @join__directive(
                graphs: [CONTACTS]
                name: "sourceAPI"
                args: {
                  name: "rest_contacts"
                  http: {
                    baseURL: "http://localhost:4002/contacts/"
                    default: true
                    headers: [
                      { name: "x-test", value: "test1234" }
                      { name: "x-before-rename-test", as: "x-after-rename-test" }
                      {
                        name: "x-before-rename-and-with-value-test",
                        as: "x-after-rename-and-with-value-test",
                        value: "test5678"
                      }
                    ]
                  }
                }
              )
              @join__directive(
                graphs: [CONTACTS]
                name: "sourceAPI"
                args: {
                  name: "rest_notes"
                  http: { baseURL: "http://localhost:4002/notes/" }
                }
              )
              @join__directive(
                graphs: [CONTACTS]
                name: "sourceAPI"
                args: {
                  name: "legacy_contacts"
                  http: { baseURL: "http://localhost:4002/legacy/contacts/" }
                }
              )
            {
                query: Query
            }
            "#;
        let partial_schema = apollo_compiler::Schema::parse(partial_sdl, "schema.graphql").unwrap();
        let graph_names = graph_enum_map(&partial_schema)
            .ok_or_else(|| {
                ConnectorDirectiveError::InvalidJoinDirective(
                    "Missing join__Graph enum".to_string(),
                )
            })
            .unwrap();
        let all_source_apis =
            SourceAPI::from_schema_and_graph_names(&partial_schema, &graph_names).unwrap();

        insta::with_settings!({sort_maps => true}, {
            assert_json_snapshot!(all_source_apis);
        });
    }

    #[test]
    fn test_source_api_directive_missing_mandatory_fields() {
        let partial_sdl = r#"
            enum join__Graph {
              CONTACTS @join__graph(name: "contacts")
            }

            schema
              @join__directive(
                graphs: [CONTACTS]
                name: "sourceAPI"
                args: {
                  http: {
                    baseURL: "http://localhost:4002/contacts/"
                  }
                }
              )
            { query: Query }
            "#;

        let partial_schema = apollo_compiler::Schema::parse(partial_sdl, "schema.graphql").unwrap();
        let graph_names = graph_enum_map(&partial_schema)
            .ok_or_else(|| {
                ConnectorDirectiveError::InvalidJoinDirective(
                    "Missing join__Graph enum".to_string(),
                )
            })
            .unwrap();
        let missing_name_error =
            SourceAPI::from_schema_and_graph_names(&partial_schema, &graph_names).unwrap_err();
        assert_eq!(
            ConnectorDirectiveError::MissingAttributeForType(
                "name".to_string(),
                "sourceAPI".to_string()
            ),
            missing_name_error
        );

        let partial_sdl = r#"
            enum join__Graph {
              CONTACTS @join__graph(name: "contacts")
            }

            schema
              @join__directive(
                graphs: [CONTACTS]
                name: "sourceAPI"
                args: {
                  name: "missing_base_url"
                  http: {
                      default: true
                  }
                }
              )
            { query: Query }
            "#;

        let partial_schema = apollo_compiler::Schema::parse(partial_sdl, "schema.graphql").unwrap();
        let graph_names = graph_enum_map(&partial_schema)
            .ok_or_else(|| {
                ConnectorDirectiveError::InvalidJoinDirective(
                    "Missing join__Graph enum".to_string(),
                )
            })
            .unwrap();
        let missing_base_url_error =
            SourceAPI::from_schema_and_graph_names(&partial_schema, &graph_names).unwrap_err();
        assert_eq!(
            ConnectorDirectiveError::MissingAttributeForType(
                "baseURL".to_string(),
                "http".to_string()
            ),
            missing_base_url_error
        );

        let partial_sdl = r#"
            enum join__Graph {
              CONTACTS @join__graph(name: "contacts")
            }

            schema
              @join__directive(
                graphs: [CONTACTS]
                name: "sourceAPI"
                args: {
                  name: "missing_header_name"
                  http: {
                      baseURL: "http://localhost:4002/contacts/"
                      headers: [{ as: "missing mandatory name field" }]
                  }
                }
              )
            { query: Query }
            "#;

        let partial_schema = apollo_compiler::Schema::parse(partial_sdl, "schema.graphql").unwrap();
        let graph_names = graph_enum_map(&partial_schema)
            .ok_or_else(|| {
                ConnectorDirectiveError::InvalidJoinDirective(
                    "Missing join__Graph enum".to_string(),
                )
            })
            .unwrap();
        let missing_header_name_error =
            SourceAPI::from_schema_and_graph_names(&partial_schema, &graph_names).unwrap_err();
        assert_eq!(
            ConnectorDirectiveError::MissingAttributeForType(
                "name".to_string(),
                "header".to_string()
            ),
            missing_header_name_error
        );
    }

    #[test]
    fn test_valid_source_types() {
        let partial_sdl = r#"
        enum join__Graph {
          CONTACTS @join__graph(name: "contacts")
        }

        type ValidSourceType
          @join__type(graph: CONTACTS, key: "contactId")
          @join__directive(
            graphs: [CONTACTS]
            name: "sourceType"
            args: { api: "contacts", http: { GET: "/contacts/{contactId}" }, selection: "a" }
          )
        {
            id: ID!
            name: String
        }

        type ValidSourceTypeDefaultHttp
          @join__type(graph: CONTACTS, key: "contactId")
          @join__directive(
            graphs: [CONTACTS]
            name: "sourceType"
            args: { api: "contacts", http: { GET: "/contacts/{contactId}" }, selection: "a" }
          )
        {
            id: ID!
            name: String
        }
        "#;

        let partial_schema = apollo_compiler::Schema::parse(partial_sdl, "schema.graphql").unwrap();
        let graph_names = graph_enum_map(&partial_schema)
            .ok_or_else(|| {
                ConnectorDirectiveError::InvalidJoinDirective(
                    "Missing join__Graph enum".to_string(),
                )
            })
            .unwrap();
        let source_types =
            SourceType::from_schema_and_graph_names(&partial_schema, &graph_names).unwrap();

        insta::with_settings!({sort_maps => true}, {
            assert_json_snapshot!(source_types);
        });
    }

    #[test]
    fn test_valid_source_field() {
        let partial_sdl = r#"
        enum join__Graph {
          CONTACTS @join__graph(name: "contacts")
        }

        type Query {
          field: String
            @join__directive(
              graphs: [CONTACTS]
              name: "sourceField"
              args: {
                api: "contacts"
                http: { GET: "/contacts/{contactId}" }
                selection: "id name"
              }
            )
        }
        "#;

        let partial_schema = apollo_compiler::Schema::parse(partial_sdl, "schema.graphql").unwrap();
        let graph_names = graph_enum_map(&partial_schema)
            .ok_or_else(|| {
                ConnectorDirectiveError::InvalidJoinDirective(
                    "Missing join__Graph enum".to_string(),
                )
            })
            .unwrap();
        let source_fields =
            SourceField::from_schema_and_graph_names(&partial_schema, &graph_names).unwrap();

        insta::with_settings!({sort_maps => true}, {
            assert_json_snapshot!(source_fields, @r###"
            [
              {
                "graph": "contacts",
                "parent_type_name": "Query",
                "field_name": "field",
                "output_type_name": "String",
                "api": "contacts",
                "http": {
                  "path_template": "/contacts/{contactId!}",
                  "method": "GET",
                  "headers": [],
                  "body": null
                },
                "selection": {
                  "Named": {
                    "selections": [
                      {
                        "Field": [
                          null,
                          "id",
                          null
                        ]
                      },
                      {
                        "Field": [
                          null,
                          "name",
                          null
                        ]
                      }
                    ],
                    "star": null
                  }
                },
                "on_interface_object": false
              }
            ]
            "###);
        });

        let partial_sdl = r#"
        enum join__Graph {
          CONTACTS @join__graph(name: "contacts")
        }

        type Query {
          field: String
            @join__directive(
              graphs: [CONTACTS]
              name: "sourceField"
              args: {
                api: "contacts"
                http: { body: "id name" }
                selection: "id name"
              }
            )
        }
        "#;

        let partial_schema = apollo_compiler::Schema::parse(partial_sdl, "schema.graphql").unwrap();
        let graph_names = graph_enum_map(&partial_schema)
            .ok_or_else(|| {
                ConnectorDirectiveError::InvalidJoinDirective(
                    "Missing join__Graph enum".to_string(),
                )
            })
            .unwrap();
        assert_eq!(
            SourceField::from_schema_and_graph_names(&partial_schema, &graph_names).unwrap_err(),
            ConnectorDirectiveError::RequiresExactlyOne(
                "GET, PATCH, POST, PUT, DELETE".to_string(),
                "HTTPSourceField".to_string(),
            ),
        );
    }
}
