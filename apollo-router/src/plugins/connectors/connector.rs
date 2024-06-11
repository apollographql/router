use std::collections::HashMap;
use std::collections::HashSet;
use std::sync::Arc;

use apollo_compiler::ast::Selection as GraphQLSelection;
use apollo_compiler::schema::Name;

use super::directives::KeyTypeMap;
use super::directives::SourceAPI;
use super::directives::SourceField;
use super::directives::SourceType;
use super::finder_fields::finder_field_for_connector;
use super::http_json_transport::HttpJsonTransport;
use crate::error::ConnectorDirectiveError;

/// A connector wraps the API and type/field connector metadata and has
/// a unique name used to construct a "subgraph" in the inner supergraph
/// TODO: make it more clone friendly
#[derive(Clone, Debug)]
pub(crate) struct Connector {
    /// Internal name used to construct "subgraphs" in the inner supergraph
    pub(super) name: Arc<String>,
    /// The api name, as defined in the `sourceAPI` directive
    pub(super) api: String,
    pub(crate) origin_subgraph: Arc<String>,
    pub(super) kind: ConnectorKind,
    pub(super) transport: ConnectorTransport,
    pub(super) output_selection: Vec<GraphQLSelection>,
    pub(super) input_selection: Vec<GraphQLSelection>,
    pub(super) key_type_map: Option<KeyTypeMap>,
}

#[derive(Clone, Debug)]
pub(super) enum ConnectorKind {
    RootField {
        parent_type_name: Name,
        field_name: Name,
        output_type_name: Name,
    },
    Entity {
        type_name: Name,
        key: String,
        is_interface_object: bool,
    },
    EntityField {
        type_name: Name,
        field_name: Name,
        output_type_name: Name,
        key: String,
        on_interface_object: bool,
    },
}

#[derive(Clone, Debug)]
pub(super) enum ConnectorTransport {
    HttpJson(HttpJsonTransport),
}

impl ConnectorTransport {
    pub(super) fn source_api_name(&self) -> Arc<String> {
        match self {
            Self::HttpJson(transport) => transport.source_api_name.clone(),
        }
    }

    pub(super) fn debug_name(&self) -> String {
        match self {
            Self::HttpJson(transport) => transport.debug_name().clone(),
        }
    }
}

/// The list of the subgraph names that should use the inner query planner
/// instead of making a normal subgraph request.
pub(crate) fn connector_subgraph_names(
    connectors: &HashMap<Arc<String>, Connector>,
) -> HashSet<Arc<String>> {
    connectors
        .values()
        .map(|c| c.origin_subgraph.clone())
        .collect()
}

impl Connector {
    pub(super) fn new_from_source_type(
        name: Arc<String>,
        api: SourceAPI,
        directive: SourceType,
    ) -> Result<Self, ConnectorDirectiveError> {
        let (input_selection, key, transport) = if let Some(http) = &directive.http {
            let (input_selection, key_string) =
                HttpJsonTransport::input_selection_from_http_source(http);

            let transport = ConnectorTransport::HttpJson(HttpJsonTransport::from_source_type(
                &api, &directive,
            )?);

            (input_selection, key_string, transport)
        } else {
            return Err(ConnectorDirectiveError::UknownSourceAPIKind);
        };

        let output_selection = directive.selection.clone().into();

        let kind = ConnectorKind::Entity {
            type_name: directive.type_name.clone(),
            key,
            is_interface_object: directive.is_interface_object,
        };

        Ok(Connector {
            name,
            api: directive.api.clone(),
            origin_subgraph: Arc::clone(&directive.graph),
            kind,
            transport,
            output_selection,
            input_selection,
            key_type_map: directive.key_type_map,
        })
    }

    pub(super) fn new_from_source_field(
        name: Arc<String>,
        api: &SourceAPI,
        directive: SourceField,
    ) -> Result<Self, ConnectorDirectiveError> {
        let (input_selection, key, transport) = if let Some(http) = &directive.http {
            let (input_selection, key_string) =
                HttpJsonTransport::input_selection_from_http_source(http);

            let transport = ConnectorTransport::HttpJson(
                HttpJsonTransport::from_source_field(api, &directive)
                    .map_err(std::convert::Into::into)?,
            );

            (input_selection, key_string, transport)
        } else {
            return Err(ConnectorDirectiveError::UknownSourceAPIKind);
        };

        let output_selection = directive.selection.clone().into();

        let kind =
            if directive.parent_type_name == "Query" || directive.parent_type_name == "Mutation" {
                ConnectorKind::RootField {
                    parent_type_name: directive.parent_type_name.clone(),
                    field_name: directive.field_name.clone(),
                    output_type_name: directive.output_type_name.clone(),
                }
            } else {
                ConnectorKind::EntityField {
                    type_name: directive.parent_type_name.clone(),
                    field_name: directive.field_name.clone(),
                    output_type_name: directive.output_type_name.clone(),
                    key,
                    on_interface_object: directive.on_interface_object,
                }
            };

        Ok(Connector {
            name,
            api: directive.api.clone(),
            origin_subgraph: Arc::clone(&directive.graph),
            kind,
            transport,
            output_selection,
            input_selection,
            key_type_map: None,
        })
    }

