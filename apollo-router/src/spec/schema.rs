//! GraphQL schema.

use std::collections::HashMap;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Instant;

use apollo_compiler::ast;
use apollo_compiler::schema::Implementers;
use apollo_compiler::validation::Valid;
use http::Uri;
use semver::Version;
use semver::VersionReq;
use sha2::Digest;
use sha2::Sha256;

use crate::configuration::ApiSchemaMode;
use crate::configuration::QueryPlannerMode;
use crate::error::ParseErrors;
use crate::error::SchemaError;
use crate::query_planner::OperationKind;
use crate::Configuration;

/// A GraphQL schema.
pub(crate) struct Schema {
    pub(crate) raw_sdl: Arc<String>,
    supergraph: Supergraph,
    subgraphs: HashMap<String, Uri>,
    pub(crate) implementers_map: HashMap<ast::Name, Implementers>,
    api_schema: Option<ApiSchema>,
}

/// TODO: remove and use apollo_federation::Supergraph unconditionally
/// when we’re more confident in its constructor
enum Supergraph {
    ApolloFederation(apollo_federation::Supergraph),
    ApolloCompiler(Valid<apollo_compiler::Schema>),
}

/// Wrapper type to distinguish from `Schema::definitions` for the supergraph schema
#[derive(Debug)]
pub(crate) struct ApiSchema(pub(crate) Valid<apollo_compiler::Schema>);

impl Schema {
    #[cfg(test)]
    pub(crate) fn parse_test(s: &str, configuration: &Configuration) -> Result<Self, SchemaError> {
        let schema = Self::parse(s, configuration)?;
        let api_schema = Self::parse_compiler_schema(&schema.create_api_schema(configuration)?)?;
        Ok(schema.with_api_schema(api_schema))
    }

    pub(crate) fn parse_ast(sdl: &str) -> Result<ast::Document, SchemaError> {
        let mut parser = apollo_compiler::Parser::new();
        let result = parser.parse_ast(sdl, "schema.graphql");

        // Trace log recursion limit data
        let recursion_limit = parser.recursion_reached();
        tracing::trace!(?recursion_limit, "recursion limit data");

        result.map_err(|invalid| {
            SchemaError::Parse(ParseErrors {
                errors: invalid.errors,
            })
        })
    }

    pub(crate) fn parse_compiler_schema(
        sdl: &str,
    ) -> Result<Valid<apollo_compiler::Schema>, SchemaError> {
        Self::parse_ast(sdl)?
            .to_schema_validate()
            .map_err(|errors| SchemaError::Validate(errors.into()))
    }

    pub(crate) fn parse(sdl: &str, config: &Configuration) -> Result<Self, SchemaError> {
        let start = Instant::now();
        let definitions = Self::parse_compiler_schema(sdl)?;

        let mut subgraphs = HashMap::new();
        // TODO: error if not found?
        if let Some(join_enum) = definitions.get_enum("join__Graph") {
            for (name, url) in join_enum.values.iter().filter_map(|(_name, value)| {
                let join_directive = value.directives.get("join__graph")?;
                let name = join_directive.argument_by_name("name")?.as_str()?;
                let url = join_directive.argument_by_name("url")?.as_str()?;
                Some((name, url))
            }) {
                if url.is_empty() {
                    return Err(SchemaError::MissingSubgraphUrl(name.to_string()));
                }
                #[cfg(unix)]
                // there is no standard for unix socket URLs apparently
                let url = if let Some(path) = url.strip_prefix("unix://") {
                    // there is no specified format for unix socket URLs (cf https://github.com/whatwg/url/issues/577)
                    // so a unix:// URL will not be parsed by http::Uri
                    // To fix that, hyperlocal came up with its own Uri type that can be converted to http::Uri.
                    // It hides the socket path in a hex encoded authority that the unix socket connector will
                    // know how to decode
                    hyperlocal::Uri::new(path, "/").into()
                } else {
                    Uri::from_str(url)
                        .map_err(|err| SchemaError::UrlParse(name.to_string(), err))?
                };
                #[cfg(not(unix))]
                let url = Uri::from_str(url)
                    .map_err(|err| SchemaError::UrlParse(name.to_string(), err))?;

                if subgraphs.insert(name.to_string(), url).is_some() {
                    return Err(SchemaError::Api(format!(
                        "must not have several subgraphs with same name '{name}'"
                    )));
                }
            }
        }

        f64_histogram!(
            "apollo.router.schema.load.duration",
            "Time spent loading the supergraph schema.",
            start.elapsed().as_secs_f64()
        );

        let implementers_map = definitions.implementers_map();
        let legacy_only = config.experimental_query_planner_mode == QueryPlannerMode::Legacy
            && config.experimental_api_schema_generation_mode == ApiSchemaMode::Legacy;
        let supergraph = if cfg!(test) || !legacy_only {
            Supergraph::ApolloFederation(apollo_federation::Supergraph::from_schema(definitions)?)
        } else {
            Supergraph::ApolloCompiler(definitions)
        };

        Ok(Schema {
            raw_sdl: Arc::new(sdl.to_owned()),
            supergraph,
            subgraphs,
            implementers_map,
            api_schema: None,
        })
    }

