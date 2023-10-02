use insta::assert_json_snapshot;
use serde_json_bytes::json;
use test_log::test;

use super::*;
use crate::json_ext::ValueExt;

macro_rules! assert_eq_and_ordered {
    ($a:expr, $b:expr $(,)?) => {
        assert_eq!($a, $b,);
        assert!(
            $a.eq_and_ordered(&$b),
            "assertion failed: objects are not ordered the same:\
            \n  left: `{:?}`\n right: `{:?}`",
            $a,
            $b,
        );
    };
}

macro_rules! assert_eq_and_ordered_json {
    ($a:expr, $b:expr $(,)?) => {
        assert_eq!(
            $a,
            $b,
            "assertion failed: objects are not the same:\
            \n  left: `{}`\n right: `{}`",
            serde_json::to_string(&$a).unwrap(),
            serde_json::to_string(&$b).unwrap()
        );
        assert!(
            $a.eq_and_ordered(&$b),
            "assertion failed: objects are not ordered the same:\
            \n  left: `{}`\n right: `{}`",
            serde_json::to_string(&$a).unwrap(),
            serde_json::to_string(&$b).unwrap(),
        );
    };
}

#[derive(Default)]
struct FormatTest {
    schema: Option<&'static str>,
    query_type_name: Option<&'static str>,
    query: Option<&'static str>,
    operation: Option<&'static str>,
    variables: Option<serde_json_bytes::Value>,
    response: Option<serde_json_bytes::Value>,
    expected: Option<serde_json_bytes::Value>,
    expected_errors: Option<serde_json_bytes::Value>,
    expected_extensions: Option<serde_json_bytes::Value>,
    federation_version: FederationVersion,
}

#[derive(Default)]
enum FederationVersion {
    #[default]
    Fed1,
    Fed2,
}

impl FormatTest {
    fn builder() -> Self {
        Self::default()
    }

    fn schema(mut self, schema: &'static str) -> Self {
        self.schema = Some(schema);
        self
    }

    fn query(mut self, query: &'static str) -> Self {
        self.query = Some(query);
        self
    }

    fn query_type_name(mut self, name: &'static str) -> Self {
        self.query_type_name = Some(name);
        self
    }

    fn operation(mut self, operation: &'static str) -> Self {
        self.operation = Some(operation);
        self
    }

    fn response(mut self, v: serde_json_bytes::Value) -> Self {
        self.response = Some(v);
        self
    }

    fn variables(mut self, v: serde_json_bytes::Value) -> Self {
        self.variables = Some(v);
        self
    }

    fn expected(mut self, v: serde_json_bytes::Value) -> Self {
        self.expected = Some(v);
        self
    }

    fn expected_extensions(mut self, v: serde_json_bytes::Value) -> Self {
        self.expected_extensions = Some(v);
        self
    }

    fn fed2(mut self) -> Self {
        self.federation_version = FederationVersion::Fed2;
        self
    }

    #[track_caller]
    fn test(self) {
        let schema = self.schema.expect("missing schema");
        let query = self.query.expect("missing query");
        let response = self.response.expect("missing response");
        let query_type_name = self.query_type_name.unwrap_or("Query");

        let schema = match self.federation_version {
            FederationVersion::Fed1 => with_supergraph_boilerplate(schema, query_type_name),
            FederationVersion::Fed2 => with_supergraph_boilerplate_fed2(schema, query_type_name),
        };

        let schema =
            Schema::parse_test(&schema, &Default::default()).expect("could not parse schema");

        let api_schema = schema.api_schema();
        let query =
            Query::parse(query, &schema, &Default::default()).expect("could not parse query");
        let mut response = Response::builder().data(response).build();

        query.format_response(
            &mut response,
            self.operation,
            self.variables
                .unwrap_or_else(|| Value::Object(Object::default()))
                .as_object()
                .unwrap()
                .clone(),
            api_schema,
            BooleanValues { bits: 0 },
        );

        if let Some(e) = self.expected {
            assert_eq_and_ordered_json!(
                serde_json_bytes::to_value(response.data.as_ref()).unwrap(),
                e
            );
        }

        if let Some(e) = self.expected_errors {
            assert_eq_and_ordered_json!(serde_json_bytes::to_value(&response.errors).unwrap(), e);
        }

        if let Some(e) = self.expected_extensions {
            assert_eq_and_ordered_json!(
                serde_json_bytes::to_value(&response.extensions).unwrap(),
                e
            );
        }
    }
}

fn with_supergraph_boilerplate(content: &str, query_type_name: &str) -> String {
    format!(
        r#"
    schema
        @core(feature: "https://specs.apollo.dev/core/v0.1")
        @core(feature: "https://specs.apollo.dev/join/v0.1")
        @core(feature: "https://specs.apollo.dev/inaccessible/v0.1")
         {{
        query: {query_type_name}
    }}
    directive @core(feature: String!) repeatable on SCHEMA
    directive @join__graph(name: String!, url: String!) on ENUM_VALUE
    directive @inaccessible on OBJECT | FIELD_DEFINITION | INTERFACE | UNION
    enum join__Graph {{
        TEST @join__graph(name: "test", url: "http://localhost:4001/graphql")
    }}

    {content}
    "#
    )
}

fn with_supergraph_boilerplate_fed2(content: &str, query_type_name: &str) -> String {
    format!(
        r#"
        schema
        @link(url: "https://specs.apollo.dev/link/v1.0")
        @link(url: "https://specs.apollo.dev/join/v0.2", for: EXECUTION)
        @link(url: "https://specs.apollo.dev/inaccessible/v0.2", for: SECURITY)
        {{
            query: {query_type_name}
        }}

        directive @join__field(graph: join__Graph!, requires: join__FieldSet, provides: join__FieldSet, type: String, external: Boolean, override: String, usedOverridden: Boolean) repeatable on FIELD_DEFINITION | INPUT_FIELD_DEFINITION
        directive @join__graph(name: String!, url: String!) on ENUM_VALUE
        directive @join__implements(graph: join__Graph!, interface: String!) repeatable on OBJECT | INTERFACE
        directive @join__type(graph: join__Graph!, key: join__FieldSet, extension: Boolean! = false, resolvable: Boolean! = true) repeatable on OBJECT | INTERFACE | UNION | ENUM | INPUT_OBJECT | SCALAR
        directive @link(url: String, as: String, for: link__Purpose, import: [link__Import]) repeatable on SCHEMA
        directive @inaccessible on FIELD_DEFINITION | OBJECT | INTERFACE | UNION | ENUM | ENUM_VALUE | SCALAR | INPUT_OBJECT | INPUT_FIELD_DEFINITION | ARGUMENT_DEFINITION

        scalar join__FieldSet
        scalar link__Import
        enum link__Purpose {{
        """
        `SECURITY` features provide metadata necessary to securely resolve fields.
        """
        SECURITY

        """
        `EXECUTION` features provide metadata necessary for operation execution.
        """
        EXECUTION
        }}

        enum join__Graph {{
            TEST @join__graph(name: "test", url: "http://localhost:4001/graphql")
        }}

        {content}
    "#,
    )
}

#[test]
fn reformat_typename_of_query_not_named_query() {
    FormatTest::builder()
        .schema(
            "type MyRootQuery {
                foo: String
            }",
        )
        .query_type_name("MyRootQuery")
        .query("{ __typename }")
        .response(json! {{}})
        .expected(json! {{
            "__typename": "MyRootQuery",
        }})
        .test();
}

#[test]
fn reformat_response_data_field() {
    FormatTest::builder()
        .schema(
            "type Query {
        foo: String
        stuff: Bar
        array: [Bar]
        baz: String
    }
    type Bar {
        bar: String
        baz: String
    }",
        )
        .query(
            "query Test {
        foo
        stuff{bar __typename }
        array{bar}
        baz
        alias:baz
        alias_obj:stuff{bar}
        alias_array:array{bar}
    }",
        )
        .response(json! {{
            "foo": "1",
            "stuff": {"bar": "2", "__typename": "Bar"},
            "array": [{"bar": "3", "baz": "4"}, {"bar": "5", "baz": "6"}],
            "baz": "7",
            "alias": "7",
            "alias_obj": {"bar": "8"},
            "alias_array": [{"bar": "9", "baz": "10"}, {"bar": "11", "baz": "12"}],
            "other": "13",
        }})
        .expected(json! {{
            "foo": "1",
            "stuff": {
                "bar": "2",
                "__typename": "Bar",
            },
            "array": [
                {"bar": "3"},
                {"bar": "5"},
            ],
            "baz": "7",
            "alias": "7",
            "alias_obj": {
                "bar": "8",
            },
            "alias_array": [
                {"bar": "9"},
                {"bar": "11"},
            ],
        }})
        .test();
}

#[test]
fn reformat_response_data_inline_fragment() {
    let schema = "type Query {
        get: Test
        getStuff: Stuff
      }

      type Stuff {
          stuff: Bar
      }
      type Bar {
          bar: String
      }
      type Thing {
          id: String
      }
      union Test = Stuff | Thing";
    let query = "{ get { ... on Stuff { stuff{bar}} ... on Thing { id }} }";

    FormatTest::builder()
        .schema(schema)
        .query(query)
        .response(json! {
            {"get": {"__typename": "Stuff", "id": "1", "stuff": {"bar": "2"}}}
        })
        .expected(json! {{
            "get": {
                "stuff": {
                    "bar": "2",
                },
            }
        }})
        .test();

    FormatTest::builder()
        .schema(schema)
        .query(query)
        .response(json! {
            {"get": {"__typename": "Thing", "id": "1", "stuff": {"bar": "2"}}}
        })
        .expected(json! {{
            "get": {
                "id": "1",
            }
        }})
        .test();
}

#[test]
fn typename_with_alias() {
    let schema = "type Query {
        getStuff: Stuff
      }

      type Stuff {
          stuff: Bar
      }
      type Bar {
          bar: String
      }";
    let query = "{ getStuff { stuff{bar} } __0_typename: __typename }";

    FormatTest::builder()
        .schema(schema)
        .query(query)
        .response(json! {
            {"getStuff": { "stuff": {"bar": "2"}}}
        })
        .expected(json! {{
            "getStuff": {
                "stuff": {
                    "bar": "2",
                },
            },
            "__0_typename": "Query"
        }})
        .test();
}

#[test]
fn inline_fragment_on_top_level_operation() {
    let schema = "type Query {
        get: Test
      }

      type Stuff {
          stuff: Bar
      }
      type Bar {
          bar: String
      }
      type Thing {
          id: String
      }
      union Test = Stuff | Thing";

    // when using a fragment on an operation exported by a subgraph,
    // we might not get a __typename field, we should instead be able
    // to know the type in advance
    FormatTest::builder()
        .schema(schema)
        .query("{ get { ... on Stuff { stuff{bar}} ... on Thing { id }} }")
        .response(json! {
            {"get": { "__typename": "Stuff", "stuff": {"bar": "2"}}}
        })
        .expected(json! {{
             "get": {
                "stuff": {"bar": "2"},
            }
        }})
        .test();
}

#[test]
fn reformat_response_data_fragment_spread() {
    let schema = "type Query {
      thing: Thing    
    }

    type Foo {
        foo: String
    }
    type Bar {
        bar: String
    }
    type Baz {
        baz: String
    }
    union Thing = Foo
    extend union Thing = Bar | Baz";
    let query = "query { thing {...foo ...bar ...baz} } fragment foo on Foo {foo} fragment bar on Bar {bar} fragment baz on Baz {baz}";

    // should only select fields from Foo
    FormatTest::builder()
        .schema(schema)
        .query(query)
        .response(json! {
            {"thing": {"__typename": "Foo", "foo": "1", "bar": "2", "baz": "3"}}
        })
        .expected(json! {
            {"thing": {"foo": "1"}}
        })
        .test();

    // should only select fields from Bar
    FormatTest::builder()
        .schema(schema)
        .query(query)
        .response(json! {
            {"thing": {"__typename": "Bar", "foo": "1", "bar": "2", "baz": "3"}}
        })
        .expected(json! {
            {"thing": {"bar": "2"} }
        })
        .test();

    // should only select fields from Baz
    FormatTest::builder()
        .schema(schema)
        .query(query)
        .response(json! {
            {"thing": {"__typename": "Baz", "foo": "1", "bar": "2", "baz": "3"}}
        })
        .expected(json! {
            {"thing": {"baz": "3"} }
        })
        .test();
}

#[test]
fn reformat_response_data_best_effort() {
    FormatTest::builder()
        .schema(
            "type Query {
        get: Thing
    }
    type Thing {
        foo: String
        stuff: Baz
        array: [Element]
        other: Bar
    }

    type Baz {
        bar: String
        baz: String
    }

    type Bar {
        bar: String
    }

    union Element = Baz | Bar
    ",
        )
        .query("{get {foo stuff{bar baz} array{... on Baz { bar baz } } other{bar}}}")
        .response(json! {
            {
                "get": {
                    "foo": "1",
                    "stuff": {"baz": "2"},
                    "array": [
                        {"baz": "3"},
                        "4",
                        {"bar": "5"},
                    ],
                    "other": "6",
                },
                "should_be_removed": {
                    "aaa": 2
                },
            }
        })
        .expected(json! {
            {
                "get": {
                    "foo": "1",
                    "stuff": {
                        "bar": null,
                        "baz": "2",
                    },
                    "array": [
                        {},
                        null,
                        {}
                    ],
                    "other": null,
                },
            }
        })
        .test();
}

#[test]
// just like the test above, except the query is one the planner would generate.
fn reformat_response_data_best_effort_relevant_query() {
    FormatTest::builder()
        .schema(
            "type Query {
        get: Thing
    }
    type Thing {
        foo: String
        stuff: Baz
        array: [Element]
        other: Bar
    }

    type Baz {
        bar: String
        baz: String
    }

    type Bar {
        bar: String
    }

    union Element = Baz | Bar
    ",
        )
        .query("{get{foo stuff{bar baz}array{...on Baz{bar baz}}other{bar}}}")
        // the planner generates this:
        // {get{foo stuff{bar baz}array{__typename ...on Baz{bar baz}}other{bar}}}
        .response(json! {
            {
                "get": {
                    "foo": "1",
                    "stuff": {"baz": "2"},
                    "array": [
                        {
                            "__typename": "Baz",
                            "baz": "3"
                        },
                        "4",
                        {
                            "__typename": "Baz",
                            "baz": "5"
                        },
                    ],
                    "other": "6",
                },
                "should_be_removed": {
                    "aaa": 2
                },
            }
        })
        .expected(json! {
            {
                "get": {
                    "foo": "1",
                    "stuff": {
                        "bar": null,
                        "baz": "2",
                    },
                    "array": [
                        {
                            "bar":null,
                            "baz":"3"
                        },
                        null,
                        {
                            "bar": null,
                            "baz":"5"
                        }
                    ],
                    "other": null,
                },
            }
        })
        .test();
}

