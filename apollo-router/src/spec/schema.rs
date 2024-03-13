//! GraphQL schema.

use std::collections::HashMap;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Instant;

use apollo_compiler::ast;
use apollo_compiler::schema::Implementers;
use apollo_compiler::validation::Valid;
use apollo_compiler::validation::WithErrors;
use http::Uri;
use semver::Version;
use semver::VersionReq;
use sha2::Digest;
use sha2::Sha256;

use crate::error::ParseErrors;
use crate::error::SchemaError;
use crate::error::ValidationErrors;
use crate::query_planner::OperationKind;
use crate::Configuration;

/// A GraphQL schema.
#[derive(Debug)]
pub(crate) struct Schema {
    pub(crate) raw_sdl: Arc<String>,
    pub(crate) definitions: Valid<apollo_compiler::Schema>,
    subgraphs: HashMap<String, Uri>,
    pub(crate) implementers_map: HashMap<ast::Name, Implementers>,
    api_schema: Option<Box<Schema>>,
    pub(crate) schema_id: Option<String>,
}

impl Schema {
    #[cfg(test)]
    pub(crate) fn parse_test(s: &str, configuration: &Configuration) -> Result<Self, SchemaError> {
        let schema = Self::parse(s, configuration)?;
        let api_schema = Self::parse(
            &schema
                .create_api_schema(configuration)
                // Avoid adding an error branch that's only used in tests--stick the error
                // string in an existing generic one
                .map_err(|err| {
                    SchemaError::Api(format!(
                        "The supergraph schema failed to produce a valid API schema: {err}"
                    ))
                })?,
            configuration,
        )?;
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

    pub(crate) fn parse(sdl: &str) -> Result<Self, SchemaError> {
        let start = Instant::now();
        let ast = Self::parse_ast(sdl)?;
        let definitions = match ast.to_schema_validate() {
            Ok(schema) => schema,
            Err(WithErrors { errors, .. }) => {
                return Err(SchemaError::Validate(ValidationErrors {
                    errors: errors.iter().map(|e| e.to_json()).collect(),
                }));
            }
        };

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
                let url = Uri::from_str(url)
                    .map_err(|err| SchemaError::UrlParse(name.to_string(), err))?;
                if subgraphs.insert(name.to_string(), url).is_some() {
                    return Err(SchemaError::Api(format!(
                        "must not have several subgraphs with same name '{name}'"
                    )));
                }
            }
        }

        let schema_id = Some(Self::schema_id(sdl));
        tracing::info!(
            histogram.apollo.router.schema.load.duration = start.elapsed().as_secs_f64()
        );

        let implementers_map = definitions.implementers_map();

        Ok(Schema {
            raw_sdl: Arc::new(sdl.to_owned()),
            definitions,
            subgraphs,
            implementers_map,
            api_schema: None,
            schema_id,
        })
    }

    pub(crate) fn schema_id(sdl: &str) -> String {
        let mut hasher = Sha256::new();
        hasher.update(sdl.as_bytes());
        format!("{:x}", hasher.finalize())
    }

    pub(crate) fn create_api_schema(
        &self,
        configuration: &Configuration,
    ) -> Result<String, apollo_federation::error::FederationError> {
        use apollo_federation::ApiSchemaOptions;
        use apollo_federation::Supergraph;

        let schema = Supergraph::from_schema(self.definitions.clone())?;
        let api_schema = schema.to_api_schema(ApiSchemaOptions {
            include_defer: configuration.supergraph.defer_support,
            ..Default::default()
        })?;
        Ok(api_schema.schema().to_string())
    }

    pub(crate) fn with_api_schema(mut self, api_schema: Schema) -> Self {
        self.api_schema = Some(Box::new(api_schema));
        self
    }
}

impl Schema {
    /// Extracts a string containing the entire [`Schema`].
    pub(crate) fn as_string(&self) -> &Arc<String> {
        &self.raw_sdl
    }

    pub(crate) fn is_subtype(&self, abstract_type: &str, maybe_subtype: &str) -> bool {
        self.definitions.is_subtype(abstract_type, maybe_subtype)
    }

    pub(crate) fn is_implementation(&self, interface: &str, implementor: &str) -> bool {
        self.definitions
            .get_interface(interface)
            .map(|interface| {
                // FIXME: this looks backwards
                interface.implements_interfaces.contains(implementor)
            })
            .unwrap_or(false)
    }

    pub(crate) fn is_interface(&self, abstract_type: &str) -> bool {
        self.definitions.get_interface(abstract_type).is_some()
    }

    pub(crate) fn is_union(&self, abstract_type: &str) -> bool {
        self.definitions.get_union(abstract_type).is_some()
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

    pub(crate) fn api_schema(&self) -> &Schema {
        match &self.api_schema {
            Some(schema) => schema,
            None => self,
        }
    }

    pub(crate) fn root_operation_name(&self, kind: OperationKind) -> &str {
        if let Some(name) = self.definitions.root_operation(kind.into()) {
            name.as_str()
        } else {
            kind.as_str()
        }
    }

    /// Return the federation major version based on the @link or @core directives in the schema,
    /// or None if there are no federation directives.
    pub(crate) fn federation_version(&self) -> Option<i64> {
        for directive in &self.definitions.schema_definition.directives {
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
        self.definitions
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

#[derive(Debug)]
pub(crate) struct InvalidObject;

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
            Schema::parse_test(&schema).unwrap()
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
            Schema::parse_test(&schema).unwrap()
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
        let schema = Schema::parse_test(schema).unwrap();

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
        let schema = Schema::parse_test(schema).unwrap();
        let has_in_stock_field = |schema: &Schema| {
            schema
                .definitions
                .get_object("Product")
                .unwrap()
                .fields
                .contains_key("inStock")
        };
        assert!(has_in_stock_field(&schema));
        assert!(!has_in_stock_field(schema.api_schema.as_ref().unwrap()));
    }

    #[test]
    fn federation_version() {
        // @core directive
        let schema =
            Schema::parse_test(include_str!("../testdata/minimal_supergraph.graphql")).unwrap();
        assert_eq!(schema.federation_version(), Some(1));

        // @link directive
        let schema =
            Schema::parse_test(include_str!("../testdata/minimal_fed2_supergraph.graphql"))
                .unwrap();
        assert_eq!(schema.federation_version(), Some(2));
    }

    #[test]
    fn schema_id() {
        #[cfg(not(windows))]
        {
            let schema = include_str!("../testdata/starstuff@current.graphql");
            let schema = Schema::parse_test(schema).unwrap();

            assert_eq!(
                schema.schema_id,
                Some(
                    "8e2021d131b23684671c3b85f82dfca836908c6a541bbd5c3772c66e7f8429d8".to_string()
                )
            );

            assert_eq!(
                schema.api_schema().schema_id,
                Some(
                    "6af283f857f47055b0069547a8ee21c942c2c72ceebbcaabf78a42f0d1786318".to_string()
                )
            );
        }
    }

    // test for https://github.com/apollographql/federation/pull/1769
    #[test]
    fn inaccessible_on_non_core() {
        let schema = include_str!("../testdata/inaccessible_on_non_core.graphql");
        match Schema::parse_test(schema) {
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
        let result = Schema::parse_test(schema);
        assert!(result.is_err());
    }
}