    pub(crate) fn federation_supergraph(&self) -> &apollo_federation::Supergraph {
        // This is only called in cases wher we create ApolloFederation above
        #[allow(clippy::panic)]
        match &self.supergraph {
            Supergraph::ApolloFederation(s) => s,
            Supergraph::ApolloCompiler(_) => panic!("expected an apollo-federation supergraph"),
        }
    }

    pub(crate) fn supergraph_schema(&self) -> &Valid<apollo_compiler::Schema> {
        match &self.supergraph {
            Supergraph::ApolloFederation(s) => s.schema.schema(),
            Supergraph::ApolloCompiler(s) => s,
        }
    }

    pub(crate) fn schema_id(sdl: &str) -> String {
        let mut hasher = Sha256::new();
        hasher.update(sdl.as_bytes());
        format!("{:x}", hasher.finalize())
    }

    pub(crate) fn create_api_schema(
        &self,
        configuration: &Configuration,
    ) -> Result<String, SchemaError> {
        use apollo_federation::ApiSchemaOptions;

        let api_schema = self
            .federation_supergraph()
            .to_api_schema(ApiSchemaOptions {
                include_defer: configuration.supergraph.defer_support,
                ..Default::default()
            })
            .map_err(|e| {
                SchemaError::Api(format!(
                    "The supergraph schema failed to produce a valid API schema: {e}"
                ))
            })?;
        Ok(api_schema.schema().to_string())
    }

    pub(crate) fn with_api_schema(mut self, api_schema: Valid<apollo_compiler::Schema>) -> Self {
        self.api_schema = Some(ApiSchema(api_schema));
        self
    }

    /// Extracts a string containing the entire [`Schema`].
    pub(crate) fn as_string(&self) -> &Arc<String> {
        &self.raw_sdl
    }

    pub(crate) fn is_subtype(&self, abstract_type: &str, maybe_subtype: &str) -> bool {
        self.supergraph_schema()
            .is_subtype(abstract_type, maybe_subtype)
    }

    pub(crate) fn is_implementation(&self, interface: &str, implementor: &str) -> bool {
        self.supergraph_schema()
            .get_interface(interface)
            .map(|interface| {
                // FIXME: this looks backwards
                interface.implements_interfaces.contains(implementor)
            })
            .unwrap_or(false)
    }

    pub(crate) fn is_interface(&self, abstract_type: &str) -> bool {
        self.supergraph_schema()
            .get_interface(abstract_type)
            .is_some()
    }

    // given two field, returns the one that implements the other, if applicable
    pub(crate) fn most_precise<'f>(&self, a: &'f str, b: &'f str) -> Option<&'f str> {
        let typename_a = a;
        let typename_b = b;
        if typename_a == typename_b {
            return Some(a);
        }
        if self.is_subtype(typename_a, typename_b) || self.is_implementation(typename_a, typename_b)
        {
            Some(b)
        } else if self.is_subtype(typename_b, typename_a)
            || self.is_implementation(typename_b, typename_a)
        {
            Some(a)
        } else {
            // No relationship between a and b
            None
        }
    }