#[test]
fn reformat_response_array_of_scalar_simple() {
    FormatTest::builder()
        .schema(
            "type Query {
                get: Thing
            }
            type Thing {
                array: [Int]
            }",
        )
        .query("{get {array}}")
        .response(json! {{
            "get": {
                "array": [1,2,3,4],
            },
        }})
        .expected(json! {{
            "get": {
                "array": [1,2,3,4],
            },
        }})
        .test();
}

#[test]
fn reformat_response_array_of_scalar_alias() {
    FormatTest::builder()
        .schema(
            "type Query {
                get: Thing
            }
            type Thing {
                array: [Int]
            }",
        )
        .query("{get {stuff: array}}")
        .response(json! {{
            "get": {
                "stuff": [1,2,3,4],
            },
        }})
        .expected(json! {{
            "get": {
                "stuff": [1,2,3,4],
            },
        }})
        .test();
}

#[test]
fn reformat_response_array_of_scalar_duplicate_alias() {
    FormatTest::builder()
        .schema(
            "type Query {
            get: Thing
        }
        type Thing {
            array: [Int]
        }",
        )
        .query("{get {array stuff:array}}")
        .response(json! {{
            "get": {
                "array": [1,2,3,4],
                "stuff": [1,2,3,4],
            },
        }})
        .expected(json! {{
            "get": {
                "array": [1,2,3,4],
                "stuff": [1,2,3,4],
            },
        }})
        .test();
}

#[test]
fn reformat_response_array_of_scalar_duplicate_key() {
    FormatTest::builder()
        .schema(
            "type Query {
                get: Thing
            }
            type Thing {
                array: [Int]
            }",
        )
        .query("{get {array array}}")
        .response(json! {{
            "get": {
                "array": [1,2,3,4],
            },
        }})
        .expected(json! {{
            "get": {
                "array": [1,2,3,4],
            },
        }})
        .test();
}

#[test]
fn reformat_response_array_of_type_simple() {
    FormatTest::builder()
        .schema(
            "type Query {
            get: Thing
        }
        type Thing {
            array: [Element]
        }

        type Element {
            stuff: String
        }
        ",
        )
        .query("{get {array{stuff}}}")
        .response(json! {{
            "get": {
                "array": [{"stuff": "FOO"}, {"stuff": "BAR"}],
            },
        }})
        .expected(json! {{
            "get": {
                "array": [{"stuff": "FOO"}, {"stuff": "BAR"}],
            },
        }})
        .test();
}

#[test]
fn reformat_response_array_of_type_alias() {
    FormatTest::builder()
        .schema(
            "type Query {
                get: Thing
            }
            type Thing {
                array: [Element]
            }

            type Element {
                stuff: String
            }
            
        ",
        )
        .query("{get { aliased: array {stuff}}}")
        .response(json! {{
            "get": {
                "aliased": [{"stuff": "FOO"}, {"stuff": "BAR"}],
            },
        }})
        .expected(json! {{
            "get": {
                "aliased": [{"stuff": "FOO"}, {"stuff": "BAR"}],
            },
        }})
        .test();
}

#[test]
fn reformat_response_array_of_type_duplicate() {
    FormatTest::builder()
        .schema(
            "type Query {
                get: Thing
            }
            type Thing {
                array: [Element]
            }

            type Element {
                stuff: String
            }
            ",
        )
        .query("{get {array{stuff} array{stuff}}}")
        .response(json! {{
            "get": {
                "array": [{"stuff": "FOO"}, {"stuff": "BAR"}],
            },
        }})
        .expected(json! {{
            "get": {
                "array": [{"stuff": "FOO"}, {"stuff": "BAR"}],
            },
        }})
        .test();
}

#[test]
fn reformat_response_array_of_type_duplicate_alias() {
    FormatTest::builder()
        .schema(
            "type Query {
            get: Thing
        }
        type Thing {
            array: [Element]
        }
        
        type Element {
            stuff: String
        }",
        )
        .query("{get {array{stuff} aliased: array{stuff}}}")
        .response(json! {{
            "get": {
                "array": [{"stuff": "FOO"}, {"stuff": "BAR"}],
                "aliased": [{"stuff": "FOO"}, {"stuff": "BAR"}],
            },
        }})
        .expected(json! {{
            "get": {
                "array": [{"stuff": "FOO"}, {"stuff": "BAR"}],
                "aliased": [{"stuff": "FOO"}, {"stuff": "BAR"}],
            },
        }})
        .test();
}

#[test]
fn reformat_response_array_of_enum_simple() {
    FormatTest::builder()
        .schema(
            "type Query {
                get: Thing
            }
            type Thing {
                array: [Element]
            }

            enum Element {
                FOO
                BAR
            }",
        )
        .query("{get {array}}")
        .response(json! {{
            "get": {
                "array": ["FOO", "BAR"],
            },
        }})
        .expected(json! {{
            "get": {
                "array": ["FOO", "BAR"],
            },
        }})
        .test();
}

#[test]
fn reformat_response_array_of_enum_alias() {
    FormatTest::builder()
        .schema(
            "type Query {
            get: Thing
        }
        type Thing {
            array: [Element]
        }

        enum Element {
            FOO
            BAR
        }",
        )
        .query("{get {stuff: array}}")
        .response(json! {{
            "get": {
                "stuff": ["FOO", "BAR"],
            },
        }})
        .expected(json! {{
            "get": {
                "stuff": ["FOO", "BAR"],
            },
        }})
        .test();
}

#[test]
fn reformat_response_array_of_enum_duplicate() {
    FormatTest::builder()
        .schema(
            "type Query {
                get: Thing
            }
            type Thing {
                array: [Element]
            }

            enum Element {
                FOO
                BAR
            }",
        )
        .query("{get {array array}}")
        .response(json! {{
            "get": {
                "array": ["FOO", "BAR"],
            },
        }})
        .expected(json! {{
            "get": {
                "array": ["FOO", "BAR"],
            },
        }})
        .test();
}

#[test]
fn reformat_response_array_of_enum_duplicate_alias() {
    FormatTest::builder()
        .schema(
            "type Query {
            get: Thing
        }
        type Thing {
            array: [Element]
        }

        enum Element {
            FOO
            BAR
        }",
        )
        .query("{get {array stuff: array}}")
        .response(json! {{
            "get": {
                "array": ["FOO", "BAR"],
                "stuff": ["FOO", "BAR"],
            },
        }})
        .expected(json! {{
            "get": {
                "array": ["FOO", "BAR"],
                "stuff": ["FOO", "BAR"],
            },
        }})
        .test();
}

#[test]
// If this test fails, this means you got greedy about allocations,
// beware of aliases!
fn reformat_response_array_of_int_duplicate() {
    FormatTest::builder()
        .schema(
            "type Query {
                get: Thing
            }
            type Thing {
                array: [Int]
            }",
        )
        .query("{get {array array}}")
        .response(json! {{
            "get": {
                "array": [1,2,3,4],
            },
        }})
        .expected(json! {{
            "get": {
                "array": [1,2,3,4],
            },
        }})
        .test();
}

#[test]
fn reformat_response_array_of_float_duplicate() {
    FormatTest::builder()
        .schema(
            "type Query {
                get: Thing
            }
            type Thing {
                array: [Float]
            }",
        )
        .query("{get {array array}}")
        .response(json! {{
            "get": {
                "array": [1,2,3,4],
            },
        }})
        .expected(json! {{
            "get": {
                "array": [1,2,3,4],
            },
        }})
        .test();
}

#[test]
fn reformat_response_array_of_bool_duplicate() {
    FormatTest::builder()
        .schema(
            "type Query {
                get: Thing
            }
            type Thing {
                array: [Boolean]
            }",
        )
        .query("{get {array array}}")
        .response(json! {{
            "get": {
                "array": [true,false],
            },
        }})
        .expected(json! {{
            "get": {
                "array": [true,false],
            },
        }})
        .test();
}

#[test]
fn reformat_response_array_of_string_duplicate() {
    FormatTest::builder()
        .schema(
            "type Query {
                get: Thing
            }
            type Thing {
                array: [String]
            }",
        )
        .query("{get {array array}}")
        .response(json! {{
            "get": {
                "array": ["hello","world"],
            },
        }})
        .expected(json! {{
            "get": {
                "array": ["hello","world"],
            },
        }})
        .test();
}

#[test]
fn reformat_response_array_of_id_duplicate() {
    FormatTest::builder()
        .schema(
            "type Query {
                get: Thing
            }
            type Thing {
                array: [ID]
            }",
        )
        .query("{get {array array}}")
        .response(json! {{
            "get": {
                "array": ["hello","world"],
            },
        }})
        .expected(json! {{
            "get": {
                "array": ["hello","world"],
            },
        }})
        .test();
}

#[test]
fn solve_query_with_single_typename() {
    FormatTest::builder()
        .schema(
            "type Query {
                get: Thing
            }
            type Thing {
                array: [String]
            }",
        )
        .query("{ __typename }")
        .response(json! {{}})
        .expected(json! {{
            "__typename": "Query"
        }})
        .test();
}

#[test]
fn solve_query_with_aliased_typename() {
    FormatTest::builder()
        .schema(
            "type Query {
                get: Thing
            }
            type Thing {
                array: [String]
            }",
        )
        .query("{ aliased: __typename }")
        .response(json! {{}})
        .expected(json! {{
            "aliased": "Query"
        }})
        .test();
}

#[test]
fn solve_query_with_multiple_typenames() {
    FormatTest::builder()
        .schema(
            "type Query {
                get: Thing
            }
            type Thing {
                array: [String]
            }",
        )
        .query("{ aliased: __typename __typename }")
        .response(json! {{}})
        .expected(json! {{
            "aliased": "Query",
            "__typename": "Query"
        }})
        .test();
}

#[test]
fn reformat_response_query_with_root_typename() {
    FormatTest::builder()
        .schema(
            "type Query {
                get: Thing
            }
            type Thing {
                foo: String
            }",
        )
        .query("{get {foo __typename} __typename}")
        .response(json! {{
            "get": {
                "foo": "1",
                "__typename": "Thing"
            }
        }})
        .expected(json! {{
            "get": {
                "foo": "1",
                "__typename": "Thing"
            },
            "__typename": "Query",
        }})
        .test();
}

#[test]
fn reformat_response_interface_typename_not_queried() {
    // With the introduction of @interfaceObject, a subgraph can send back a typename that
    // correspond to an interface in the supergraph. As long as that typename is not requested,
    // we want this to be fine and to prevent formatting of the response.
    FormatTest::builder()
        .schema(
            "type Query {
                i: I
            }
            interface I {
                x: String
            }
            type A implements I {
                x: String
            }",
        )
        .query("{i{x}}")
        .response(json! {{
            "i": {
                "__typename": "I",
                "x": "foo",
            }
        }})
        .expected(json! {{
            "i": {
                "x": "foo",
            },
        }})
        .test();
}

#[test]
fn reformat_response_interface_typename_queried() {
    // As mentioned in the previous test, the introduction of @interfaceObject makes it possible
    // for a subgraph to send back a typename that correspond to an interface in the supergraph.
    // If that typename is queried, then the query planner will ensure that such typename is
    // replaced (overriden to a proper object type of the supergraph by a followup fetch). But
    // as that later fetch can fail, we can have to format a response where the typename is
    // requested and is still set to the interface. We must not return that value (it's invalid
    // graphQL) and instead nullify the response in that case.
    FormatTest::builder()
        .schema(
            "type Query {
                i: I
            }
            interface I {
                x: String
            }
            type A implements I {
                x: String
            }",
        )
        .query("{i{__typename x}}")
        .response(json! {{
            "i": {
                "__typename": "I",
                "x": "foo",
            }
        }})
        .expected(json! {{
            "i": null,
        }})
        .test();
}

#[test]
fn reformat_response_unknown_typename() {
    // If in a response we get a typename for a completely unknown type name, then we should
    // nullify the object as something is off, and in the worst case we could be inadvertently
    // leaking some @inaccessible type (or the subgraph is simply drunk but nullifying is fine too
    // then). This should happen whether the actual __typename is queried or not.
    let schema = "
      type Query {
          i: I
      }
      interface I {
          x: String
      }
      type A implements I {
          x: String
      }";

    // Without __typename queried
    FormatTest::builder()
        .schema(schema)
        .query("{i{x}}")
        .response(json! {{
            "i": {
                "__typename": "X",
                "x": "foo",
            }
        }})
        .expected(json! {{ "i": null, }})
        .test();

    // With __typename queried
    FormatTest::builder()
        .schema(schema)
        .query("{i{__typename x}}")
        .response(json! {{
            "i": {
                "__typename": "X",
                "x": "foo",
            }
        }})
        .expected(json! {{ "i": null, }})
        .test();
}

macro_rules! run_validation {
    ($schema:expr, $query:expr, $variables:expr $(,)?) => {{
        let variables = match $variables {
            Value::Object(object) => object,
            _ => unreachable!("variables must be an object"),
        };
        let schema =
            Schema::parse_test(&$schema, &Default::default()).expect("could not parse schema");
        let request = Request::builder()
            .variables(variables)
            .query($query.to_string())
            .build();
        let query = Query::parse(
            request
                .query
                .as_ref()
                .expect("query has been added right above; qed"),
            &schema,
            &Default::default(),
        )
        .expect("could not parse query");
        query.validate_variables(&request, &schema)
    }};
}

macro_rules! assert_validation {
    ($schema:expr, $query:expr, $variables:expr $(,)?) => {{
        let res = run_validation!(
            with_supergraph_boilerplate($schema, "Query"),
            $query,
            $variables
        );
        assert!(res.is_ok(), "validation should have succeeded: {:?}", res);
    }};
}

macro_rules! assert_validation_error {
    ($schema:expr, $query:expr, $variables:expr $(,)?) => {{
        let res = run_validation!(
            with_supergraph_boilerplate($schema, "Query"),
            $query,
            $variables
        );
        assert!(res.is_err(), "validation should have failed");
    }};
}

