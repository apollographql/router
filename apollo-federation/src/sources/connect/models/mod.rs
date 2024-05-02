mod validation;

use apollo_compiler::NodeStr;
use indexmap::IndexMap;

use crate::{
    error::FederationError,
    schema::ValidFederationSchema,
    sources::connect::{
        spec::{extract_connect_directive_arguments, extract_source_directive_arguments},
        ConnectSpecDefinition,
    },
};

use super::{
    spec::{ConnectHTTPArguments, HTTPHeaderOption, SourceHTTPArguments},
    ConnectId, Selection, URLPathTemplate,
};

// --- Connector ---------------------------------------------------------------

#[cfg_attr(test, derive(Debug))]
pub(crate) struct Connector {
    pub(crate) id: ConnectId,
    transport: Transport,
    pub(crate) selection: Selection,
}

#[cfg_attr(test, derive(Debug))]
enum Transport {
    HttpJson(HttpJsonTransport),
}

impl Connector {
    pub(crate) fn from_valid_schema(
        schema: &ValidFederationSchema,
        subgraph_name: NodeStr,
    ) -> Result<IndexMap<ConnectId, Self>, FederationError> {
        let Some(metadata) = schema.metadata() else {
            return Ok(IndexMap::new());
        };

        let Some(link) = metadata.for_identity(&ConnectSpecDefinition::identity()) else {
            return Ok(IndexMap::new());
        };

        let source_name = ConnectSpecDefinition::source_directive_name(&link);
        let source_arguments = extract_source_directive_arguments(schema, &source_name)?;

        let connect_name = ConnectSpecDefinition::connect_directive_name(&link);
        let connect_arguments = extract_connect_directive_arguments(schema, &connect_name)?;

        connect_arguments
            .into_iter()
            .map(move |args| {
                let id = ConnectId {
                    subgraph_name: subgraph_name.clone(),
                    directive: args.position,
                };

                let source = if let Some(source_name) = args.source {
                    source_arguments
                        .iter()
                        .find(|source| source.name == source_name)
                } else {
                    None
                };

                let connect_http = args.http.expect("@connect http missing");
                let source_http = source.map(|s| &s.http);

                let transport = Transport::HttpJson(HttpJsonTransport::from_directive(
                    &connect_http,
                    source_http,
                )?);

                Ok((
                    id.clone(),
                    Connector {
                        id,
                        transport,
                        selection: args.selection,
                    },
                ))
            })
            .collect()
    }
}

// --- HTTP JSON ---------------------------------------------------------------

#[cfg_attr(test, derive(Debug))]
struct HttpJsonTransport {
    base_url: NodeStr,
    path_template: URLPathTemplate,
    method: HTTPMethod,
    headers: IndexMap<NodeStr, Option<HTTPHeaderOption>>,
    body: Option<Selection>,
}

impl HttpJsonTransport {
    fn from_directive(
        http: &ConnectHTTPArguments,
        source: Option<&SourceHTTPArguments>,
    ) -> Result<Self, FederationError> {
        let (method, path) = if let Some(path) = &http.get {
            (HTTPMethod::Get, path)
        } else if let Some(path) = &http.post {
            (HTTPMethod::Post, path)
        } else if let Some(path) = &http.patch {
            (HTTPMethod::Patch, path)
        } else if let Some(path) = &http.put {
            (HTTPMethod::Put, path)
        } else if let Some(path) = &http.delete {
            (HTTPMethod::Delete, path)
        } else {
            return Err(FederationError::internal("missing http method"));
        };

        let base_url = if let Some(base_url) = source.as_ref().map(|source| source.base_url.clone())
        {
            base_url
        } else {
            todo!("get base url from path")
        };

        let mut headers = source
            .as_ref()
            .map(|source| source.headers.0.clone())
            .unwrap_or_default();
        headers.extend(http.headers.0.clone());

        Ok(Self {
            base_url,
            path_template: URLPathTemplate::parse(path).expect("path template"),
            method,
            headers,
            body: http.body.clone(),
        })
    }
}

/// The HTTP arguments needed for a connect request
#[cfg_attr(test, derive(Debug))]
pub(crate) enum HTTPMethod {
    Get,
    Post,
    Patch,
    Put,
    Delete,
}

#[cfg(test)]
mod tests {
    use apollo_compiler::Schema;

    use crate::{
        query_graph::extract_subgraphs_from_supergraph::extract_subgraphs_from_supergraph,
        schema::FederationSchema, ValidFederationSubgraphs,
    };

    use super::*;

    static SIMPLE_SUPERGRAPH: &str = include_str!("../tests/schemas/simple.graphql");

    fn get_subgraphs(supergraph_sdl: &str) -> ValidFederationSubgraphs {
        let schema = Schema::parse(supergraph_sdl, "supergraph.graphql").unwrap();
        let supergraph_schema = FederationSchema::new(schema).unwrap();
        extract_subgraphs_from_supergraph(&supergraph_schema, Some(true)).unwrap()
    }

    #[test]
    fn test_from_schema() {
        let subgraphs = get_subgraphs(SIMPLE_SUPERGRAPH);
        let subgraph = subgraphs.get("connectors").unwrap();
        let connectors =
            Connector::from_valid_schema(&subgraph.schema, "connectors".into()).unwrap();
        dbg!(&connectors);
    }
}