    pub(super) fn finder_field_name(&self) -> Option<serde_json_bytes::ByteString> {
        finder_field_for_connector(self).map(|f| serde_json_bytes::ByteString::from(f.as_str()))
    }

    pub(crate) fn override_base_url(&mut self, url: url::Url) {
        match &mut self.transport {
            ConnectorTransport::HttpJson(transport) => transport.base_uri = url,
        }
    }

    pub(super) fn schema_coordinate(&self) -> String {
        match &self.kind {
            ConnectorKind::RootField {
                parent_type_name,
                field_name,
                ..
            } => format!("{}.{}", parent_type_name, field_name),
            ConnectorKind::Entity { type_name, .. } => type_name.to_string(),
            ConnectorKind::EntityField {
                type_name,
                field_name,
                ..
            } => format!("{}.{}", type_name, field_name),
        }
    }

    pub(super) fn debug_name(&self) -> String {
        match &self.kind {
            ConnectorKind::RootField { .. } => format!(
                "[{}] {} @sourceField(api: {}, {})",
                self.origin_subgraph,
                self.schema_coordinate(),
                self.api,
                self.transport.debug_name(),
            ),
            ConnectorKind::Entity { .. } => format!(
                "[{}] {} @sourceType(api: {}, {})",
                self.origin_subgraph,
                self.schema_coordinate(),
                self.api,
                self.transport.debug_name(),
            ),
            ConnectorKind::EntityField { .. } => format!(
                "[{}] {} @sourceField(api: {}, {})",
                self.origin_subgraph,
                self.schema_coordinate(),
                self.api,
                self.transport.debug_name(),
            ),
        }
    }

    pub(crate) fn display_name(&self) -> String {
        let ConnectorTransport::HttpJson(tr) = &self.transport;
        format!(
            "{}.{}: {} {}",
            self.origin_subgraph, self.api, tr.method, tr.path_template
        )
    }

    pub(crate) fn _name(&self) -> Arc<String> {
        self.name.clone()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use apollo_compiler::name;
    use apollo_compiler::Schema;
    use apollo_federation::sources::connect::JSONSelection;
    use apollo_federation::sources::connect::URLPathTemplate;

    use super::*;
    use crate::plugins::connectors::directives::HTTPSource;
    use crate::plugins::connectors::directives::HTTPSourceAPI;
    use crate::plugins::connectors::request_response::make_requests;
    use crate::services::subgraph;

    #[test]
    fn request() {
        let subgraph_request = subgraph::Request::fake_builder()
            .subgraph_request(
                http::Request::builder()
                    .body(crate::graphql::Request::builder().query("{field}").build())
                    .unwrap(),
            )
            .build();

        let api = SourceAPI {
            graph: "B".to_string(),
            name: Arc::new("API".to_string()),
            http: Some(HTTPSourceAPI {
                base_url: "http://localhost/api".to_string(),
                default: true,
                headers: vec![],
            }),
        };

        let directive = SourceField {
            graph: Arc::new("B".to_string()),
            parent_type_name: name!(Query),
            field_name: name!(field),
            output_type_name: name!(String),
            api: "API".to_string(),
            http: Some(HTTPSource {
                method: http::Method::GET,
                path_template: URLPathTemplate::parse("/path").unwrap(),
                body: None,
                headers: vec![],
            }),
            selection: JSONSelection::parse(".data").unwrap().1,
            on_interface_object: false,
        };

        let connector = Connector::new_from_source_field(
            Arc::new("CONNECTOR_QUERY_FIELDB".to_string()),
            &api,
            directive,
        )
        .unwrap();

        let schema = Arc::new(
            Schema::parse_and_validate(
                "type Query { field: String }".to_string(),
                "schema.graphql",
            )
            .unwrap(),
        );

        let requests_and_params =
            make_requests(subgraph_request, &connector, schema.clone()).unwrap();
        insta::assert_debug_snapshot!(requests_and_params);
    }
}