#[test]
fn variable_validation() {
    let schema = r#"
        type Query {
            int(a: Int): String
            float(a: Float): String
            str(a: String): String
            bool(a: Boolean): String
            id(a: ID): String
            intList(a: [Int]): String
            intListList(a: [[Int]]): String
            strList(a: [String]): String
        }
    "#;
    // https://spec.graphql.org/June2018/#sec-Int
    assert_validation!(schema, "query($foo:Int){int(a:$foo)}", json!({}));
    assert_validation_error!(schema, "query($foo:Int!){int(a:$foo)}", json!({}));
    assert_validation!(schema, "query($foo:Int=1){int(a:$foo)}", json!({}));
    assert_validation!(schema, "query($foo:Int!=1){int(a:$foo)}", json!({}));
    // When expected as an input type, only integer input values are accepted.
    assert_validation!(schema, "query($foo:Int){int(a:$foo)}", json!({"foo":2}));
    assert_validation!(
        schema,
        "query($foo:Int){int(a:$foo)}",
        json!({ "foo": i32::MAX })
    );
    assert_validation!(
        schema,
        "query($foo:Int){int(a:$foo)}",
        json!({ "foo": i32::MIN })
    );
    // All other input values, including strings with numeric content, must raise a query error indicating an incorrect type.
    assert_validation_error!(schema, "query($foo:Int){int(a:$foo)}", json!({"foo":"2"}));
    assert_validation_error!(schema, "query($foo:Int){int(a:$foo)}", json!({"foo":2.0}));
    assert_validation_error!(schema, "query($foo:Int){int(a:$foo)}", json!({"foo":"str"}));
    assert_validation_error!(schema, "query($foo:Int){int(a:$foo)}", json!({"foo":true}));
    assert_validation_error!(schema, "query($foo:Int){int(a:$foo)}", json!({"foo":{}}));
    //  If the integer input value represents a value less than -231 or greater than or equal to 231, a query error should be raised.
    assert_validation_error!(
        schema,
        "query($foo:Int){int(a:$foo)}",
        json!({ "foo": i32::MAX as i64 + 1 })
    );
    assert_validation_error!(
        schema,
        "query($foo:Int){int(a:$foo)}",
        json!({ "foo": i32::MIN as i64 - 1 })
    );

    // https://spec.graphql.org/draft/#sec-Float.Input-Coercion
    assert_validation!(schema, "query($foo:Float){float(a:$foo)}", json!({}));
    assert_validation_error!(schema, "query($foo:Float!){float(a:$foo)}", json!({}));

    // When expected as an input type, both integer and float input values are accepted.
    assert_validation!(schema, "query($foo:Float){float(a:$foo)}", json!({"foo":2}));
    assert_validation!(
        schema,
        "query($foo:Float){float(a:$foo)}",
        json!({"foo":2.0})
    );
    // double precision floats are valid
    assert_validation!(
        schema,
        "query($foo:Float){float(a:$foo)}",
        json!({"foo":1600341978193i64})
    );
    assert_validation!(
        schema,
        "query($foo:Float){float(a:$foo)}",
        json!({"foo":1600341978193f64})
    );
    // All other input values, including strings with numeric content,
    // must raise a request error indicating an incorrect type.
    assert_validation_error!(
        schema,
        "query($foo:Float){float(a:$foo)}",
        json!({"foo":"2.0"})
    );
    assert_validation_error!(
        schema,
        "query($foo:Float){float(a:$foo)}",
        json!({"foo":"2"})
    );

    // https://spec.graphql.org/June2018/#sec-String
    assert_validation!(schema, "query($foo:String){str(a:$foo)}", json!({}));
    assert_validation_error!(schema, "query($foo:String!){str(a:$foo)}", json!({}));

    // When expected as an input type, only valid UTF‐8 string input values are accepted.
    assert_validation!(
        schema,
        "query($foo:String){str(a:$foo)}",
        json!({"foo": "str"})
    );

    // All other input values must raise a query error indicating an incorrect type.
    assert_validation_error!(
        schema,
        "query($foo:String){str(a:$foo)}",
        json!({"foo":true})
    );
    assert_validation_error!(schema, "query($foo:String){str(a:$foo)}", json!({"foo": 0}));
    assert_validation_error!(
        schema,
        "query($foo:String){str(a:$foo)}",
        json!({"foo": 42.0})
    );
    assert_validation_error!(
        schema,
        "query($foo:String){str(a:$foo)}",
        json!({"foo": {}})
    );

    // https://spec.graphql.org/June2018/#sec-Boolean
    assert_validation!(schema, "query($foo:Boolean){bool(a:$foo)}", json!({}));
    assert_validation_error!(schema, "query($foo:Boolean!){bool(a:$foo)}", json!({}));
    // When expected as an input type, only boolean input values are accepted.
    // All other input values must raise a query error indicating an incorrect type.
    assert_validation!(
        schema,
        "query($foo:Boolean!){bool(a:$foo)}",
        json!({"foo":true})
    );
    assert_validation_error!(
        schema,
        "query($foo:Boolean!){bool(a:$foo)}",
        json!({"foo":"true"})
    );
    assert_validation_error!(
        schema,
        "query($foo:Boolean!){bool(a:$foo)}",
        json!({"foo": 0})
    );
    assert_validation_error!(
        schema,
        "query($foo:Boolean!){bool(a:$foo)}",
        json!({"foo": "no"})
    );

    assert_validation!(schema, "query($foo:Boolean=true){bool(a:$foo)}", json!({}));
    assert_validation!(schema, "query($foo:Boolean!=true){bool(a:$foo)}", json!({}));

    // https://spec.graphql.org/June2018/#sec-ID
    assert_validation!(schema, "query($foo:ID){id(a:$foo)}", json!({}));
    assert_validation_error!(schema, "query($foo:ID!){id(a:$foo)}", json!({}));
    // When expected as an input type, any string (such as "4") or integer (such as 4)
    // input value should be coerced to ID as appropriate for the ID formats a given GraphQL server expects.
    assert_validation!(schema, "query($foo:ID){id(a:$foo)}", json!({"foo": 4}));
    assert_validation!(schema, "query($foo:ID){id(a:$foo)}", json!({"foo": "4"}));
    assert_validation!(
        schema,
        "query($foo:String){str(a:$foo)}",
        json!({"foo": "str"})
    );
    assert_validation!(
        schema,
        "query($foo:String){str(a:$foo)}",
        json!({"foo": "4.0"})
    );
    // Any other input value, including float input values (such as 4.0), must raise a query error indicating an incorrect type.
    assert_validation_error!(schema, "query($foo:ID){id(a:$foo)}", json!({"foo": 4.0}));
    assert_validation_error!(schema, "query($foo:ID){id(a:$foo)}", json!({"foo": true}));
    assert_validation_error!(schema, "query($foo:ID){id(a:$foo)}", json!({"foo": {}}));

    // https://spec.graphql.org/June2018/#sec-Type-System.List
    assert_validation!(schema, "query($foo:[Int]){intList(a:$foo)}", json!({}));
    assert_validation!(schema, "query($foo:[Int!]){intList(a:$foo)}", json!({}));
    assert_validation!(
        schema,
        "query($foo:[Int!]){intList(a:$foo)}",
        json!({ "foo": null })
    );
    assert_validation!(
        schema,
        "query($foo:[Int]){intList(a:$foo)}",
        json!({"foo":1})
    );
    assert_validation!(
        schema,
        "query($foo:[String]){strList(a:$foo)}",
        json!({"foo":"bar"})
    );
    assert_validation!(
        schema,
        "query($foo:[[Int]]){intListList(a:$foo)}",
        json!({"foo":1})
    );
    assert_validation!(
        schema,
        "query($foo:[[Int]]){intListList(a:$foo)}",
        json!({"foo":[[1], [2, 3]]})
    );
    assert_validation_error!(
        schema,
        "query($foo:[Int]){intList(a:$foo)}",
        json!({"foo":"str"})
    );
    assert_validation_error!(
        schema,
        "query($foo:[Int]){intList(a:$foo)}",
        json!({"foo":{}})
    );
    assert_validation_error!(schema, "query($foo:[Int]!){intList(a:$foo)}", json!({}));
    assert_validation_error!(
        schema,
        "query($foo:[Int!]){intList(a:$foo)}",
        json!({"foo":[1, null]})
    );
    assert_validation!(
        schema,
        "query($foo:[Int]!){intList(a:$foo)}",
        json!({"foo":[]})
    );
    assert_validation!(
        schema,
        "query($foo:[Int]){intList(a:$foo)}",
        json!({"foo":[1,2,3]})
    );
    assert_validation_error!(
        schema,
        "query($foo:[Int]){intList(a:$foo)}",
        json!({"foo":["f","o","o"]})
    );
    assert_validation_error!(
        schema,
        "query($foo:[Int]){intList(a:$foo)}",
        json!({"foo":["1","2","3"]})
    );
    assert_validation!(
        schema,
        "query($foo:[String]){strList(a:$foo)}",
        json!({"foo":["1","2","3"]})
    );
    assert_validation_error!(
        schema,
        "query($foo:[String]){strList(a:$foo)}",
        json!({"foo":[1,2,3]})
    );
    assert_validation!(
        schema,
        "query($foo:[Int!]){intList(a:$foo)}",
        json!({"foo":[1,2,3]})
    );
    assert_validation_error!(
        schema,
        "query($foo:[Int!]){intList(a:$foo)}",
        json!({"foo":[1,null,3]})
    );
    assert_validation!(
        schema,
        "query($foo:[Int]){intList(a:$foo)}",
        json!({"foo":[1,null,3]})
    );

    // https://spec.graphql.org/June2018/#sec-Input-Objects
    assert_validation!(
        "input Foo{ y: String } type Query { x: String }",
        "query($foo:Foo){x}",
        json!({})
    );
    assert_validation!(
        "input Foo{ y: String } type Query { x: String }",
        "query($foo:Foo){x}",
        json!({"foo":{}})
    );
    assert_validation_error!(
        "input Foo{ y: String } type Query { x: String }",
        "query($foo:Foo){x}",
        json!({"foo":1})
    );
    assert_validation_error!(
        "input Foo{ y: String } type Query { x: String }",
        "query($foo:Foo){x}",
        json!({"foo":"str"})
    );
    assert_validation_error!(
        "input Foo{x:Int!} type Query { x: String }",
        "query($foo:Foo){x}",
        json!({"foo":{}})
    );
    assert_validation!(
        "input Foo{x:Int!} type Query { x: String }",
        "query($foo:Foo){x}",
        json!({"foo":{"x":1}})
    );
    assert_validation!(
        "scalar Foo type Query { x: String }",
        "query($foo:Foo!){x}",
        json!({"foo":{}})
    );
    assert_validation!(
        "scalar Foo type Query { x: String }",
        "query($foo:Foo!){x}",
        json!({"foo":1})
    );
    assert_validation_error!(
        "scalar Foo type Query { x: String }",
        "query($foo:Foo!){x}",
        json!({})
    );
    assert_validation!(
        "input Foo{bar:Bar!} input Bar{x:Int!} type Query { x: String }",
        "query($foo:Foo){x}",
        json!({"foo":{"bar":{"x":1}}})
    );
    assert_validation!(
        "enum Availability{AVAILABLE} type Product{availability:Availability! name:String} type Query{products(availability: Availability!): [Product]!}",
        "query GetProductsByAvailability($availability: Availability!){products(availability: $availability) {name}}",
        json!({"availability": "AVAILABLE"})
    );

    assert_validation!(
        "input MessageInput {
            content: String
            author: String
          }
          type Receipt {
              id: ID!
          }
          type Query{
              send(message: MessageInput): String}",
        "query {
            send(message: {
                content: \"Hello\"
                author: \"Me\"
            }) {
                id
            }}",
        json!({"availability": "AVAILABLE"})
    );

    assert_validation!(
        "input MessageInput {
            content: String
            author: String
          }
          type Receipt {
              id: ID!
          }
          type Query{
              send(message: MessageInput): String}",
        "query($msg: MessageInput) {
            send(message: $msg) {
                id
            }}",
        json!({"msg":  {
            "content": "Hello",
            "author": "Me"
        }})
    );

    assert_validation!(
        "type Mutation{
            foo(input: FooInput!): FooResponse!
        }
        type Query{
            data: String
        }

        input FooInput {
          enumWithDefault: EnumWithDefault! = WEB
        }
        type FooResponse {
            id: ID!
        }

        enum EnumWithDefault {
          WEB
          MOBILE
        }",
        "mutation foo($input: FooInput!) {
            foo (input: $input) {
            __typename
        }}",
        json!({"input":{}})
    );
}

#[test]
fn filter_root_errors() {
    let schema = "type Query {
        getInt: Int
        getNonNullString: String!
    }";

    FormatTest::builder()
        .schema(schema)
        .query("query MyOperation { getInt }")
        .response(json! {{
            "getInt": "not_an_int",
            "other": "2",
        }})
        .operation("MyOperation")
        .expected(json! {{
            "getInt": null,
        }})
        .test();

    FormatTest::builder()
        .schema(schema)
        .query("query { getNonNullString }")
        .response(json! {{
            "getNonNullString": 1,
        }})
        .expected(Value::Null)
        .test();
}

#[test]
fn filter_object_errors() {
    let schema = "type Query {
        me: User
    }

    type User {
        id: String!
        name: String
    }";
    let query = "query  { me { id name } }";

    // name expected a string, got an int
    FormatTest::builder()
        .schema(schema)
        .query(query)
        .response(json! {{
            "me": {
                "id": "a",
                "name": 1,
            },
        }})
        .expected(json! {{
            "me": {
                "id": "a",
                "name": null,
            },
        }})
        .test();

    // non null id expected a string, got an int
    FormatTest::builder()
        .schema(schema)
        .query(query)
        .response(json! {{
            "me": {
                "id": 1,
                "name": 1,
            },
        }})
        .expected(json! {{
            "me": null,
        }})
        .test();

    // non null id got a null
    FormatTest::builder()
        .schema(schema)
        .query(query)
        .response(json! {{
            "me": {
                "id": null,
            },
        }})
        .expected(json! {{
            "me": null,
        }})
        .test();

    // non null id was absent
    FormatTest::builder()
        .schema(schema)
        .query(query)
        .response(json! {{
            "me": { },
        }})
        .expected(json! {{
            "me": null,
        }})
        .test();

    // non null id was absent
    FormatTest::builder()
        .schema(schema)
        .query(query)
        .response(json! {{
            "me": {
                "name": 1,
            },
        }})
        .expected(json! {{
            "me": null,
        }})
        .test();

    // a non null field not present in the query should not be an error
    FormatTest::builder()
        .schema(schema)
        .query("query  { me { name } }")
        .response(json! {{
            "me": {
                "name": "a",
            },
        }})
        .expected(json! {{
            "me": {
                "name": "a",
            },
        }})
        .test();

    // if a field appears multiple times, selection should be deduplicated
    FormatTest::builder()
        .schema(schema)
        .query("query  { me { id id } }")
        .response(json! {{
            "me": {
                "id": "a",
            },
        }})
        .expected(json! {{
            "me": {
                "id": "a",
            },
        }})
        .test();

    // duplicate id field
    FormatTest::builder()
        .schema(schema)
        .query("query  { me { id ...on User { id } } }")
        .response(json! {{
            "me": {
                "__typename": "User",
                "id": "a",
            },
        }})
        .expected(json! {{
            "me": {
                "id": "a",
            },
        }})
        .test();
}