    /// Return an iterator over subgraphs that yields the subgraph name and its URL.
    pub(crate) fn subgraphs(&self) -> impl Iterator<Item = (&String, &Uri)> {
        self.subgraphs.iter()
    }

    /// Return the subgraph URI given the service name
    pub(crate) fn subgraph_url(&self, service_name: &str) -> Option<&Uri> {
        self.subgraphs.get(service_name)
    }

    // TODO: make `self.api_schema` non-optional after we move to Rust-only API schema generation
    #[allow(clippy::panic)]
    pub(crate) fn api_schema(&self) -> &ApiSchema {
        match &self.api_schema {
            Some(schema) => schema,
            None => panic!("missing API schema"),
        }
    }

    pub(crate) fn root_operation_name(&self, kind: OperationKind) -> &str {
        if let Some(name) = self.supergraph_schema().root_operation(kind.into()) {
            name.as_str()
        } else {
            kind.default_type_name()
        }
    }

    /// Return the federation major version based on the @link or @core directives in the schema,
    /// or None if there are no federation directives.
    pub(crate) fn federation_version(&self) -> Option<i64> {
        for directive in &self.supergraph_schema().schema_definition.directives {
            let join_url = if directive.name == "core" {
                let Some(feature) = directive
                    .argument_by_name("feature")
                    .and_then(|value| value.as_str())
                else {
                    continue;
                };

                feature
            } else if directive.name == "link" {
                let Some(url) = directive
                    .argument_by_name("url")
                    .and_then(|value| value.as_str())
                else {
                    continue;
                };

                url
            } else {
                continue;
            };

            match join_url.rsplit_once("/v") {
                Some(("https://specs.apollo.dev/join", "0.1")) => return Some(1),
                Some(("https://specs.apollo.dev/join", _)) => return Some(2),
                _ => {}
            }
        }
        None
    }

    pub(crate) fn has_spec(&self, base_url: &str, expected_version_range: &str) -> bool {
        self.supergraph_schema()
            .schema_definition
            .directives
            .iter()
            .filter(|dir| dir.name.as_str() == "link")
            .any(|link| {
                if let Some(url_in_link) = link
                    .argument_by_name("url")
                    .and_then(|value| value.as_str())
                {
                    let Some((base_url_in_link, version_in_link)) = url_in_link.rsplit_once("/v")
                    else {
                        return false;
                    };

                    let Some(version_in_url) =
                        Version::parse(format!("{}.0", version_in_link).as_str()).ok()
                    else {
                        return false;
                    };

                    let Some(version_range) = VersionReq::parse(expected_version_range).ok() else {
                        return false;
                    };

                    base_url_in_link == base_url && version_range.matches(&version_in_url)
                } else {
                    false
                }
            })
    }

    pub(crate) fn directive_name(
        schema: &apollo_compiler::schema::Schema,
        base_url: &str,
        expected_version_range: &str,
        default: &str,
    ) -> Option<String> {
        schema
            .schema_definition
            .directives
            .iter()
            .filter(|dir| dir.name.as_str() == "link")
            .find(|link| {
                if let Some(url_in_link) = link
                    .argument_by_name("url")
                    .and_then(|value| value.as_str())
                {
                    let Some((base_url_in_link, version_in_link)) = url_in_link.rsplit_once("/v")
                    else {
                        return false;
                    };

                    let Some(version_in_url) =
                        Version::parse(format!("{}.0", version_in_link).as_str()).ok()
                    else {
                        return false;
                    };

                    let Some(version_range) = VersionReq::parse(expected_version_range).ok() else {
                        return false;
                    };

                    base_url_in_link == base_url && version_range.matches(&version_in_url)
                } else {
                    false
                }
            })
            .map(|link| {
                link.argument_by_name("as")
                    .and_then(|value| value.as_str().map(|s| s.to_string()))
                    .unwrap_or_else(|| default.to_string())
            })
    }
}

