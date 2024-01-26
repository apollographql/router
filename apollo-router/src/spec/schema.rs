//! GraphQL schema.

use std::collections::HashMap;
use std::collections::HashSet;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Instant;

use apollo_compiler::ast;
use apollo_compiler::validation::DiagnosticList;
use apollo_compiler::validation::Valid;
use apollo_compiler::validation::WithErrors;
use http::Uri;
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
    /// Stored for comparison with the validation errors from query planning.
    diagnostics: Option<DiagnosticList>,
    subgraphs: HashMap<String, Uri>,
    pub(crate) implementers_map: HashMap<ast::Name, HashSet<ast::Name>>,
    api_schema: Option<Box<Schema>>,
    pub(crate) schema_id: Option<String>,
}

#[cfg(test)]
fn make_api_schema(schema: &str, configuration: &Configuration) -> Result<String, SchemaError> {
    use itertools::Itertools;
    use router_bridge::api_schema::api_schema;
    use router_bridge::api_schema::ApiSchemaOptions;
    let s = api_schema(
        schema,
        ApiSchemaOptions {
            graphql_validation: false,
        },
    )
    .map_err(|e| SchemaError::Api(e.to_string()))?
    .map_err(|e| SchemaError::Api(e.iter().filter_map(|e| e.message.as_ref()).join(", ")))?;
    Ok(format!("{s}\n"))
}

impl Schema {
    #[cfg(test)]
    pub(crate) fn parse_test(s: &str, configuration: &Configuration) -> Result<Self, SchemaError> {
        let api_schema = Self::parse(&make_api_schema(s, configuration)?, configuration)?;
        let schema = Self::parse(s, configuration)?.with_api_schema(api_schema);
        Ok(schema)
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

    pub(crate) fn parse(sdl: &str, configuration: &Configuration) -> Result<Self, SchemaError> {
        let start = Instant::now();
        let ast = Self::parse_ast(sdl)?;
        // Stretch the meaning of "assume valid" to "we’ll check later that it’s valid"
        let (definitions, diagnostics) = match ast.to_schema_validate() {
            Ok(schema) => (schema, None),
            Err(WithErrors { partial, errors }) => {
                return Err(SchemaError::Validate(ValidationErrors { errors }));
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
            diagnostics,
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

    pub(crate) fn create_api_schema(&self) -> String {
        apollo_federation::Supergraph::from(self.definitions.clone())
            .to_api_schema()
            .to_string()
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

    pub(crate) fn has_errors(&self) -> bool {
        self.diagnostics.is_some()
    }

    pub(crate) fn has_spec(&self, url: &str) -> bool {
        self.definitions
            .schema_definition
            .directives
            .iter()
            .filter(|dir| dir.name.as_str() == "link")
            .any(|link| {
                link.argument_by_name("url")
                    .and_then(|value| value.as_str())
                    == Some(url)
            })
    }

    pub(crate) fn directive_name(
        schema: &apollo_compiler::schema::Schema,
        url: &str,
        default: &str,
    ) -> Option<String> {
        schema
            .schema_definition
            .directives
            .iter()
            .filter(|dir| dir.name.as_str() == "link")
            .find(|link| {
                link.argument_by_name("url")
                    .and_then(|value| value.as_str())
                    == Some(url)
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
    fn schema_id() {
        #[cfg(not(windows))]
        {
            let schema = include_str!("../testdata/starstuff@current.graphql");
            let schema = Schema::parse_test(schema, &Default::default()).unwrap();

            assert_eq!(
                schema.schema_id,
                Some(
                    "8e2021d131b23684671c3b85f82dfca836908c6a541bbd5c3772c66e7f8429d8".to_string()
                )
            );

            assert_eq!(
                schema.api_schema().schema_id,
                Some(
                    "ba573b479c8b3fa273f439b26b9eda700152341d897f18090d52cd073b15f909".to_string()
                )
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
                    r#"The supergraph schema failed to produce a valid API schema. Caused by:
Input field "InputObject.privateField" is @inaccessible but is used in the default value of "@foo(someArg:)", which is in the API schema.

GraphQL request:42:1
41 |
42 | input InputObject {
   | ^
43 |   someField: String"#
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