#[test]
fn filter_list_errors() {
    let schema = "type Query {
        list: TestList
    }

    type TestList {
        l1: [String]
        l2: [String!]
        l3: [String]!
        l4: [String!]!
    }";

    // l1: nullable list of nullable elements
    // any error should stop at the list elements
    FormatTest::builder()
        .schema(schema)
        .query("query { list { l1 } }")
        .response(json! {{
            "list": {
                "l1": ["abc", 1, { "foo": "bar"}, ["aaa"], "def"],
                "name": 1,
            },
        }})
        .expected(json! {{
            "list": {
                "l1": ["abc", null, null, null, "def"],
            },
        }})
        .test();

    // l1 expected a list, got a string
    FormatTest::builder()
        .schema(schema)
        .query("query { list { l1 } }")
        .response(json! {{
            "list": {
                "l1": "abc",
            },
        }})
        .expected(json! {{
            "list": {
                "l1": null,
            },
        }})
        .test();

    // l2: nullable list of non nullable elements
    // any element error should nullify the entire list
    FormatTest::builder()
        .schema(schema)
        .query("query { list { l2 } }")
        .response(json! {{
            "list": {
                "l2": ["abc", 1, { "foo": "bar"}, ["aaa"], "def"],
                "name": 1,
            },
        }})
        .expected(json! {{
            "list": {
                "l2": null,
            },
        }})
        .expected_extensions(json! {{
            "valueCompletion": [
                {
                    "message": "Cannot return null for non-nullable array element of type String at index 1",
                    "path": ["list", "l2", 1]
                }
            ]
        }},)
        .test();

    FormatTest::builder()
        .schema(schema)
        .query("query { list { l2 } }")
        .response(json! {{
            "list": {
                "l2": ["abc", "def"],
                "name": 1,
            },
        }})
        .expected(json! {{
            "list": {
                "l2": ["abc", "def"],
            },
        }})
        .test();

    // l3: nullable list of nullable elements
    // any element error should stop at the list elements
    FormatTest::builder()
        .schema(schema)
        .query("query { list { l3 } }")
        .response(json! {{
            "list": {
                "l3": ["abc", 1, { "foo": "bar"}, ["aaa"], "def"],
                "name": 1,
            },
        }})
        .expected(json! {{
            "list": {
                "l3": ["abc", null, null, null, "def"],
            },
        }})
        .test();

    // non null l3 expected a list, got an int, parrent element should be null
    FormatTest::builder()
        .schema(schema)
        .query("query { list { l3 } }")
        .response(json! {{
            "list": {
                "l3": 1,
            },
        }})
        .expected(json! {{
            "list": null,
        }})
        .test();

    // l4: non nullable list of non nullable elements
    // any element error should nullify the entire list,
    // which will nullify the parent element
    FormatTest::builder()
        .schema(schema)
        .query("query { list { l4 } }")
        .response(json! {{
            "list": {
                "l4": ["abc", 1, { "foo": "bar"}, ["aaa"], "def"],
            },
        }})
        .expected(json! {{
            "list": null,
        }})
        .test();

    FormatTest::builder()
        .schema(schema)
        .query("query { list { l4 } }")
        .response(json! {{
            "list": {
                "l4": ["abc", "def"],
            },
        }})
        .expected(json! {{
            "list": {
                "l4": ["abc", "def"],
            },
        }})
        .test();

    FormatTest::builder()
        .schema(schema)
        .query("query { list { l4 } }")
        .response(json! {{
            "list": {
                "l4": 1,
            },
        }})
        .expected(json! {{
            "list": null,
        }})
        .test();
}

#[test]
fn filter_nested_object_errors() {
    let schema = "type Query {
        me: User
    }

    type User {
        id: String!
        name: String
        reviews1: [Review]
        reviews2: [Review!]
        reviews3: [Review!]!
    }
    
    type Review {
        text1: String
        text2: String!
    }
    ";

    // nullable parent and child elements
    // child errors should stop at the child's level
    let query_review1_text1 = "query  { me { id reviews1 { text1 } } }";
    FormatTest::builder()
        .schema(schema)
        .query(query_review1_text1)
        .response(json! {{
            "me": {
                "id": "a",
                "name": 1,
                "reviews1": [ { } ],
            },
        }})
        .expected(json! {{
            "me": {
                "id": "a",
                "reviews1": [ {
                    "text1": null,
                } ],
            },
        }})
        .test();

    // nullable text1 was null
    FormatTest::builder()
        .schema(schema)
        .query(query_review1_text1)
        .response(json! {{
            "me": {
                "id": "a",
                "name": 1,
                "reviews1": [ { "text1": null } ],
            },
        }})
        .expected(json! {{
            "me": {
                "id": "a",
                "reviews1": [ { "text1": null } ],
            },
        }})
        .test();

    // nullable text1 expected a string, got an int, so text1 is nullified
    FormatTest::builder()
        .schema(schema)
        .query(query_review1_text1)
        .response(json! {{
            "me": {
                "id": "a",
                "name": 1,
                "reviews1": [ { "text1": 1 } ],
            },
        }})
        .expected(json! {{
            "me": {
                "id": "a",
                "reviews1": [ { "text1": null } ],
            },
        }})
        .test();

    // text2 is non null so errors should nullify reviews1 element
    let query_review1_text2 = "query  { me { id reviews1 { text2 } } }";
    // text2 was absent, reviews1 element should be nullified
    FormatTest::builder()
        .schema(schema)
        .query(query_review1_text2)
        .response(json! {{
            "me": {
                "id": "a",
                "name": 1,
                "reviews1": [ { } ],
            },
        }})
        .expected(json! {{
            "me": {
                "id": "a",
                "reviews1": [ null ],
            },
        }})
        .expected_extensions(json! {{
            "valueCompletion": [
                {
                    "message": "Cannot return null for non-nullable field Review.text2",
                    "path": ["me", "reviews1", 0]
                }
            ]
        }})
        .test();

    // text2 was null, reviews1 element should be nullified
    FormatTest::builder()
        .schema(schema)
        .query(query_review1_text2)
        .response(json! {{
            "me": {
                "id": "a",
                "name": 1,
                "reviews1": [ { "text2": null } ],
            },
        }})
        .expected(json! {{
            "me": {
                "id": "a",
                "reviews1": [ null ],
            },
        }})
        .test();

    // text2 expected a string, got an int, text2 is nullified, reviews1 element should be nullified
    FormatTest::builder()
        .schema(schema)
        .query(query_review1_text2)
        .response(json! {{
            "me": {
                "id": "a",
                "name": 1,
                "reviews1": [ { "text2": 1 } ],
            },
        }})
        .expected(json! {{
            "me": {
                "id": "a",
                "reviews1": [ null ],
            },
        }})
        .test();

    // reviews2: [Review!]
    // reviews2 elements are non null, so any error there should nullify the entire list
    let query_review2_text1 = "query  { me { id reviews2 { text1 } } }";
    // nullable text1 was absent
    FormatTest::builder()
        .schema(schema)
        .query(query_review2_text1)
        .response(json! {{
            "me": {
                "id": "a",
                "name": 1,
                "reviews2": [ { } ],
            },
        }})
        .expected(json! {{
            "me": {
                "id": "a",
                "reviews2": [ {
                    "text1": null,
                } ],
            },
        }})
        .test();

    // nullable text1 was null
    FormatTest::builder()
        .schema(schema)
        .query(query_review2_text1)
        .response(json! {{
            "me": {
                "id": "a",
                "name": 1,
                "reviews2": [ { "text1": null } ],
            },
        }})
        .expected(json! {{
            "me": {
                "id": "a",
                "reviews2": [ { "text1": null } ],
            },
        }})
        .test();

    // nullable text1 expected a string, got an int
    FormatTest::builder()
        .schema(schema)
        .query(query_review2_text1)
        .response(json! {{
            "me": {
                "id": "a",
                "name": 1,
                "reviews2": [ { "text1": 1 } ],
            },
        }})
        .expected(json! {{
            "me": {
                "id": "a",
                "reviews2": [ { "text1": null } ],
            },
        }})
        .test();

    // text2 is non null
    let query_review2_text2 = "query  { me { id reviews2 { text2 } } }";
    // text2 was absent, so the reviews2 element is nullified, so reviews2 is nullified
    FormatTest::builder()
        .schema(schema)
        .query(query_review2_text2)
        .response(json! {{
            "me": {
                "id": "a",
                    "name": 1,
                    "reviews2": [ { } ],
            },
        }})
        .expected(json! {{
            "me": {
                "id": "a",
                "reviews2": null,
            },
        }})
        .test();

    // text2 was null, so the reviews2 element is nullified, so reviews2 is nullified
    FormatTest::builder()
        .schema(schema)
        .query(query_review2_text2)
        .response(json! {{
            "me": {
                "id": "a",
                "name": 1,
                "reviews2": [ { "text2": null } ],
            },
        }})
        .expected(json! {{
            "me": {
                "id": "a",
                "reviews2": null,
            },
        }})
        .test();

    // text2 expected a string, got an int, so the reviews2 element is nullified, so reviews2 is nullified
    FormatTest::builder()
        .schema(schema)
        .query(query_review2_text2)
        .response(json! {{
            "me": {
                "id": "a",
                "name": 1,
                "reviews2": [ { "text2": 1 } ],
            },
        }})
        .expected(json! {{
            "me": {
                "id": "a",
                "reviews2": null,
            },
        }})
        .test();

    //reviews3: [Review!]!
    // reviews3 is non null, and its elements are non null
    let query_review3_text1 = "query  { me { id reviews3 { text1 } } }";

    // nullable text1 was absent
    FormatTest::builder()
        .schema(schema)
        .query(query_review3_text1)
        .response(json! {{
            "me": {
                "id": "a",
                    "name": 1,
                    "reviews3": [ { } ],
            },
        }})
        .expected(json! {{
            "me": {
                "id": "a",
                "reviews3": [ {
                    "text1": null,
                } ],
            },
        }})
        .test();

    // nullable text1 was null
    FormatTest::builder()
        .schema(schema)
        .query(query_review3_text1)
        .response(json! {{
            "me": {
                "id": "a",
                "name": 1,
                "reviews3": [ { "text1": null } ],
            },
        }})
        .expected(json! {{
            "me": {
                "id": "a",
                "reviews3": [ { "text1": null } ],
            },
        }})
        .test();

    // nullable text1 expected a string, got an int
    FormatTest::builder()
        .schema(schema)
        .query(query_review3_text1)
        .response(json! {{
            "me": {
                "id": "a",
                "name": 1,
                "reviews3": [ { "text1": 1 } ],
            },
        }})
        .expected(json! {{
            "me": {
                "id": "a",
                "reviews3": [ { "text1": null } ],
            },
        }})
        .test();

    // reviews3 is non null, and its elements are non null, text2 is non null
    let query_review3_text2 = "query  { me { id reviews3 { text2 } } }";

    // text2 was absent, nulls should propagate up to the operation
    FormatTest::builder()
        .schema(schema)
        .query(query_review3_text2)
        .response(json! {{
            "me": {
                "id": "a",
                "name": 1,
                "reviews3": [ { } ],
            },
        }})
        .expected(json! {{
            "me": null,

        }})
        .test();

    // text2 was null, nulls should propagate up to the operation
    FormatTest::builder()
        .schema(schema)
        .query(query_review3_text2)
        .response(json! {{
            "me": {
                "id": "a",
                "name": 1,
                "reviews3": [ { "text2": null } ],
            },
        }})
        .expected(json! {{
            "me": null,

        }})
        .test();

    // text2 expected a string, got an int, nulls should propagate up to the operation
    FormatTest::builder()
        .schema(schema)
        .query(query_review3_text2)
        .response(json! {{
            "me": {
                "id": "a",
                "name": 1,
                "reviews3": [ { "text2": 1 } ],
            },
        }})
        .expected(json! {{
            "me": null,

        }})
        .test();
}

#[test]
fn filter_alias_errors() {
    let schema = "type Query {
        me: User
    }

    type User {
        id: String!
        name: String
    }";
    let query = "query  { me { id identifiant:id } }";

    // both aliases got valid values
    FormatTest::builder()
        .schema(schema)
        .query(query)
        .response(json! {{
            "me": {
                "id": "a",
                "identifiant": "b",
            },
        }})
        .expected(json! {{
            "me": {
                "id": "a",
                "identifiant": "b",
            },
        }})
        .test();

    // non null identifiant expected a string, got an int, the operation should be null
    FormatTest::builder()
        .schema(schema)
        .query(query)
        .response(json! {{
            "me": {
                "id": "a",
                "identifiant": 1,
            },
        }})
        .expected(json! {{
           "me": null,
        }})
        .test();

    // non null identifiant was null, the operation should be null
    FormatTest::builder()
        .schema(schema)
        .query(query)
        .response(json! {{
            "me": {
                "id": "a",
                "identifiant": null,
            },
        }})
        .expected(json! {{
           "me": null,
        }})
        .test();

    // non null identifiant was absent, the operation should be null
    FormatTest::builder()
        .schema(schema)
        .query(query)
        .response(json! {{
            "me": {
                "id": "a",
            },
        }})
        .expected(json! {{
           "me": null,
        }})
        .test();

    let query2 = "query  { me { name name2:name } }";

    // both aliases got valid values
    FormatTest::builder()
        .schema(schema)
        .query(query2)
        .response(json! {{
            "me": {
                "name": "a",
                "name2": "b",
            },
        }})
        .expected(json! {{
            "me": {
                "name": "a",
                "name2": "b",
            },
        }})
        .test();

    // nullable name2 expected a string, got an int, name2 should be null
    FormatTest::builder()
        .schema(schema)
        .query(query2)
        .response(json! {{
            "me": {
                "name": "a",
                "name2": 1,
            },
        }})
        .expected(json! {{
            "me": {
                "name": "a",
                "name2": null,
            },
        }})
        .test();

    // nullable name2 was null
    FormatTest::builder()
        .schema(schema)
        .query(query2)
        .response(json! {{
            "me": {
                "name": "a",
                "name2": null,
            },
        }})
        .expected(json! {{
            "me": {
                "name": "a",
                "name2": null,
            },
        }})
        .test();

    // nullable name2 was absent
    FormatTest::builder()
        .schema(schema)
        .query(query2)
        .response(json! {{
            "me": {
                "name": "a",
            },
        }})
        .expected(json! {{
            "me": {
                "name": "a",
                "name2": null,
            },
        }})
        .test();
}