impl std::fmt::Debug for Schema {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let Self {
            raw_sdl,
            supergraph: _, // skip
            subgraphs,
            implementers_map,
            api_schema: _, // skip
        } = self;
        f.debug_struct("Schema")
            .field("raw_sdl", raw_sdl)
            .field("subgraphs", subgraphs)
            .field("implementers_map", implementers_map)
            .finish()
    }
}

#[derive(Debug)]
pub(crate) struct InvalidObject;

impl std::ops::Deref for ApiSchema {
    type Target = Valid<apollo_compiler::Schema>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn with_supergraph_boilerplate(content: &str) -> String {
        format!(
            "{}\n{}",
            r#"
        schema
            @core(feature: "https://specs.apollo.dev/core/v0.1")
            @core(feature: "https://specs.apollo.dev/join/v0.1") {
            query: Query
        }
        directive @core(feature: String!) repeatable on SCHEMA
        directive @join__graph(name: String!, url: String!) on ENUM_VALUE
        enum join__Graph {
            TEST @join__graph(name: "test", url: "http://localhost:4001/graphql")
        }

        "#,
            content
        )
    }

    #[test]
    fn is_subtype() {
        fn gen_schema_types(schema: &str) -> Schema {
            let base_schema = with_supergraph_boilerplate(
                r#"
            type Query {
              me: String
            }
            type Foo {
              me: String
            }
            type Bar {
              me: String
            }
            type Baz {
              me: String
            }
            
            union UnionType2 = Foo | Bar
            "#,
            );
            let schema = format!("{base_schema}\n{schema}");
            Schema::parse_test(&schema, &Default::default()).unwrap()
        }

        fn gen_schema_interfaces(schema: &str) -> Schema {
            let base_schema = with_supergraph_boilerplate(
                r#"
            type Query {
              me: String
            }
            interface Foo {
              me: String
            }
            interface Bar {
              me: String
            }
            interface Baz {
              me: String,
            }

            type ObjectType2 implements Foo & Bar { me: String }
            interface InterfaceType2 implements Foo & Bar { me: String }
            "#,
            );
            let schema = format!("{base_schema}\n{schema}");
            Schema::parse_test(&schema, &Default::default()).unwrap()
        }
        let schema = gen_schema_types("union UnionType = Foo | Bar | Baz");
        assert!(schema.is_subtype("UnionType", "Foo"));
        assert!(schema.is_subtype("UnionType", "Bar"));
        assert!(schema.is_subtype("UnionType", "Baz"));
        let schema =
            gen_schema_interfaces("type ObjectType implements Foo & Bar & Baz { me: String }");
        assert!(schema.is_subtype("Foo", "ObjectType"));
        assert!(schema.is_subtype("Bar", "ObjectType"));
        assert!(schema.is_subtype("Baz", "ObjectType"));
        let schema = gen_schema_interfaces(
            "interface InterfaceType implements Foo & Bar & Baz { me: String }",
        );
        assert!(schema.is_subtype("Foo", "InterfaceType"));
        assert!(schema.is_subtype("Bar", "InterfaceType"));
        assert!(schema.is_subtype("Baz", "InterfaceType"));
        let schema = gen_schema_types("extend union UnionType2 = Baz");
        assert!(schema.is_subtype("UnionType2", "Foo"));
        assert!(schema.is_subtype("UnionType2", "Bar"));
        assert!(schema.is_subtype("UnionType2", "Baz"));
        let schema =
            gen_schema_interfaces("extend type ObjectType2 implements Baz { me2: String }");
        assert!(schema.is_subtype("Foo", "ObjectType2"));
        assert!(schema.is_subtype("Bar", "ObjectType2"));
        assert!(schema.is_subtype("Baz", "ObjectType2"));
        let schema =
            gen_schema_interfaces("extend interface InterfaceType2 implements Baz { me2: String }");
        assert!(schema.is_subtype("Foo", "InterfaceType2"));
        assert!(schema.is_subtype("Bar", "InterfaceType2"));
        assert!(schema.is_subtype("Baz", "InterfaceType2"));
    }

