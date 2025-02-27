use apollo_compiler::collections::IndexMap;
use apollo_compiler::Schema;
use itertools::Itertools;
use thiserror::Error;
use url::Url;

use super::ConnectSpec;
use super::Connector;

#[derive(Debug)]
pub enum BaseUrl {
    Static(Url),
    Dynamic(String),
}

#[derive(Debug, Error)]
pub enum BaseUrlError {
    #[error("Invalid schema: {0}")]
    ParseError(String),

    #[error("Could not find connectors: {0}")]
    ConnectorError(String),
}

pub fn base_urls_for_connectors(
    sdl: &str,
    subgraph_name: &str,
) -> Result<IndexMap<String, BaseUrl>, BaseUrlError> {
    let schema = Schema::builder()
        .adopt_orphan_extensions()
        .parse(sdl, "schema.graphql")
        .build()
        .map_err(|e| BaseUrlError::ParseError(e.errors.iter().join("\n")))?;

    let Some((spec, _)) = ConnectSpec::get_from_schema(&schema) else {
        return Ok(Default::default());
    };

    let connectors = Connector::from_schema(&schema, subgraph_name, spec)
        .map_err(|e| BaseUrlError::ConnectorError(e.to_string()))?;

    let mut base_urls = IndexMap::default();

    for (_, connector) in connectors {
        if let (Some(base_url), Some(source_name)) =
            (connector.transport.source_url, connector.id.source_name)
        {
            // TODO: dynamic url strings when supported
            base_urls.insert(source_name, BaseUrl::Static(base_url));
        }
    }

    Ok(base_urls)
}

#[cfg(test)]
mod tests {
    use insta::assert_debug_snapshot;

    #[test]
    fn test_base_urls_for_connectors() {
        let sdl = r#"
        extend schema
          @link(url: "https://specs.apollo.dev/federation/v2.10")
          @link(
            url: "https://specs.apollo.dev/connect/v0.1"
            import: ["@connect", "@source"]
          )
          @source(
            name: "one"
            http: { baseURL: "https://jsonplaceholder.typicode.com/" }
          )
          @source(
            name: "two"
            http: { baseURL: "https://api.stripe.com/v1/" }
          )

        type Query {
          a: String @connect(source: "one", http: { GET: "/" }, selection: "$")
          b: String @connect(source: "one", http: { GET: "/" }, selection: "$")
          c: String @connect(source: "two", http: { GET: "/" }, selection: "$")
          d: String @connect(http: { GET: "https://localhost/" }, selection: "$")
        }
    "#;

        let base_urls = super::base_urls_for_connectors(sdl, "test").unwrap();

        assert_debug_snapshot!(base_urls, @r#"
        {
            "one": Static(
                Url {
                    scheme: "https",
                    cannot_be_a_base: false,
                    username: "",
                    password: None,
                    host: Some(
                        Domain(
                            "jsonplaceholder.typicode.com",
                        ),
                    ),
                    port: None,
                    path: "/",
                    query: None,
                    fragment: None,
                },
            ),
            "two": Static(
                Url {
                    scheme: "https",
                    cannot_be_a_base: false,
                    username: "",
                    password: None,
                    host: Some(
                        Domain(
                            "api.stripe.com",
                        ),
                    ),
                    port: None,
                    path: "/v1/",
                    query: None,
                    fragment: None,
                },
            ),
        }
        "#);
    }
}