#[test]
fn filter_scalar_errors() {
    let schema = "type Query {
        me: User
    }

    type User {
        id: String!
        a: A
        b: A!
    }
    
    scalar A
    ";

    let query = "query  { me { id a } }";

    // scalar a is present, no further validation
    FormatTest::builder()
        .schema(schema)
        .query(query)
        .response(json! {{
            "me": {
                "id": "a",
                "a": "hello",
            },
        }})
        .expected(json! {{
            "me": {
                "id": "a",
                "a": "hello",
            },
        }})
        .test();

    // scalar a is present, no further validation=
    FormatTest::builder()
        .schema(schema)
        .query(query)
        .response(json! {{
            "me": {
                "id": "a",
                "a": {
                    "field": 1234,
                },
            },
        }})
        .expected(json! {{
            "me": {
                "id": "a",
                "a": {
                    "field": 1234,
                },
            },
        }})
        .test();

    let query2 = "query  { me { id b } }";

    // non null scalar b is present, no further validation
    FormatTest::builder()
        .schema(schema)
        .query(query2)
        .response(json! {{
            "me": {
                "id": "a",
                "b": "hello",
            },
        }})
        .expected(json! {{
            "me": {
                "id": "a",
                "b": "hello",
            },
        }})
        .test();

    // non null scalar b is present, no further validation
    FormatTest::builder()
        .schema(schema)
        .query(query2)
        .response(json! {{
            "me": {
                "id": "a",
                "b": {
                    "field": 1234,
                },
            },
        }})
        .expected(json! {{
            "me": {
                "id": "a",
                "b": {
                    "field": 1234,
                },
            },
        }})
        .test();

    // non null scalar b was null, the operation should be null
    FormatTest::builder()
        .schema(schema)
        .query(query2)
        .response(json! {{
            "me": {
                "id": "a",
                "b": null,
            },
        }})
        .expected(json! {{
            "me": null,
        }})
        .test();

    // non null scalar b was absent, the operation should be null
    FormatTest::builder()
        .schema(schema)
        .query(query2)
        .response(json! {{
            "me": {
                "id": "a",
            },
        }})
        .expected(json! {{
            "me": null,
        }})
        .test();
}

#[test]
fn filter_enum_errors() {
    let schema = "type Query {
        me: User
    }

    type User {
        id: String!
        a: A
        b: A!
    }

    enum A {
        X
        Y
        Z
    }";

    let query_a = "query  { me { id a } }";

    // enum a got a correct value
    FormatTest::builder()
        .schema(schema)
        .query(query_a)
        .response(json! {{
            "me": {
                "id": "a",
                "a": "X",
            },
        }})
        .expected(json! {{
            "me": {
                "id": "a",
                "a": "X",
            },
        }})
        .test();

    // nullable enum a expected "X", "Y" or "Z", got another string, a should be null
    FormatTest::builder()
        .schema(schema)
        .query(query_a)
        .response(json! {{
            "me": {
                "id": "a",
                "a": "hello",
            },
        }})
        .expected(json! {{
            "me": {
                "id": "a",
                "a": null,
            },
        }})
        .test();

    // nullable enum a was null
    FormatTest::builder()
        .schema(schema)
        .query(query_a)
        .response(json! {{
            "me": {
                "id": "a",
                "a": null,
            },
        }})
        .expected(json! {{
            "me": {
                "id": "a",
                "a": null,
            },
        }})
        .test();

    let query_b = "query  { me { id b } }";

    // non nullable enum b got a correct value
    FormatTest::builder()
        .schema(schema)
        .query(query_b)
        .response(json! {{
            "me": {
                "id": "a",
                "b": "X",
            },
        }})
        .expected(json! {{
            "me": {
                "id": "a",
                "b": "X",
            },
        }})
        .test();

    // non nullable enum b expected "X", "Y" or "Z", got another string, b and the operation should be null
    FormatTest::builder()
        .schema(schema)
        .query(query_b)
        .response(json! {{
            "me": {
                "id": "a",
                "b": "hello",
            },
        }})
        .expected(json! {{
            "me": null,
        }})
        .test();

    // non nullable enum b was null, the operation should be null
    FormatTest::builder()
        .schema(schema)
        .query(query_b)
        .response(json! {{
            "me": {
                "id": "a",
                "b": null,
            },
        }})
        .expected(json! {{
            "me": null,
        }})
        .test();
}

#[test]
fn filter_interface_errors() {
    let schema = "type Query {
        me: NamedEntity
    }

    interface NamedEntity {
        name: String
        name2: String!
    }

    type User implements NamedEntity {
        name: String
        name2: String!
    }

    type User2 implements NamedEntity {
        name: String
        name2: String!
    }
    ";

    let query = "query  { me { name } }";

    // nullable name field got a correct value
    FormatTest::builder()
        .schema(schema)
        .query(query)
        .response(json! {{
            "me": {
                "name": "a",
            },
        }})
        .expected(json! {{
            "me": {
                "name": "a",
            },
        }})
        .test();

    // nullable name field was absent
    FormatTest::builder()
        .schema(schema)
        .query(query)
        .response(json! {{
            "me": { },
        }})
        .expected(json! {{
            "me": {
                "name": null,
            },
        }})
        .test();

    // nullable name field was null
    FormatTest::builder()
        .schema(schema)
        .query(query)
        .response(json! {{
            "me": {
                "name": null,
            },
        }})
        .expected(json! {{
            "me": {
                "name": null,
            },
        }})
        .test();

    // nullable name field expected a string, got an int
    FormatTest::builder()
        .schema(schema)
        .query(query)
        .response(json! {{
            "me": {
                "name": 1,
            },
        }})
        .expected(json! {{
            "me": {
                "name": null,
            },
        }})
        .test();

    let query2 = "query  { me { name2 } }";

    // non nullable name2 field got a correct value
    FormatTest::builder()
        .schema(schema)
        .query(query2)
        .response(json! {{
            "me": {
                "name2": "a",
            },
        }})
        .expected(json! {{
            "me": {
                "name2": "a",
            },
        }})
        .test();

    // non nullable name2 field was absent, the operation should be null
    FormatTest::builder()
        .schema(schema)
        .query(query2)
        .response(json! {{
            "me": { },
        }})
        .expected(json! {{
            "me": null,
        }})
        .test();

    // non nullable name2 field was null, the operation should be null
    FormatTest::builder()
        .schema(schema)
        .query(query2)
        .response(json! {{
            "me": {
                "name2": null,
            },
        }})
        .expected(json! {{
            "me": null,
        }})
        .test();

    // non nullable name2 field expected a string, got an int, name2 and the operation should be null
    FormatTest::builder()
        .schema(schema)
        .query(query2)
        .response(json! {{
            "me": {
                "name2": 1,
            },
        }})
        .expected(json! {{
            "me": null,
        }})
        .test();

    // we should be able to handle duplicate fields even across fragments and interfaces
    FormatTest::builder()
        .schema(schema)
        .query("query { me { ... on User { name2 } name2 } }")
        .response(json! {{
            "me": {
                "__typename": "User",
                "name2": "a",
            },
        }})
        .expected(json! {{
            "me": {
                "name2": "a",
            },
        }})
        .test();
}

#[test]
fn filter_extended_interface_errors() {
    let schema = "type Query {
        me: NamedEntity
    }

    interface NamedEntity {
        name: String
    }

    type User implements NamedEntity {
        name: String
    }

    type User2 implements NamedEntity {
        name: String
    }

    extend interface NamedEntity {
        name2: String!
    }

    extend type User {
        name2: String!
    }

    extend type User2 {
        name2: String!
    }
    ";

    let query = "query  { me { name2 } }";

    // non nullable name2 got a correct value
    FormatTest::builder()
        .schema(schema)
        .query(query)
        .response(json! {{
            "me": {
                "name2": "a",
            },
        }})
        .expected(json! {{
            "me": {
                "name2": "a",
            },
        }})
        .test();

    // non nullable name2 was null, the operation should be null
    FormatTest::builder()
        .schema(schema)
        .query(query)
        .response(json! {{
            "me": {
                "name2": null,
            },
        }})
        .expected(json! {{
            "me": null,
        }})
        .test();

    // non nullable name2 was absent, the operation should be null
    FormatTest::builder()
        .schema(schema)
        .query(query)
        .response(json! {{
            "me": { },
        }})
        .expected(json! {{
            "me": null,
        }})
        .test();

    // non nullable name2 expected a string, got an int, the operation should be null
    FormatTest::builder()
        .schema(schema)
        .query(query)
        .response(json! {{
            "me": {
                "name2": 1,
            },
        }})
        .expected(json! {{
            "me": null,
        }})
        .test();
}

#[test]
fn filter_errors_top_level_fragment() {
    let schema = "type Query {
        get: Thing   
      }

      type Thing {
          name: String
          name2: String!
      }";

    // fragments can appear on top level queries
    let query = "{ ...frag } fragment frag on Query { __typename get { name } }";

    // nullable name got a correct value
    FormatTest::builder()
        .schema(schema)
        .query(query)
        .response(json! {{
            "get": {
                "name": "a",
                "other": "b"
            }
        }})
        .expected(json! {{
            "__typename": null,
            "get": {
                "name": "a",
            }
        }})
        .test();

    // nullable name was null
    FormatTest::builder()
        .schema(schema)
        .query(query)
        .response(json! {{
            "get": {"name": null, "other": "b"}
        }})
        .expected(json! {{
            "__typename": null,
            "get": {
                "name": null,
            }
        }})
        .test();

    let query2 = "{ ...frag2 } fragment frag2 on Query { __typename get { name2 } }";
    // non nullable name2 got a correct value
    FormatTest::builder()
        .schema(schema)
        .query(query2)
        .response(json! {{
            "get": {"name2": "a", "other": "b"}
        }})
        .expected(json! {{
            "__typename": null,
            "get": {
                "name2": "a",
            }
        }})
        .test();

    // non nullable name2 was null, the operation should be null
    FormatTest::builder()
        .schema(schema)
        .query(query2)
        .response(json! {{
            "get": {"name2": null, "other": "b"}
        }})
        .expected(json! {{
            "__typename": null,
            "get": null
        }})
        .test();

    let query3 = "{ ... on Query { __typename get { name } } }";
    // nullable name got a correct value
    FormatTest::builder()
        .schema(schema)
        .query(query3)
        .response(json! {{
            "get": {"name": "a", "other": "b"}
        }})
        .expected(json! {{
            "__typename": null,
            "get": {
                "name": "a",
            }
        }})
        .test();

    // nullable name was null
    FormatTest::builder()
        .schema(schema)
        .query(query3)
        .response(json! {{
            "get": {"name": null, "other": "b"}
        }})
        .expected(json! {{
            "__typename": null,
            "get": {
                "name": null,
            }
        }})
        .test();

    let query4 = "{ ... on Query { __typename get { name2 } } }";
    // non nullable name2 got a correct value
    FormatTest::builder()
        .schema(schema)
        .query(query4)
        .response(json! {{
            "get": {"name2": "a", "other": "b"}
        }})
        .expected(json! {{
            "__typename": null,
            "get": {
                "name2": "a",
            }
        }})
        .test();

    // non nullable name2 was null, the operation should be null
    FormatTest::builder()
        .schema(schema)
        .query(query4)
        .response(json! {{
            "get": {"name2": null, "other": "b"}
        }})
        .expected(json! {{
            "__typename": null,
            "get": null,
        }})
        .test();
}

#[test]
fn merge_selections() {
    let schema = "type Query {
        get: Product
    }

    type Product {
        id: String!
        name: String
        review: Review
    }
    
    type Review {
        id: String!
        body: String
    }";

    // duplicate operation name
    FormatTest::builder()
        .schema(schema)
        .query(
            "query  {
            get {
                id
            }
            get {
                name
            }
        }",
        )
        .response(json! {{
            "get": {
                "id": "a",
                "name": "Chair",
            },
        }})
        .expected(json! {{
            "get": {
                "id": "a",
                "name": "Chair",
            },
        }})
        .test();

    // merge nested selection
    FormatTest::builder()
        .schema(schema)
        .query(
            "query  {
            get {
                id
                review {
                    id
                }

                ... on Product {
                    review {
                        body
                    }
                }
            }
            get {
                name
            }
        }",
        )
        .response(json! {{
            "get": {
                "id": "a",
                "review": {
                    "__typename": "Review",
                    "id": "b",
                    "body": "hello",
                }
            },
        }})
        .expected(json! {{
            "get": {
                "id": "a",
                "review": {
                    "id": "b",
                    "body": "hello",
                },
                "name": null,
            },
        }})
        .test();
}

#[test]
fn it_parses_default_floats() {
    let schema = with_supergraph_boilerplate(
        r#"
        type Query {
            name: String
        }

        input WithAllKindsOfFloats {
            a_regular_float: Float = 1.2
            an_integer_float: Float = 1234
            a_float_that_doesnt_fit_an_int: Float = 9876543210
        }
        "#,
        "Query",
    );

    let schema = Schema::parse_test(&schema, &Default::default()).unwrap();
    let value = schema.type_system.definitions.input_objects["WithAllKindsOfFloats"]
        .field("a_float_that_doesnt_fit_an_int")
        .unwrap()
        .default_value()
        .unwrap();
    assert_eq!(f64::try_from(value).unwrap() as i64, 9876543210);
}