    #[test]
    fn routing_urls() {
        let schema = r#"
        schema
          @core(feature: "https://specs.apollo.dev/core/v0.1"),
          @core(feature: "https://specs.apollo.dev/join/v0.1")
        {
          query: Query
        }
        type Query {
          me: String
        }
        directive @core(feature: String!) repeatable on SCHEMA
        directive @join__graph(name: String!, url: String!) on ENUM_VALUE

        enum join__Graph {
            ACCOUNTS @join__graph(name:"accounts" url: "http://localhost:4001/graphql")
            INVENTORY
              @join__graph(name: "inventory", url: "http://localhost:4004/graphql")
            PRODUCTS
            @join__graph(name: "products" url: "http://localhost:4003/graphql")
            REVIEWS @join__graph(name: "reviews" url: "http://localhost:4002/graphql")
        }"#;
        let schema = Schema::parse_test(schema, &Default::default()).unwrap();

        assert_eq!(schema.subgraphs.len(), 4);
        assert_eq!(
            schema
                .subgraphs
                .get("accounts")
                .map(|s| s.to_string())
                .as_deref(),
            Some("http://localhost:4001/graphql"),
            "Incorrect url for accounts"
        );

        assert_eq!(
            schema
                .subgraphs
                .get("inventory")
                .map(|s| s.to_string())
                .as_deref(),
            Some("http://localhost:4004/graphql"),
            "Incorrect url for inventory"
        );

        assert_eq!(
            schema
                .subgraphs
                .get("products")
                .map(|s| s.to_string())
                .as_deref(),
            Some("http://localhost:4003/graphql"),
            "Incorrect url for products"
        );

        assert_eq!(
            schema
                .subgraphs
                .get("reviews")
                .map(|s| s.to_string())
                .as_deref(),
            Some("http://localhost:4002/graphql"),
            "Incorrect url for reviews"
        );

        assert_eq!(schema.subgraphs.get("test"), None);
    }

    #[test]
    fn api_schema() {
        let schema = include_str!("../testdata/contract_schema.graphql");
        let schema = Schema::parse_test(schema, &Default::default()).unwrap();
        let has_in_stock_field = |schema: &apollo_compiler::Schema| {
            schema
                .get_object("Product")
                .unwrap()
                .fields
                .contains_key("inStock")
        };
        assert!(has_in_stock_field(schema.supergraph_schema()));
        assert!(!has_in_stock_field(schema.api_schema()));
    }

    #[test]
    fn federation_version() {
        // @core directive
        let schema = Schema::parse_test(
            include_str!("../testdata/minimal_supergraph.graphql"),
            &Default::default(),
        )
        .unwrap();
        assert_eq!(schema.federation_version(), Some(1));

        // @link directive
        let schema = Schema::parse_test(
            include_str!("../testdata/minimal_fed2_supergraph.graphql"),
            &Default::default(),
        )
        .unwrap();
        assert_eq!(schema.federation_version(), Some(2));
    }

    #[test]
    fn schema_id() {
        #[cfg(not(windows))]
        {
            let schema = include_str!("../testdata/starstuff@current.graphql");
            let schema = Schema::parse_test(schema, &Default::default()).unwrap();

            assert_eq!(
                Schema::schema_id(&schema.raw_sdl),
                "8e2021d131b23684671c3b85f82dfca836908c6a541bbd5c3772c66e7f8429d8".to_string()
            );
        }
    }

    // test for https://github.com/apollographql/federation/pull/1769
    #[test]
    fn inaccessible_on_non_core() {
        let schema = include_str!("../testdata/inaccessible_on_non_core.graphql");
        match Schema::parse_test(schema, &Default::default()) {
            Err(SchemaError::Api(s)) => {
                assert_eq!(
                    s,
                    r#"The supergraph schema failed to produce a valid API schema: The following errors occurred:

  - Input field `InputObject.privateField` is @inaccessible but is used in the default value of `@foo(someArg:)`, which is in the API schema."#
                );
            }
            other => panic!("unexpected schema result: {other:?}"),
        };
    }

    // https://github.com/apollographql/router/issues/2269
    #[test]
    fn unclosed_brace_error_does_not_panic() {
        let schema = "schema {";
        let result = Schema::parse_test(schema, &Default::default());
        assert!(result.is_err());
    }
}