#[test]
fn it_statically_includes() {
    let schema = with_supergraph_boilerplate(
        "type Query {
        name: String
        review: Review
        product: Product
    }

    type Product {
        id: String!
        name: String
        review: Review
    }

    type Review {
        id: String!
        body: String
    }",
        "Query",
    );
    let schema = Schema::parse_test(&schema, &Default::default()).expect("could not parse schema");

    let query = Query::parse(
        "query  {
            name @include(if: false)
            review @include(if: false) {
                body
            }
            product @include(if: true) {
                name
            }
        }",
        &schema,
        &Default::default(),
    )
    .expect("could not parse query");
    assert_eq!(query.operations.len(), 1);
    let operation = &query.operations[0];
    assert_eq!(operation.selection_set.len(), 1);
    match operation.selection_set.get(0).unwrap() {
        Selection::Field { name, .. } => assert_eq!(name, &ByteString::from("product")),
        _ => panic!("expected a field"),
    }

    let query = Query::parse(
        "query  {
            name @include(if: false)
            review {
                body
            }
            product @include(if: true) {
                name
            }
        }",
        &schema,
        &Default::default(),
    )
    .expect("could not parse query");

    assert_eq!(query.operations.len(), 1);
    let operation = &query.operations[0];
    assert_eq!(operation.selection_set.len(), 2);
    match operation.selection_set.get(0).unwrap() {
        Selection::Field { name, .. } => assert_eq!(name, &ByteString::from("review")),
        _ => panic!("expected a field"),
    }
    match operation.selection_set.get(1).unwrap() {
        Selection::Field { name, .. } => assert_eq!(name, &ByteString::from("product")),
        _ => panic!("expected a field"),
    }

    // Inline fragment
    let query = Query::parse(
        "query  {
            name @include(if: false)
            ... @include(if: false) {
                review {
                    body
                }
            }
            product @include(if: true) {
                name
            }
        }",
        &schema,
        &Default::default(),
    )
    .expect("could not parse query");

    assert_eq!(query.operations.len(), 1);
    let operation = &query.operations[0];
    assert_eq!(operation.selection_set.len(), 1);
    match operation.selection_set.get(0).unwrap() {
        Selection::Field {
            name,
            selection_set: Some(selection_set),
            ..
        } => {
            assert_eq!(name, &ByteString::from("product"));
            assert_eq!(selection_set.len(), 1);
        }
        _ => panic!("expected a field"),
    }

    // Fragment spread
    let query = Query::parse(
        "
        fragment ProductName on Product {
            name
        }
        query  {
            name @include(if: false)
            review {
                body
            }
            product @include(if: true) {
                ...ProductName @include(if: false)
            }
        }",
        &schema,
        &Default::default(),
    )
    .expect("could not parse query");

    assert_eq!(query.operations.len(), 1);
    let operation = &query.operations[0];
    assert_eq!(operation.selection_set.len(), 2);
    match operation.selection_set.get(0).unwrap() {
        Selection::Field { name, .. } => assert_eq!(name, &ByteString::from("review")),
        _ => panic!("expected a field"),
    }
    match operation.selection_set.get(1).unwrap() {
        Selection::Field {
            name,
            selection_set: Some(selection_set),
            ..
        } => {
            assert_eq!(name, &ByteString::from("product"));
            assert_eq!(selection_set.len(), 0);
        }
        _ => panic!("expected a field"),
    }
}

#[test]
fn it_statically_skips() {
    let schema = with_supergraph_boilerplate(
        "type Query {
        name: String
        review: Review
        product: Product
    }

    type Product {
        id: String!
        name: String
        review: Review
    }

    type Review {
        id: String!
        body: String
    }",
        "Query",
    );
    let schema = Schema::parse_test(&schema, &Default::default()).expect("could not parse schema");

    let query = Query::parse(
        "query  {
            name @skip(if: true)
            review @skip(if: true) {
                body
            }
            product @skip(if: false) {
                name
            }
        }",
        &schema,
        &Default::default(),
    )
    .expect("could not parse query");
    assert_eq!(query.operations.len(), 1);
    let operation = &query.operations[0];
    assert_eq!(operation.selection_set.len(), 1);
    match operation.selection_set.get(0).unwrap() {
        Selection::Field { name, .. } => assert_eq!(name, &ByteString::from("product")),
        _ => panic!("expected a field"),
    }

    let query = Query::parse(
        "query  {
            name @skip(if: true)
            review {
                body
            }
            product @skip(if: false) {
                name
            }
        }",
        &schema,
        &Default::default(),
    )
    .expect("could not parse query");

    assert_eq!(query.operations.len(), 1);
    let operation = &query.operations[0];
    assert_eq!(operation.selection_set.len(), 2);
    match operation.selection_set.get(0).unwrap() {
        Selection::Field { name, .. } => assert_eq!(name, &ByteString::from("review")),
        _ => panic!("expected a field"),
    }
    match operation.selection_set.get(1).unwrap() {
        Selection::Field { name, .. } => assert_eq!(name, &ByteString::from("product")),
        _ => panic!("expected a field"),
    }

    // Inline fragment
    let query = Query::parse(
        "query  {
            name @skip(if: true)
            ... @skip(if: true) {
                review {
                    body
                }
            }
            product @skip(if: false) {
                name
            }
        }",
        &schema,
        &Default::default(),
    )
    .expect("could not parse query");

    assert_eq!(query.operations.len(), 1);
    let operation = &query.operations[0];
    assert_eq!(operation.selection_set.len(), 1);
    match operation.selection_set.get(0).unwrap() {
        Selection::Field {
            name,
            selection_set: Some(selection_set),
            ..
        } => {
            assert_eq!(name, &ByteString::from("product"));
            assert_eq!(selection_set.len(), 1);
        }
        _ => panic!("expected a field"),
    }

    // Fragment spread
    let query = Query::parse(
        "
        fragment ProductName on Product {
            name
        }
        query  {
            name @skip(if: true)
            review {
                body
            }
            product @skip(if: false) {
                ...ProductName @skip(if: true)
            }
        }",
        &schema,
        &Default::default(),
    )
    .expect("could not parse query");

    assert_eq!(query.operations.len(), 1);
    let operation = &query.operations[0];
    assert_eq!(operation.selection_set.len(), 2);
    match operation.selection_set.get(0).unwrap() {
        Selection::Field { name, .. } => assert_eq!(name, &ByteString::from("review")),
        _ => panic!("expected a field"),
    }
    match operation.selection_set.get(1).unwrap() {
        Selection::Field {
            name,
            selection_set: Some(selection_set),
            ..
        } => {
            assert_eq!(name, &ByteString::from("product"));
            assert_eq!(selection_set.len(), 0);
        }
        _ => panic!("expected a field"),
    }
}

#[test]
fn it_should_fail_with_empty_selection_set() {
    let schema = with_supergraph_boilerplate(
        "type Query {
        product: Product
    }

    type Product {
        id: String!
        name: String
    }",
        "Query",
    );
    let schema = Schema::parse_test(&schema, &Default::default()).expect("could not parse schema");

    let _query_error = Query::parse(
        "query  {
            product {
            }
        }",
        &schema,
        &Default::default(),
    )
    .expect_err("should not parse query");
}

#[test]
fn skip() {
    let schema = "type Query {
        get: Product
    }

    type Product {
        id: String!
        name: String
        review: Review
    }

    type Review {
        id: String!
        body: String
    }";

    // duplicate operation name
    FormatTest::builder()
        .schema(schema)
        .query(
            "query  {
                get {
                    name @skip(if: true)
                }
                get @skip(if: false) {
                    id 
                    review {
                        id
                    }
                }
            }",
        )
        .response(json! {{
            "get": {
                "id": "a",
                "name": "Chair",
                "review": {
                    "id": "b",
                }
            },
        }})
        .expected(json! {{
            "get": {
                "id": "a",
                "review": {
                    "id": "b",
                }
            },
        }})
        .test();

    // skipped non null
    FormatTest::builder()
        .schema(schema)
        .query(
            "query  {
            get {
                id @skip(if: true)
                name
            }
        }",
        )
        .response(json! {{
            "get": {
                "name": "Chair",
            },
        }})
        .expected(json! {{
            "get": {
                "name": "Chair",
            },
        }})
        .test();

    // inline fragment
    FormatTest::builder()
        .schema(schema)
        .query(
            "query  {
            get {
                id
                ... on Product @skip(if: true) {
                    name
                }
            }
        }",
        )
        .response(json! {{
            "get": {
                "id": "a",
                "name": "Chair",
            },
        }})
        .expected(json! {{
            "get": {
                "id": "a",
            },
        }})
        .test();

    FormatTest::builder()
        .schema(schema)
        .query(
            "query  {
        get {
            id
            ... on Product @skip(if: false) {
                name
            }
        }
    }",
        )
        .response(json! {{
            "get": {
                "id": "a",
                "name": "Chair",
            },
        }})
        .expected(json! {{
            "get": {
                "id": "a",
                "name": "Chair",
            },
        }})
        .test();

    // directive on fragment spread
    FormatTest::builder()
        .schema(schema)
        .query(
            "query  {
            get {
                id
                ...test @skip(if: false)
            }
        }

        fragment test on Product {
            nom: name
            name @skip(if: true)
        }",
        )
        .response(json! {{
            "get": {
                "id": "a",
                "nom": "Chaise",
                "name": "Chair",
            },
        }})
        .expected(json! {{
            "get": {
                "id": "a",
                "nom": "Chaise",
            },
        }})
        .test();

    FormatTest::builder()
        .schema(schema)
        .query(
            "query  {
                get {
                    id
                    ...test @skip(if: true)
                }
            }

            fragment test on Product {
                nom: name
                name @skip(if: true)
            }",
        )
        .response(json! {{
            "get": {
                "id": "a",
                "nom": "Chaise",
                "name": "Chair",
            },
        }})
        .expected(json! {{
            "get": {
                "id": "a",
            },
        }})
        .test();

    // variables
    // duplicate operation name
    FormatTest::builder()
        .schema(schema)
        .query(
            "query Example($shouldSkip: Boolean) {
                get {
                    id
                    name @skip(if: $shouldSkip)
                }
            }",
        )
        .response(json! {{
            "get": {
                "id": "a",
                "name": "Chair",
            },
        }})
        .operation("Example")
        .variables(json! {{
            "shouldSkip": true
        }})
        .expected(json! {{
            "get": {
                "id": "a",
            },
        }})
        .test();

    FormatTest::builder()
        .schema(schema)
        .query(
            "query Example($shouldSkip: Boolean) {
                get {
                    id
                    name @skip(if: $shouldSkip)
                }
            }",
        )
        .response(json! {{
            "get": {
                "id": "a",
                "name": "Chair",
            },
        }})
        .operation("Example")
        .variables(json! {{
            "shouldSkip": false
        }})
        .expected(json! {{
            "get": {
                "id": "a",
                "name": "Chair",
            },
        }})
        .test();

    // default variable value
    FormatTest::builder()
        .schema(schema)
        .query(
            "query Example($shouldSkip: Boolean) {
                get {
                    id
                    name @skip(if: $shouldSkip)
                }
            }",
        )
        .response(json! {{
            "get": {
                "id": "a",
                "name": "Chair",
            },
        }})
        .operation("Example")
        .variables(json! {{
            "shouldSkip": false
        }})
        .expected(json! {{
            "get": {
                "id": "a",
                "name": "Chair",
            },
        }})
        .test();

    FormatTest::builder()
        .schema(schema)
        .query(
            "query Example($shouldSkip: Boolean = true) {
                get {
                    id
                    name @skip(if: $shouldSkip)
                }
            }",
        )
        .response(json! {{
            "get": {
                "id": "a",
                "name": "Chair",
            },
        }})
        .operation("Example")
        .variables(json! {{
            "shouldSkip": false
        }})
        .expected(json! {{
            "get": {
                "id": "a",
                "name": "Chair",
            },
        }})
        .test();

    FormatTest::builder()
        .schema(schema)
        .query(
            "query Example($shouldSkip: Boolean = true) {
                get {
                    id
                    name @skip(if: $shouldSkip)
                }
            }",
        )
        .response(json! {{
            "get": {
                "id": "a",
                "name": "Chair",
            },
        }})
        .operation("Example")
        .expected(json! {{
            "get": {
                "id": "a",
            },
        }})
        .test();
}

#[test]
fn check_fragment_on_interface() {
    let schema = "type Query {
        get: Product
    }

    interface Product {
        id: String!
        name: String
    }

    type Vodka {
        id: String!
        name: String
    }

    type Beer implements Product {
        id: String!
        name: String
    }";

    FormatTest::builder()
        .schema(schema)
        .query(
            "fragment ProductBase on Product {
            __typename
            id
            name
          }
          query  {
              get {
                ...ProductBase
              }
          }",
        )
        .response(json! {{
            "get": {
                "__typename": "Beer",
                "id": "a",
                "name": "Asahi",
            },
        }})
        .expected(json! {{
            "get": {
                "__typename": "Beer",
                "id": "a",
                "name": "Asahi",
            },
        }})
        .test();

    FormatTest::builder()
        .schema(schema)
        .query(
            "fragment ProductBase on Product {
            id
            name
          }
          query  {
              get {
                ...ProductBase
              }
          }",
        )
        .response(json! {{
            "get": {
                "id": "a",
                    "name": "Asahi",
            },
        }})
        .expected(json! {{
            "get": {
                "id": "a",
                "name": "Asahi",
            },
        }})
        .test();

    FormatTest::builder()
        .schema(schema)
        .query(
            "query  {
                get {
                  ... on Product {
                    __typename
                    id
                    name
                  }
                }
            }",
        )
        .response(json! {{
            "get": {
                "__typename": "Beer",
                "id": "a",
                "name": "Asahi",
            },
        }})
        .expected(json! {{
            "get": {
                "__typename": "Beer",
                "id": "a",
                "name": "Asahi",
            },
        }})
        .test();

    FormatTest::builder()
        .schema(schema)
        .query(
            "query  {
            get {
              ... on Product {
                id
                name
              }
            }
        }",
        )
        .response(json! {{
            "get": {
                "id": "a",
                "name": "Asahi",
            },
        }})
        .expected(json! {{
            "get": {
                "id": "a",
                "name": "Asahi",
            },
        }})
        .test();

    // Make sure we do not return data for type that doesn't implement interface
    FormatTest::builder()
        .schema(schema)
        .query(
            "fragment ProductBase on Product {
            __typename
            id
            name
          }
          query  {
              get {
                ...ProductBase
              }
          }",
        )
        .response(json! {{
            "get": {
                "__typename": "Vodka",
                "id": "a",
                "name": "Crystal",
            },
        }})
        .expected(json! {{
            "get": { }
        }})
        .test();

    FormatTest::builder()
        .schema(schema)
        .query(
            "query  {
                get {
                  ... on Product {
                    __typename
                    id
                    name
                  }
                }
            }",
        )
        .response(json! {{
            "get": {
                "__typename": "Vodka",
                "id": "a",
                "name": "Crystal",
            },
        }})
        .expected(json! {{
            "get": { }
        }})
        .test();
}

#[test]
fn include() {
    let schema = "type Query {
        get: Product
    }

    type Product {
        id: String!
        name: String
        review: Review
    }

    type Review {
        id: String!
        body: String
    }";

    // duplicate operation name
    FormatTest::builder()
        .schema(schema)
        .query(
            "query  {
            get {
                name @include(if: false)
            }
            get @include(if: true) {
                id
                review {
                    id
                }
            }
        }",
        )
        .response(json! {{
            "get": {
                "id": "a",
                "name": "Chair",
                "review": {
                    "id": "b",
                }
            },
        }})
        .expected(json! {{
            "get": {
                "id": "a",
                "review": {
                    "id": "b",
                }
            },
        }})
        .test();

    // skipped non null
    FormatTest::builder()
        .schema(schema)
        .query(
            "query  {
            get {
                id @include(if: false)
                name
            }
        }",
        )
        .response(json! {{
            "get": {
                "name": "Chair",
            },
        }})
        .expected(json! {{
            "get": {
                "name": "Chair",
            },
        }})
        .test();

    // inline fragment
    FormatTest::builder()
        .schema(schema)
        .query(
            "query  {
            get {
                id
                ... on Product @include(if: false) {
                    name
                }
            }
        }",
        )
        .response(json! {{
            "get": {
                "id": "a",
                "name": "Chair",
            },
        }})
        .expected(json! {{
            "get": {
                "id": "a",
            },
        }})
        .test();

    FormatTest::builder()
        .schema(schema)
        .query(
            "query  {
            get {
                id
                ... on Product @include(if: true) {
                    name
                }
            }
        }",
        )
        .response(json! {{
            "get": {
                "id": "a",
                "name": "Chair",
            },
        }})
        .expected(json! {{
            "get": {
                "id": "a",
                "name": "Chair",
            },
        }})
        .test();

    // directive on fragment spread
    FormatTest::builder()
        .schema(schema)
        .query(
            "query  {
            get {
                id
                ...test @include(if: true)
            }
        }

        fragment test on Product {
            nom: name
            name @skip(if: true)
        }",
        )
        .response(json! {{
            "get": {
                "id": "a",
                "nom": "Chaise",
                "name": "Chair",
            },
        }})
        .expected(json! {{
            "get": {
                "id": "a",
                "nom": "Chaise",
            },
        }})
        .test();

    FormatTest::builder()
        .schema(schema)
        .query(
            "query  {
                get {
                    id
                    ...test @include(if: false)
                }
            }

            fragment test on Product {
                nom: name
                name @include(if: false)
            }",
        )
        .response(json! {{
            "get": {
                "id": "a",
                "nom": "Chaise",
                "name": "Chair",
            },
        }})
        .expected(json! {{
            "get": {
                "id": "a",
            },
        }})
        .test();

    // variables
    // duplicate operation name
    FormatTest::builder()
        .schema(schema)
        .query(
            "query Example($shouldInclude: Boolean) {
            get {
                id
                name @include(if: $shouldInclude)
            }
        }",
        )
        .response(json! {{
            "get": {
                "id": "a",
                "name": "Chair",
            },
        }})
        .operation("Example")
        .variables(json! {{
            "shouldInclude": false
        }})
        .expected(json! {{
            "get": {
                "id": "a",
            },
        }})
        .test();

    FormatTest::builder()
        .schema(schema)
        .query(
            "query Example($shouldInclude: Boolean) {
            get {
                id
                name @include(if: $shouldInclude)
            }
        }",
        )
        .response(json! {{
            "get": {
                "id": "a",
                "name": "Chair",
            },
        }})
        .operation("Example")
        .variables(json! {{
            "shouldInclude": true
        }})
        .expected(json! {{
            "get": {
                "id": "a",
                "name": "Chair",
            },
        }})
        .test();

    // default variable value
    FormatTest::builder()
        .schema(schema)
        .query(
            "query Example($shouldInclude: Boolean = false) {
            get {
                id
                name @include(if: $shouldInclude)
            }
        }",
        )
        .response(json! {{
            "get": {
                "id": "a",
                "name": "Chair",
            },
        }})
        .operation("Example")
        .variables(json! {{ }})
        .expected(json! {{
            "get": {
                "id": "a",
            },
        }})
        .test();

    FormatTest::builder()
        .schema(schema)
        .query(
            "query Example($shouldInclude: Boolean = false) {
            get {
                id
                name @include(if: $shouldInclude)
            }
        }",
        )
        .response(json! {{
            "get": {
                "id": "a",
                "name": "Chair",
            },
        }})
        .operation("Example")
        .variables(json! {{
            "shouldInclude": true
        }})
        .expected(json! {{
            "get": {
                "id": "a",
                "name": "Chair",
            },
        }})
        .test();

    FormatTest::builder()
        .schema(schema)
        .query(
            "query Example($shouldInclude: Boolean) {
            get {
                name
            }
            ...test @include(if: $shouldInclude)
        }

        fragment test on Query {
            get {
                id
            }
        }",
        )
        .response(json! {{
            "get": {
                "name": "a",
            },
        }})
        .operation("Example")
        .variables(json! {{
            "shouldInclude": false
        }})
        .expected(json! {{
            "get": {
               "name": "a",
            },
        }})
        .test();

    FormatTest::builder()
        .schema(schema)
        .query(
            "query Example($shouldInclude: Boolean) {
            get {
                name
            }
            ...test @include(if: $shouldInclude)
        }

        fragment test on Query {
            get {
                id
            }
        }",
        )
        .response(json! {{
            "get": {
                "name": "a",
            },
        }})
        .operation("Example")
        .expected(json! {{
            "get": null,
        }})
        .test();

    FormatTest::builder()
        .schema(schema)
        .query(
            "query Example($shouldInclude: Boolean) {
            get {
                name
            }
            ... @include(if: $shouldInclude) {
                get {
                    id
                }
            }
        }",
        )
        .response(json! {{
            "get": {
                "name": "a",
            },
        }})
        .operation("Example")
        .variables(json! {{
            "shouldInclude": false
        }})
        .expected(json! {{
            "get": {
               "name": "a",
            },
        }})
        .test();

    FormatTest::builder()
        .schema(schema)
        .query(
            "query Example($shouldInclude: Boolean) {
            get {
                name
            }
            ... @include(if: $shouldInclude) {
                get {
                    id
                }
            }
        }",
        )
        .response(json! {{
            "get": {
                "name": "a",
            },
        }})
        .operation("Example")
        .expected(json! {{
            "get": null,
        }})
        .test();
}

#[test]
fn skip_and_include() {
    let schema = "type Query {
        get: Product
    }

    type Product {
        id: String!
        name: String
    }";

    // combine skip and include
    // both of them must accept the field
    // ref: https://spec.graphql.org/October2021/#note-f3059
    FormatTest::builder()
        .schema(schema)
        .query(
            "query  {
            get {
                a:name @skip(if:true) @include(if: true)
                b:name @skip(if:true) @include(if: false)
                c:name @skip(if:false) @include(if: true)
                d:name @skip(if:false) @include(if: false)
            }
        }",
        )
        .response(json! {{
            "get": {
                "a": "a",
                "b": "b",
                "c": "c",
                "d": "d",
            },
        }})
        .expected(json! {{
            "get": {
                "c": "c",
            },
        }})
        .test();
}

#[test]
fn skip_and_include_multi_operation() {
    let schema = "type Query {
        get: Product
    }

    type Product {
        id: String!
        name: String
        bar: String
    }";

    // combine skip and include
    // both of them must accept the field
    // ref: https://spec.graphql.org/October2021/#note-f3059
    FormatTest::builder()
        .schema(schema)
        .query(
            "query  {
            get {
                a:name @skip(if:false)
            }
            get {
                a:name @skip(if:true)
            }
        }",
        )
        .response(json! {{
            "get": {
                "a": "a",
            },
        }})
        .expected(json! {{
            "get": {
                "a": "a",
            },
        }})
        .test();

    FormatTest::builder()
        .schema(schema)
        .query(
            "query  {
            get {
                a:name @skip(if:true)
            }
            get {
                a:name @skip(if:false)
            }
        }",
        )
        .response(json! {{
            "get": {
                "a": "a",
            },
        }})
        .expected(json! {{
            "get": {
                "a": "a",
            },
        }})
        .test();

    FormatTest::builder()
        .schema(schema)
        .query(
            "query  {
                get @skip(if: false) @include(if: false) {
                    a:name
                    bar
                }
                get @skip(if: false) {
                    a:name
                    a:name
                }
            }",
        )
        .response(json! {{
            "get": {
                "a": "a",
                "bar": "foo",
            },
        }})
        .expected(json! {{
            "get": {
                "a": "a",
            },
        }})
        .test();
}

#[test]
fn union_with_typename() {
    let schema = "type Query {
        get: ProductResult
    }

    type Product{
        symbol: String!
    }
    type ProductError{
        reason: String
    }
    union ProductResult = Product | ProductError
    ";

    FormatTest::builder()
        .schema(schema)
        .query(
            "query  {
            get {
                __typename
                ... on Product {
                  symbol
                }
                ... on ProductError {
                  reason
                }
            }
        }",
        )
        .response(json! {{
            "get": {
                "__typename": "Product",
                    "symbol": "1"
            },
        }})
        .expected(json! {{
            "get": {
                "__typename": "Product",
                "symbol": "1"
            },
        }})
        .test();
}

#[test]
fn inaccessible_on_interface() {
    let schema = "type Query
    {
        test_interface: Interface
        test_union: U
        test_enum: E
    }
    
    type Object implements Interface @inaccessible {
        foo: String
        other: String
    }

    type Object2 implements Interface {
        foo: String
        other: String @inaccessible
    }
      
    interface Interface {
        foo: String
    }

    type A @inaccessible {
        common: String
        a: String
    }

    type B {
        common: String
        b: String
    }
    
    union U = A | B

    enum E {
        X @inaccessible
        Y @inaccessible
        Z
    }
    ";

    FormatTest::builder()
        .schema(schema)
        .fed2()
        .query(
            "query  {
                test_interface {
                    __typename
                    foo
                }

                test_interface2: test_interface {
                    __typename
                    foo
                }

                test_union {
                    ...on B {
                        __typename
                        common
                    }
                }

                test_union2: test_union {
                    ...on B {
                        __typename
                        common
                    }
                }

                test_enum
                test_enum2: test_enum
            }",
        )
        .response(json! {{
            "test_interface": {
                "__typename": "Object",
                "foo": "bar",
                "other": "a"
            },

            "test_interface2": {
                "__typename": "Object2",
                "foo": "bar",
                "other": "a"
            },

            "test_union": {
                "__typename": "A",
                "common": "hello",
                "a": "A"
            },

            "test_union2": {
                "__typename": "B",
                "common": "hello",
                "b": "B"
            },

            "test_enum": "X",
            "test_enum2": "Z"
        }})
        .expected(json! {{
            "test_interface": null,
            "test_interface2": {
                "__typename": "Object2",
                "foo": "bar",
            },
            "test_union": null,
            "test_union2": {
                "__typename": "B",
                "common": "hello",
            },
            "test_enum": null,
            "test_enum2": "Z"
        }})
        .test();
}

#[test]
fn fragment_on_interface_on_query() {
    let schema = r#"schema
        @link(url: "https://specs.apollo.dev/link/v1.0")
        @link(url: "https://specs.apollo.dev/join/v0.2", for: EXECUTION)
        @link(url: "https://specs.apollo.dev/inaccessible/v0.2", for: SECURITY)
    {
        query: MyQueryObject
    }

    directive @join__field(graph: join__Graph!, requires: join__FieldSet, provides: join__FieldSet, type: String, external: Boolean, override: String, usedOverridden: Boolean) repeatable on FIELD_DEFINITION | INPUT_FIELD_DEFINITION
    directive @join__graph(name: String!, url: String!) on ENUM_VALUE
    directive @join__implements(graph: join__Graph!, interface: String!) repeatable on OBJECT | INTERFACE
    directive @join__type(graph: join__Graph!, key: join__FieldSet, extension: Boolean! = false, resolvable: Boolean! = true) repeatable on OBJECT | INTERFACE | UNION | ENUM | INPUT_OBJECT | SCALAR
    directive @link(url: String, as: String, for: link__Purpose, import: [link__Import]) repeatable on SCHEMA
    directive @inaccessible on FIELD_DEFINITION | OBJECT | INTERFACE | UNION | ENUM | ENUM_VALUE | SCALAR | INPUT_OBJECT | INPUT_FIELD_DEFINITION | ARGUMENT_DEFINITION

    scalar join__FieldSet
    scalar link__Import
    enum link__Purpose {
    """
    `SECURITY` features provide metadata necessary to securely resolve fields.
    """
    SECURITY

    """
    `EXECUTION` features provide metadata necessary for operation execution.
    """
    EXECUTION
    }

    enum join__Graph {
        TEST @join__graph(name: "test", url: "http://localhost:4001/graphql")
    }

    type MyQueryObject implements Interface {
        object: MyObject
        other: String
    }

    type MyObject {
        data: String
        foo: String
    }

    interface Interface {
        object: MyObject
    }"#;

    let query = "{
        ...FragmentTest
    }
    fragment FragmentTest on Interface {
        object {
            data
        }
    }";

    let schema = Schema::parse_test(schema, &Default::default()).expect("could not parse schema");
    let api_schema = schema.api_schema();
    let query = Query::parse(query, &schema, &Default::default()).expect("could not parse query");
    let mut response = Response::builder()
        .data(json! {{
            "object": {
                "__typename": "MyObject",
                "data": "a",
                "foo": "bar"
            }
        }})
        .build();

    query.format_response(
        &mut response,
        None,
        Default::default(),
        api_schema,
        BooleanValues { bits: 0 },
    );
    assert_eq_and_ordered!(
        response.data.as_ref().unwrap(),
        &json! {{
            "object": {
                "data": "a"
            }
        }}
    );
}

#[test]
fn fragment_on_interface() {
    let schema = "type Query
    {
        test_interface: Interface
    }

    interface Interface {
        foo: String
    }

    type MyTypeA implements Interface {
        foo: String
        something: String
    }

    type MyTypeB implements Interface {
        foo: String
        somethingElse: String!
    }
    ";

    FormatTest::builder()
        .schema(schema)
        .fed2()
        .query(
            "query  {
            test_interface {
                __typename
                foo
                ...FragmentA
            }
        }

        fragment FragmentA on MyTypeA {
            something
        }",
        )
        .response(json! {{
            "test_interface": {
                "__typename": "MyTypeA",
                "foo": "bar",
                "something": "something"
            }
        }})
        .expected(json! {{
            "test_interface": {
                "__typename": "MyTypeA",
                "foo": "bar",
                "something": "something"
            }
        }})
        .test();

    FormatTest::builder()
        .schema(schema)
        .fed2()
        .query(
            "query  {
            test_interface {
                __typename
                ...FragmentI
            }
        }

        fragment FragmentI on Interface {
            foo
        }",
        )
        .response(json! {{
            "test_interface": {
                "__typename": "MyTypeA",
                "foo": "bar"
            }
        }})
        .expected(json! {{
            "test_interface": {
                "__typename": "MyTypeA",
                "foo": "bar"
            }
        }})
        .test();

    FormatTest::builder()
        .schema(schema)
        .fed2()
        .query(
            "query  {
            test_interface {
                __typename
                foo
                ... on MyTypeA {
                    something
                }
                ... on MyTypeB {
                    somethingElse
                }
            }
        }",
        )
        .response(json! {{
            "test_interface": {
                "__typename": "MyTypeA",
                "foo": "bar",
                "something": "something"
            }
        }})
        .expected(json! {{
            "test_interface": {
                "__typename": "MyTypeA",
                "foo": "bar",
                "something": "something"
            }
        }})
        .test();

    FormatTest::builder()
        .schema(schema)
        .fed2()
        .query(
            "query  {
            test_interface {
                __typename
                foo
                ...FragmentB
            }
        }

        fragment FragmentB on MyTypeB {
            somethingElse
        }",
        )
        .response(json! {{
            "test_interface": {
                "__typename": "MyTypeA",
                "foo": "bar",
                "something": "something"
            }
        }})
        .expected(json! {{
            "test_interface": {
                "__typename": "MyTypeA",
                "foo": "bar",
            }
        }})
        .test();
}

#[test]
fn parse_introspection_query() {
    let schema = "type Query {
        foo: String
        stuff: Bar
        array: [Bar]
        baz: String
    }
    type Bar {
        bar: String
        baz: String
    }";

    let schema = with_supergraph_boilerplate(schema, "Query");
    let schema = Schema::parse_test(&schema, &Default::default()).expect("could not parse schema");
    let api_schema = schema.api_schema();

    let query = "{
        __type(name: \"Bar\") {
          name
          fields {
            name
            type {
              name
            }
          }
        }
      }";
    assert!(Query::parse(query, api_schema, &Default::default())
        .unwrap()
        .operations
        .get(0)
        .unwrap()
        .is_introspection());

    let query = "query {
        __schema {
          queryType {
            name
          }
        }
      }";

    assert!(Query::parse(query, api_schema, &Default::default())
        .unwrap()
        .operations
        .get(0)
        .unwrap()
        .is_introspection());

    let query = "query {
        __typename
      }";

    assert!(Query::parse(query, api_schema, &Default::default())
        .unwrap()
        .operations
        .get(0)
        .unwrap()
        .is_introspection());
}

#[test]
fn fragment_on_union() {
    let schema = "type Query {
        settings: ServiceSettings
    }

    type ServiceSettings {
        location: ServiceLocation
    }

    union ServiceLocation = AccountLocation | Address

    type AccountLocation {
        id: ID
        address: Address
    }

    type Address {
        city: String
    }";

    FormatTest::builder()
        .schema(schema)
        .fed2()
        .query(
            "{
            settings {
              location {
                ...SettingsLocation
              }
            }
          }

          fragment SettingsLocation on ServiceLocation {
            ... on Address {
              city
            }
             ... on AccountLocation {
               id
               address {
                 city
               }
             }
          }",
        )
        .response(json! {{
            "settings": {
                "location": {
                    "__typename": "AccountLocation",
                    "id": "1234"
                }
            }
        }})
        .expected(json! {{
            "settings": {
                "location": {
                    "id": "1234",
                    "address": null
                }
            }
        }})
        .test();
}

#[test]
fn fragment_on_interface_without_typename() {
    let schema = "type Query {
        inStore(key: String!): InStore!
    }

    type InStore implements CartQueryInterface {
        cart: Cart
        carts: CartQueryResult!
    }

    interface CartQueryInterface {
        carts: CartQueryResult!
        cart: Cart
    }

    type Cart {
        id: ID!
        total: Int!
    }

    type CartQueryResult {
        results: [Cart!]!
        total: Int!
    }";

    FormatTest::builder()
        .schema(schema)
        .fed2()
        .query(
            r#"query {
                mtb: inStore(key: "mountainbikes") {
                    ...CartFragmentTest
                }
            }

            fragment CartFragmentTest on CartQueryInterface {
                carts {
                    results {
                        id
                    }
                    total
                }
            }"#,
        )
        .response(json! {{
            "mtb": {
                "carts": {
                    "results": [{"id": "id"}],
                    "total": 1234
                },
                "cart": null
            }
        }})
        .expected(json! {{
            "mtb": {
                "carts": {
                    "results": [{"id": "id"}],
                    "total": 1234
                },
            }
        }})
        .test();

    // With inline fragment
    FormatTest::builder()
        .schema(schema)
        .fed2()
        .query(
            r#"query {
            mtb: inStore(key: "mountainbikes") {
                ... on CartQueryInterface {
                    carts {
                        results {
                            id
                        }
                        total
                    }
                }
            }
        }"#,
        )
        .response(json! {{
            "mtb": {
                "carts": {
                    "results": [{"id": "id"}],
                    "total": 1234
                },
                "cart": null
            }
        }})
        .expected(json! {{
            "mtb": {
                "carts": {
                    "results": [{"id": "id"}],
                    "total": 1234
                },
            }
        }})
        .test();
}

#[test]
fn query_operation_nullification() {
    FormatTest::builder()
        .schema(
            "type Query {
            get: Thing
        }
        type Thing {
            name: String
        }
        ",
        )
        .query(
            "{
                get {
                    name
                }
            }",
        )
        .response(json! {{ }})
        .expected(json! {{
            "get": null,
        }})
        .test();

    FormatTest::builder()
        .schema(
            "type Query {
                get: Thing
            }
            type Thing {
                name: String
            }",
        )
        .query(
            "query {
                ...F
             }
             
             fragment F on Query {
                 get {
                     name
                 }
             }",
        )
        .response(json! {{ }})
        .expected(json! {{
            "get": null,
        }})
        .test();

    FormatTest::builder()
        .schema(
            "type Query {
            get: Thing
        }
        type Thing {
            name: String
        }",
        )
        .query(
            "query {
            ... on Query {
             get {
                 name
             }
            }
         }",
        )
        .response(json! {{ }})
        .expected(json! {{
            "get": null,
        }})
        .test();

    FormatTest::builder()
        .schema(
            "type Query {
            get: Thing!
        }
        type Thing {
            name: String
        }",
        )
        .query(
            "{
            get {
                name
            }
        }",
        )
        .response(json! {{ }})
        .expected(Value::Null)
        .test();
}

#[test]
fn test_error_path_works_across_inline_fragments() {
    let schema = Schema::parse_test(
        r#"
    schema
        @link(url: "https://specs.apollo.dev/link/v1.0")
        @link(url: "https://specs.apollo.dev/join/v0.2", for: EXECUTION)
    {
        query: Query
    }

    directive @join__field(graph: join__Graph!, requires: join__FieldSet, provides: join__FieldSet, type: String, external: Boolean, override: String, usedOverridden: Boolean) repeatable on FIELD_DEFINITION | INPUT_FIELD_DEFINITION
    directive @join__graph(name: String!, url: String!) on ENUM_VALUE
    directive @join__implements(graph: join__Graph!, interface: String!) repeatable on OBJECT | INTERFACE
    directive @join__type(graph: join__Graph!, key: join__FieldSet, extension: Boolean! = false, resolvable: Boolean! = true) repeatable on OBJECT | INTERFACE | UNION | ENUM | INPUT_OBJECT | SCALAR
    directive @link(url: String, as: String, for: link__Purpose, import: [link__Import]) repeatable on SCHEMA

    scalar link__Import
    scalar join__FieldSet
    enum link__Purpose {
        SECURITY
        EXECUTION
    }
    enum join__Graph {
        TEST @join__graph(name: "test", url: "http://localhost:4001/graphql")
    }

    type Query {
        rootType: RootType
    }

    union RootType
    @join__type(graph: TEST)
    = MyFragment

    type MyFragment
    @join__type(graph: TEST)
    {
        edges: [MyFragmentEdge]
    }

    type MyFragmentEdge
    @join__type(graph: TEST)
    {
      node: MyType
    }

    type MyType
    @join__type(graph: TEST)
    {
        id: ID!
        subType: MySubtype
    }


    type MySubtype
    @join__type(graph: TEST)
    {
        edges: [MySubtypeEdge]
    }

    type MySubtypeEdge
    @join__type(graph: TEST)
    {
      node: MyLeafType
    }

    type MyLeafType
    @join__type(graph: TEST)
    {
        id: ID!
        myField: String!
    }
"#,
        &Default::default(),
    )
    .unwrap();

    let query = Query::parse(
        r#"query MyQueryThatContainsFragments {
                rootType {
                  ... on MyFragment {
                    edges {
                      node {
                        id
                        subType {
                          __typename
                          edges {
                            __typename
                            node {
                              __typename
                              id
                              myField
                            }
                          }
                        }
                      }
                      __typename
                    }
                    __typename
                  }
                }
              }"#,
        &schema,
        &Default::default(),
    )
    .unwrap();

    assert!(query.contains_error_path(
        None,
        &None,
        &Path::from("rootType/edges/0/node/subType/edges/0/node/myField"),
        BooleanValues { bits: 0 }
    ));
}

#[test]
fn test_query_not_named_query() {
    let config = Default::default();
    let schema = Schema::parse_test(
        r#"
        schema
            @core(feature: "https://specs.apollo.dev/core/v0.1")
            @core(feature: "https://specs.apollo.dev/join/v0.1")
            @core(feature: "https://specs.apollo.dev/inaccessible/v0.1")
            {
            query: TheOneAndOnlyQuery
        }
        directive @core(feature: String!) repeatable on SCHEMA
        directive @join__graph(name: String!, url: String!) on ENUM_VALUE
        directive @inaccessible on OBJECT | FIELD_DEFINITION | INTERFACE | UNION
        enum join__Graph {
            TEST @join__graph(name: "test", url: "http://localhost:4001/graphql")
        }

        type TheOneAndOnlyQuery { example: Boolean }
        "#,
        &config,
    )
    .unwrap();
    let query = Query::parse("{ example }", &schema, &config).unwrap();
    let selection = &query.operations[0].selection_set[0];
    assert!(
        matches!(
            selection,
            Selection::Field {
                field_type: FieldType(hir::Type::Named { name, .. }),
                ..
            }
            if name == "Boolean"
        ),
        "unexpected selection {selection:?}"
    );
}

#[test]
fn filtered_defer_fragment() {
    let config = Configuration::default();
    let schema = Schema::parse_test(
        r#"
        schema
            @core(feature: "https://specs.apollo.dev/core/v0.1")
            @core(feature: "https://specs.apollo.dev/join/v0.1")
            @core(feature: "https://specs.apollo.dev/inaccessible/v0.1")
            {
                query: Query
        }
        directive @core(feature: String!) repeatable on SCHEMA
        directive @join__graph(name: String!, url: String!) on ENUM_VALUE
        directive @inaccessible on OBJECT | FIELD_DEFINITION | INTERFACE | UNION
        enum join__Graph {
            TEST @join__graph(name: "test", url: "http://localhost:4001/graphql")
        }

        type Query {
            a: A
        }

        type A {
            b: String
            c: String!
        }
        "#,
        &config,
    )
    .unwrap();
    let query = r#"{
        a {
          b
          ... @defer(label: "A") {
            c
          }
        }
      }"#;

    let filtered_query = "{
        a {
          b
        }
      }";

    let mut compiler = ApolloCompiler::new();
    compiler.add_executable(query, "query.graphql");
    let (fragments, operations, defer_stats) =
        Query::extract_query_information(&compiler, &schema).unwrap();

    let subselections = crate::spec::query::subselections::collect_subselections(
        &config,
        &operations,
        &fragments.map,
        &defer_stats,
    )
    .unwrap();
    let mut query = Query {
        string: query.to_string(),
        fragments,
        operations,
        filtered_query: None,
        subselections,
        defer_stats,
        is_original: true,
        unauthorized_paths: vec![],
        validation_error: None,
    };

    let mut compiler = ApolloCompiler::new();
    compiler.add_executable(filtered_query, "filtered_query.graphql");
    let (fragments, operations, defer_stats) =
        Query::extract_query_information(&compiler, &schema).unwrap();

    let subselections = crate::spec::query::subselections::collect_subselections(
        &config,
        &operations,
        &fragments.map,
        &defer_stats,
    )
    .unwrap();

    let filtered = Query {
        string: filtered_query.to_string(),
        fragments,
        operations,
        filtered_query: None,
        subselections,
        defer_stats,
        is_original: false,
        unauthorized_paths: vec![],
        validation_error: None,
    };

    query.filtered_query = Some(Arc::new(filtered));

    let mut response = crate::graphql::Response::builder()
        .data(json! {{
            "a": {
                "b": "b",
              }
        }})
        .build();

    query.filtered_query.as_ref().unwrap().format_response(
        &mut response,
        None,
        Object::new(),
        &schema,
        BooleanValues { bits: 0 },
    );

    assert_json_snapshot!(response);

    query.format_response(
        &mut response,
        None,
        Object::new(),
        &schema,
        BooleanValues { bits: 0 },
    );

    assert_json_snapshot!(response);
}
