use apollo_compiler::Name;
use apollo_compiler::Node;
use apollo_compiler::ast::Value;
use apollo_compiler::schema::ExtendedType;
use apollo_federation::assert_errors;
use apollo_federation::subgraph::test_utils::BuildOption;
use apollo_federation::subgraph::test_utils::build_and_validate;
use apollo_federation::subgraph::test_utils::build_for_errors_with_option;

#[test]
fn coerces_directive_argument_values() {
    // Test that directive argument values are coerced correctly.
    let schema = r#"
        extend schema @link(url: "https://specs.apollo.dev/federation/v2.0", import: ["@key"])

        type Query {
            test: T!
        }

        type T @key(fields: "id") {
            id: ID!
            x: Int!
        }
    "#;

    let subgraph = build_and_validate(schema);
    let t_type = subgraph
        .validated_schema()
        .schema()
        .types
        .get("T")
        .and_then(|ty| match ty {
            ExtendedType::Object(t) => Some(t),
            _ => None::<&Node<apollo_compiler::schema::ObjectType>>,
        })
        .expect("T type not found");
    let key_directive = t_type
        .directives
        .iter()
        .find(|d| d.name == "key")
        .expect("@key directive exists");
    let fields_value = key_directive
        .specified_argument_by_name("fields")
        .expect("fields argument exists");

    assert_eq!(fields_value.as_ref(), &Value::String("id".into()));
}

#[test]
fn validates_field_argument_default_values() {
    // Test that field argument default values are validated correctly. The field argument expects
    // String! but the default is a list ["id"], which results in an error.
    let schema = r#"
        extend schema @link(url: "https://specs.apollo.dev/federation/v2.0", import: ["@key"])

        type Query {
            test: T!
        }

        type T @key(fields: "id") {
            id: ID!
            name(arg: String! = ["id"]): String!
            x: Int!
        }
    "#;

    let errors = build_for_errors_with_option(schema, BuildOption::AsIs);
    assert_errors!(
        errors,
        [(
            "INVALID_GRAPHQL",
            r#"[S] Invalid default value (got: ["id"]) provided for argument T.name(arg:) of type String!."#,
        )]
    );
}

#[test]
fn validates_input_field_default_values() {
    // Checks that input object field default values are validated/coerced correctly.
    // - `name` has an enum-like default value `Anonymous` which should be coerced to string, and
    //   should not generate an error.
    // - `age` expects Int but the default is a list [18] which should generate an error.
    let schema = r#"
        extend schema @link(url: "https://specs.apollo.dev/federation/v2.0", import: ["@key"])

        type Query {
            test(input: UserInput): String
        }

        input UserInput {
            name: String = Anonymous
            age: Int = [18]
        }
    "#;

    let errors = build_for_errors_with_option(schema, BuildOption::AsIs);
    assert_errors!(
        errors,
        [(
            "INVALID_GRAPHQL",
            r#"[S] Invalid default value (got: [18]) provided for input field UserInput.age of type Int."#,
        )]
    );
}

#[test]
fn coerces_enum_value_to_non_null_string_on_custom_directive() {
    let schema = r#"
        extend schema @link(url: "https://specs.apollo.dev/federation/v2.0")
        directive @myDirective(arg: String!) on FIELD_DEFINITION

        type Query {
            test: T!
        }

        interface T {
            id: ID! @myDirective(arg: MyEnum)
            x: Int!
        }
    "#;

    let subgraph = build_and_validate(schema);
    let t_interface = subgraph
        .validated_schema()
        .schema()
        .types
        .get("T")
        .and_then(|ty| {
            if let ExtendedType::Interface(i) = ty {
                Some(i)
            } else {
                None
            }
        })
        .expect("T interface not found");
    let id_field = t_interface.fields.get("id").expect("id field exists");
    let directive = id_field
        .directives
        .iter()
        .find(|d| d.name == "myDirective")
        .expect("myDirective exists");
    let arg_value = directive
        .specified_argument_by_name("arg")
        .expect("arg argument exists");

    assert_eq!(arg_value.as_ref(), &Value::String("MyEnum".into()));
}

#[test]
fn coerces_enum_literal_to_string_on_union_directive() {
    // Test that enum literal values are coerced to strings for union type directives.
    // The directive expects String! but receives an enum literal Searchable
    // which should be coerced to "Searchable".
    let schema = r#"
        extend schema @link(url: "https://specs.apollo.dev/federation/v2.0")
        directive @metadata(tag: String!) on UNION

        type Query {
            search: SearchResult
        }

        type Book {
            title: String!
        }

        type Author {
            name: String!
        }

        union SearchResult @metadata(tag: Searchable) = Book | Author
    "#;

    let subgraph = build_and_validate(schema);
    let search_result = subgraph
        .validated_schema()
        .schema()
        .types
        .get("SearchResult")
        .and_then(|ty| {
            if let ExtendedType::Union(u) = ty {
                Some(u)
            } else {
                None
            }
        })
        .expect("SearchResult union not found");
    let directive = search_result
        .directives
        .iter()
        .find(|d| d.name == "metadata")
        .expect("metadata directive exists");
    let tag_value = directive
        .specified_argument_by_name("tag")
        .expect("tag argument exists");

    assert_eq!(tag_value.as_ref(), &Value::String("Searchable".into()));
}

#[test]
fn coerces_enum_literal_to_string_on_scalar_directive() {
    // Test that enum literal values are coerced to strings for scalar type directives.
    // The directive expects String! but receives an enum literal ISO8601
    // which should be coerced to "ISO8601".
    let schema = r#"
        extend schema @link(url: "https://specs.apollo.dev/federation/v2.0")
        directive @format(type: String!) on SCALAR

        type Query {
            data: JSON
        }

        scalar JSON @format(type: ISO8601)
    "#;

    let subgraph = build_and_validate(schema);
    let json_scalar = subgraph
        .validated_schema()
        .schema()
        .types
        .get("JSON")
        .and_then(|ty| {
            if let ExtendedType::Scalar(s) = ty {
                Some(s)
            } else {
                None
            }
        })
        .expect("JSON scalar not found");
    let directive = json_scalar
        .directives
        .iter()
        .find(|d| d.name == "format")
        .expect("format directive exists");
    let type_value = directive
        .specified_argument_by_name("type")
        .expect("type argument exists");

    assert_eq!(type_value.as_ref(), &Value::String("ISO8601".into()));
}

#[test]
fn coerces_enum_literal_to_string_on_enum_type_directive() {
    // Test that enum literal values are coerced to strings for enum type directives.
    // The directive expects String! but receives an enum literal StatusType
    // which should be coerced to "StatusType".
    let schema = r#"
        extend schema @link(url: "https://specs.apollo.dev/federation/v2.0")
        directive @metadata(category: String!) on ENUM

        type Query {
            status: Status
        }

        enum Status @metadata(category: StatusType) {
            ACTIVE
            INACTIVE
        }
    "#;

    let subgraph = build_and_validate(schema);
    let status_enum = subgraph
        .validated_schema()
        .schema()
        .types
        .get("Status")
        .and_then(|ty| {
            if let ExtendedType::Enum(e) = ty {
                Some(e)
            } else {
                None
            }
        })
        .expect("Status enum not found");
    let directive = status_enum
        .directives
        .iter()
        .find(|d| d.name == "metadata")
        .expect("metadata directive exists");
    let category_value = directive
        .specified_argument_by_name("category")
        .expect("category argument exists");

    assert_eq!(category_value.as_ref(), &Value::String("StatusType".into()));
}

#[test]
fn coerces_enum_literal_to_string_on_enum_value_directive() {
    // Test that enum literal values are coerced to strings for enum value directives.
    // The directive expects String! but receives an enum literal Important
    // which should be coerced to "Important".
    let schema = r#"
        extend schema @link(url: "https://specs.apollo.dev/federation/v2.0")
        directive @alias(name: String!) on ENUM_VALUE

        type Query {
            priority: Priority
        }

        enum Priority {
            HIGH @alias(name: Important)
            MEDIUM
            LOW
        }
    "#;

    let subgraph = build_and_validate(schema);
    let priority_enum = subgraph
        .validated_schema()
        .schema()
        .types
        .get("Priority")
        .and_then(|ty| {
            if let ExtendedType::Enum(e) = ty {
                Some(e)
            } else {
                None
            }
        })
        .expect("Priority enum not found");
    let high_value = priority_enum.values.get("HIGH").expect("HIGH value exists");
    let directive = high_value
        .directives
        .iter()
        .find(|d| d.name == "alias")
        .expect("alias directive exists");
    let name_value = directive
        .specified_argument_by_name("name")
        .expect("name argument exists");

    assert_eq!(name_value.as_ref(), &Value::String("Important".into()));
}

#[test]
fn coerces_string_to_enum() {
    let schema = r#"
      extend schema @link(url: "https://specs.apollo.dev/federation/v2.0")

      type Query {
        foo(arg: Status = "ACTIVE"): String!
      }

      enum Status {
        ACTIVE
        INACTIVE
      }
    "#;

    let subgraph = build_and_validate(schema);
    let query = subgraph
        .validated_schema()
        .schema()
        .types
        .get("Query")
        .and_then(|ty| {
            if let ExtendedType::Object(obj) = ty {
                Some(obj)
            } else {
                None
            }
        })
        .expect("Query type not found");
    let foo = query.fields.get("foo").expect("foo field exists");
    let arg = foo.argument_by_name("arg").expect("arg argument exists");

    assert_eq!(
        arg.default_value,
        Some(Node::new(Value::Enum(Name::new_unchecked("ACTIVE"))))
    );
}

#[test]
fn validates_null_default_for_non_null_field_argument() {
    let schema = r#"
        extend schema @link(url: "https://specs.apollo.dev/federation/v2.0")

        type Query {
            test(arg: String! = null): String
        }
    "#;

    let errors = build_for_errors_with_option(schema, BuildOption::AsIs);
    assert_errors!(
        errors,
        [(
            "INVALID_GRAPHQL",
            r#"[S] Invalid default value (got: null) provided for argument Query.test(arg:) of type String!."#,
        )]
    );
}

#[test]
fn validates_null_default_for_non_null_input_field() {
    let schema = r#"
        extend schema @link(url: "https://specs.apollo.dev/federation/v2.0")

        type Query {
            test(input: MyInput): String
        }

        input MyInput {
            name: String! = null
        }
    "#;

    let errors = build_for_errors_with_option(schema, BuildOption::AsIs);
    assert_errors!(
        errors,
        [(
            "INVALID_GRAPHQL",
            r#"[S] Invalid default value (got: null) provided for input field MyInput.name of type String!."#,
        )]
    );
}

#[test]
fn validates_deprecated_required_field_argument() {
    let schema = r#"
        extend schema @link(url: "https://specs.apollo.dev/federation/v2.0")

        type Query {
            test(arg: String! @deprecated): String
        }
    "#;

    let errors = build_for_errors_with_option(schema, BuildOption::AsIs);
    assert_errors!(
        errors,
        [(
            "INVALID_GRAPHQL",
            r#"[S] Required argument Query.test(arg:) cannot be deprecated."#,
        )]
    );
}

#[test]
fn validates_deprecated_required_input_field() {
    let schema = r#"
        extend schema @link(url: "https://specs.apollo.dev/federation/v2.0")

        type Query {
            test(input: MyInput): String
        }

        input MyInput {
            name: String! @deprecated
        }
    "#;

    let errors = build_for_errors_with_option(schema, BuildOption::AsIs);
    assert_errors!(
        errors,
        [(
            "INVALID_GRAPHQL",
            r#"[S] Required argument MyInput.name cannot be deprecated."#,
        )]
    );
}

#[test]
fn validates_unknown_field_in_input_object_default() {
    let schema = r#"
        extend schema @link(url: "https://specs.apollo.dev/federation/v2.0")

        type Query {
            test(arg: MyInput = {unknownField: 1}): String
        }

        input MyInput {
            name: String
        }
    "#;

    let errors = build_for_errors_with_option(schema, BuildOption::AsIs);
    assert_errors!(
        errors,
        [(
            "INVALID_GRAPHQL",
            r#"[S] Invalid default value (got: {unknownField: 1}) provided for argument Query.test(arg:) of type MyInput."#,
        )]
    );
}

#[test]
fn validates_non_object_value_for_input_object_default() {
    let schema = r#"
        extend schema @link(url: "https://specs.apollo.dev/federation/v2.0")

        type Query {
            test(arg: MyInput = "not an object"): String
        }

        input MyInput {
            name: String
        }
    "#;

    let errors = build_for_errors_with_option(schema, BuildOption::AsIs);
    assert_errors!(
        errors,
        [(
            "INVALID_GRAPHQL",
            r#"[S] Invalid default value (got: "not an object") provided for argument Query.test(arg:) of type MyInput."#,
        )]
    );
}

#[test]
fn validates_unknown_enum_value_in_default() {
    let schema = r#"
        extend schema @link(url: "https://specs.apollo.dev/federation/v2.0")

        type Query {
            test(arg: Status = UNKNOWN_VALUE): String
        }

        enum Status {
            ACTIVE
            INACTIVE
        }
    "#;

    let errors = build_for_errors_with_option(schema, BuildOption::AsIs);
    assert_errors!(
        errors,
        [(
            "INVALID_GRAPHQL",
            r#"[S] Invalid default value (got: UNKNOWN_VALUE) provided for argument Query.test(arg:) of type Status."#,
        )]
    );
}

#[test]
fn validates_invalid_string_to_enum_coercion() {
    let schema = r#"
        extend schema @link(url: "https://specs.apollo.dev/federation/v2.0")

        type Query {
            test(arg: Status = "not a name!"): String
        }

        enum Status {
            ACTIVE
            INACTIVE
        }
    "#;

    let errors = build_for_errors_with_option(schema, BuildOption::AsIs);
    assert_errors!(
        errors,
        [(
            "INVALID_GRAPHQL",
            r#"[S] Invalid default value (got: "not a name!") provided for argument Query.test(arg:) of type Status."#,
        )]
    );
}

#[test]
fn validates_wrong_type_for_boolean_default() {
    let schema = r#"
        extend schema @link(url: "https://specs.apollo.dev/federation/v2.0")

        type Query {
            test(arg: Boolean = 42): String
        }
    "#;

    let errors = build_for_errors_with_option(schema, BuildOption::AsIs);
    assert_errors!(
        errors,
        [(
            "INVALID_GRAPHQL",
            r#"[S] Invalid default value (got: 42) provided for argument Query.test(arg:) of type Boolean."#,
        )]
    );
}

#[test]
fn accepts_int_default_for_id_type() {
    let schema = r#"
        extend schema @link(url: "https://specs.apollo.dev/federation/v2.0")

        type Query {
            test(arg: ID = 42): String
        }
    "#;

    let subgraph = build_and_validate(schema);
    let query = subgraph
        .validated_schema()
        .schema()
        .types
        .get("Query")
        .and_then(|ty| {
            if let ExtendedType::Object(obj) = ty {
                Some(obj)
            } else {
                None
            }
        })
        .expect("Query type not found");
    let test_field = query.fields.get("test").expect("test field exists");
    let arg = test_field
        .argument_by_name("arg")
        .expect("arg argument exists");

    assert_eq!(arg.default_value, Some(Node::new(Value::Int(42.into()))));
}

#[test]
fn validates_wrong_type_for_id_default() {
    let schema = r#"
        extend schema @link(url: "https://specs.apollo.dev/federation/v2.0")

        type Query {
            test(arg: ID = true): String
        }
    "#;

    let errors = build_for_errors_with_option(schema, BuildOption::AsIs);
    assert_errors!(
        errors,
        [(
            "INVALID_GRAPHQL",
            r#"[S] Invalid default value (got: true) provided for argument Query.test(arg:) of type ID."#,
        )]
    );
}

#[test]
fn accepts_int_default_for_float_type() {
    let schema = r#"
        extend schema @link(url: "https://specs.apollo.dev/federation/v2.0")

        type Query {
            test(arg: Float = 42): String
        }
    "#;

    let subgraph = build_and_validate(schema);
    let query = subgraph
        .validated_schema()
        .schema()
        .types
        .get("Query")
        .and_then(|ty| {
            if let ExtendedType::Object(obj) = ty {
                Some(obj)
            } else {
                None
            }
        })
        .expect("Query type not found");
    let test_field = query.fields.get("test").expect("test field exists");
    let arg = test_field
        .argument_by_name("arg")
        .expect("arg argument exists");

    assert_eq!(arg.default_value, Some(Node::new(Value::Int(42.into()))));
}

// --- JS values.test.ts parity tests ---

#[test]
fn validates_invalid_default_in_directive_argument() {
    let schema = r#"
        extend schema @link(url: "https://specs.apollo.dev/federation/v2.0")
        type Query { f: Int }
        directive @myDirective(a: Int = "foo") on FIELD
    "#;

    let errors = build_for_errors_with_option(schema, BuildOption::AsIs);
    assert_errors!(
        errors,
        [(
            "INVALID_GRAPHQL",
            r#"[S] Invalid default value (got: "foo") provided for argument @myDirective(a:) of type Int."#,
        )]
    );
}

#[test]
fn validates_invalid_nested_input_field_default() {
    let schema = r#"
        extend schema @link(url: "https://specs.apollo.dev/federation/v2.0")
        type Query { f(i: I = { x: 2, y: "3" }): Int }
        input I { x: Int  y: Int }
    "#;

    let errors = build_for_errors_with_option(schema, BuildOption::AsIs);
    assert_errors!(
        errors,
        [(
            "INVALID_GRAPHQL",
            r#"[S] Invalid default value (got: {x: 2, y: "3"}) provided for argument Query.f(i:) of type I."#,
        )]
    );
}

#[test]
fn validates_unknown_enum_value_as_string() {
    // JS shows "TWOO" (quoted) because it stores enum values as strings internally.
    // Rust coerces the string to an enum value first, so the error shows TWOO (unquoted).
    let schema = r#"
        extend schema @link(url: "https://specs.apollo.dev/federation/v2.0")
        type Query { f(e: E = "TWOO"): Int }
        enum E { ONE  TWO }
    "#;

    let errors = build_for_errors_with_option(schema, BuildOption::AsIs);
    assert_errors!(
        errors,
        [(
            "INVALID_GRAPHQL",
            r#"[S] Invalid default value (got: TWOO) provided for argument Query.f(e:) of type E."#,
        )]
    );
}

#[test]
fn accepts_custom_scalar_object_in_field_arg() {
    let schema = r#"
        extend schema @link(url: "https://specs.apollo.dev/federation/v2.0")
        type Query { f(i: Scalar = { x: 2, y: "3" }): Int }
        scalar Scalar
    "#;
    build_and_validate(schema);
}

#[test]
fn accepts_custom_scalar_object_in_directive_arg() {
    let schema = r#"
        extend schema @link(url: "https://specs.apollo.dev/federation/v2.0")
        type Query { f: Int }
        directive @myDirective(i: Scalar = { x: 2, y: "3" }) on FIELD
        scalar Scalar
    "#;
    build_and_validate(schema);
}

#[test]
fn accepts_custom_scalar_object_in_input_field() {
    let schema = r#"
        extend schema @link(url: "https://specs.apollo.dev/federation/v2.0")
        type Query { f(x: I): Int }
        input I { x: Scalar = { z: { a: 4 } } }
        scalar Scalar
    "#;
    build_and_validate(schema);
}

#[test]
fn accepts_non_list_coercible_to_list() {
    let schema = r#"
        extend schema @link(url: "https://specs.apollo.dev/federation/v2.0")
        type Query { f(x: [String] = "foo"): Int }
    "#;
    build_and_validate(schema);
}

#[test]
fn accepts_multi_level_list_coercion() {
    let schema = r#"
        extend schema @link(url: "https://specs.apollo.dev/federation/v2.0")
        type Query { f(x: [[[String]!]]! = "foo"): Int }
    "#;
    build_and_validate(schema);
}

#[test]
fn validates_invalid_multi_level_list_coercion() {
    let schema = r#"
        extend schema @link(url: "https://specs.apollo.dev/federation/v2.0")
        type Query { f(x: [[[String]!]]! = 2): Int }
    "#;

    let errors = build_for_errors_with_option(schema, BuildOption::AsIs);
    assert_errors!(
        errors,
        [(
            "INVALID_GRAPHQL",
            r#"[S] Invalid default value (got: 2) provided for argument Query.f(x:) of type [[[String]!]]!."#,
        )]
    );
}

#[test]
fn accepts_nested_input_coercion_with_list_and_id() {
    let schema = r#"
        extend schema @link(url: "https://specs.apollo.dev/federation/v2.0")
        type Query { f(x: I = { j: { x: 1, z: "Foo" } }): Int }
        input I { j: [J] }
        input J { x: ID  y: ID  z: ID }
    "#;
    build_and_validate(schema);
}

#[test]
fn accepts_cyclic_input_defaults_no_infinite_loop() {
    let schema = r#"
        extend schema @link(url: "https://specs.apollo.dev/federation/v2.0")
        input A { b: B = {} }
        input B { a: A = {} }
        type Query { q(a: A = {}): Int }
    "#;
    build_and_validate(schema);
}

#[test]
fn accepts_null_default_for_nullable_input() {
    let schema = r#"
        extend schema @link(url: "https://specs.apollo.dev/federation/v2.0")
        type Query { f(i: Int = null): Int }
    "#;
    build_and_validate(schema);
}

// --- Additional edge cases (cross-checked with JS) ---

#[test]
fn validates_float_default_for_int_type() {
    let schema = r#"
        extend schema @link(url: "https://specs.apollo.dev/federation/v2.0")
        type Query { f(x: Int = 3.14): Int }
    "#;

    let errors = build_for_errors_with_option(schema, BuildOption::AsIs);
    assert_errors!(
        errors,
        [(
            "INVALID_GRAPHQL",
            r#"[S] Invalid default value (got: 3.14) provided for argument Query.f(x:) of type Int."#,
        )]
    );
}

#[test]
fn validates_string_default_for_int_type() {
    let schema = r#"
        extend schema @link(url: "https://specs.apollo.dev/federation/v2.0")
        type Query { f(x: Int = "foo"): Int }
    "#;

    let errors = build_for_errors_with_option(schema, BuildOption::AsIs);
    assert_errors!(
        errors,
        [(
            "INVALID_GRAPHQL",
            r#"[S] Invalid default value (got: "foo") provided for argument Query.f(x:) of type Int."#,
        )]
    );
}

#[test]
fn validates_int_default_for_string_type() {
    let schema = r#"
        extend schema @link(url: "https://specs.apollo.dev/federation/v2.0")
        type Query { f(x: String = 42): Int }
    "#;

    let errors = build_for_errors_with_option(schema, BuildOption::AsIs);
    assert_errors!(
        errors,
        [(
            "INVALID_GRAPHQL",
            r#"[S] Invalid default value (got: 42) provided for argument Query.f(x:) of type String."#,
        )]
    );
}

#[test]
fn validates_boolean_default_for_string_type() {
    let schema = r#"
        extend schema @link(url: "https://specs.apollo.dev/federation/v2.0")
        type Query { f(x: String = true): Int }
    "#;

    let errors = build_for_errors_with_option(schema, BuildOption::AsIs);
    assert_errors!(
        errors,
        [(
            "INVALID_GRAPHQL",
            r#"[S] Invalid default value (got: true) provided for argument Query.f(x:) of type String."#,
        )]
    );
}

#[test]
fn accepts_enum_literal_for_string_default() {
    let schema = r#"
        extend schema @link(url: "https://specs.apollo.dev/federation/v2.0")
        type Query { f(x: String = SomeValue): Int }
    "#;
    build_and_validate(schema);
}

#[test]
fn accepts_float_for_float_default() {
    let schema = r#"
        extend schema @link(url: "https://specs.apollo.dev/federation/v2.0")
        type Query { f(x: Float = 3.14): Int }
    "#;
    build_and_validate(schema);
}

#[test]
fn validates_string_default_for_boolean_type() {
    let schema = r#"
        extend schema @link(url: "https://specs.apollo.dev/federation/v2.0")
        type Query { f(x: Boolean = "true"): Int }
    "#;

    let errors = build_for_errors_with_option(schema, BuildOption::AsIs);
    assert_errors!(
        errors,
        [(
            "INVALID_GRAPHQL",
            r#"[S] Invalid default value (got: "true") provided for argument Query.f(x:) of type Boolean."#,
        )]
    );
}

#[test]
fn accepts_null_for_nullable_list_type() {
    let schema = r#"
        extend schema @link(url: "https://specs.apollo.dev/federation/v2.0")
        type Query { f(x: [String] = null): Int }
    "#;
    build_and_validate(schema);
}

#[test]
fn validates_null_for_non_null_list_type() {
    let schema = r#"
        extend schema @link(url: "https://specs.apollo.dev/federation/v2.0")
        type Query { f(x: [String]! = null): Int }
    "#;

    let errors = build_for_errors_with_option(schema, BuildOption::AsIs);
    assert_errors!(
        errors,
        [(
            "INVALID_GRAPHQL",
            r#"[S] Invalid default value (got: null) provided for argument Query.f(x:) of type [String]!."#,
        )]
    );
}

#[test]
fn validates_list_for_non_list_type() {
    let schema = r#"
        extend schema @link(url: "https://specs.apollo.dev/federation/v2.0")
        type Query { f(x: String = ["foo"]): Int }
    "#;

    let errors = build_for_errors_with_option(schema, BuildOption::AsIs);
    assert_errors!(
        errors,
        [(
            "INVALID_GRAPHQL",
            r#"[S] Invalid default value (got: ["foo"]) provided for argument Query.f(x:) of type String."#,
        )]
    );
}

#[test]
fn accepts_empty_object_for_input_with_no_required_fields() {
    let schema = r#"
        extend schema @link(url: "https://specs.apollo.dev/federation/v2.0")
        type Query { f(x: I = {}): Int }
        input I { name: String }
    "#;
    build_and_validate(schema);
}

#[test]
fn accepts_custom_scalar_with_enum_literal() {
    let schema = r#"
        extend schema @link(url: "https://specs.apollo.dev/federation/v2.0")
        type Query { f(x: MyScalar = SOME_VALUE): Int }
        scalar MyScalar
    "#;
    build_and_validate(schema);
}

#[test]
fn accepts_custom_scalar_with_list() {
    let schema = r#"
        extend schema @link(url: "https://specs.apollo.dev/federation/v2.0")
        type Query { f(x: MyScalar = [1, 2, 3]): Int }
        scalar MyScalar
    "#;
    build_and_validate(schema);
}

#[test]
fn accepts_custom_scalar_with_boolean() {
    let schema = r#"
        extend schema @link(url: "https://specs.apollo.dev/federation/v2.0")
        type Query { f(x: MyScalar = true): Int }
        scalar MyScalar
    "#;
    build_and_validate(schema);
}

// --- List element and nested structure edge cases (cross-checked with JS) ---

#[test]
fn validates_null_element_inside_non_null_list() {
    let schema = r#"
        extend schema @link(url: "https://specs.apollo.dev/federation/v2.0")
        type Query { f(x: [String!] = ["foo", null]): Int }
    "#;

    let errors = build_for_errors_with_option(schema, BuildOption::AsIs);
    assert_errors!(
        errors,
        [(
            "INVALID_GRAPHQL",
            r#"[S] Invalid default value (got: ["foo", null]) provided for argument Query.f(x:) of type [String!]."#,
        )]
    );
}

#[test]
fn validates_wrong_type_in_list_element() {
    let schema = r#"
        extend schema @link(url: "https://specs.apollo.dev/federation/v2.0")
        type Query { f(x: [Int] = [1, "two", 3]): Int }
    "#;

    let errors = build_for_errors_with_option(schema, BuildOption::AsIs);
    assert_errors!(
        errors,
        [(
            "INVALID_GRAPHQL",
            r#"[S] Invalid default value (got: [1, "two", 3]) provided for argument Query.f(x:) of type [Int]."#,
        )]
    );
}

#[test]
fn accepts_nested_list() {
    let schema = r#"
        extend schema @link(url: "https://specs.apollo.dev/federation/v2.0")
        type Query { f(x: [[Int]] = [[1, 2], [3]]): Int }
    "#;
    build_and_validate(schema);
}

#[test]
fn validates_nested_list_with_wrong_element_type() {
    let schema = r#"
        extend schema @link(url: "https://specs.apollo.dev/federation/v2.0")
        type Query { f(x: [[Int]] = [[1, "two"]]): Int }
    "#;

    let errors = build_for_errors_with_option(schema, BuildOption::AsIs);
    assert_errors!(
        errors,
        [(
            "INVALID_GRAPHQL",
            r#"[S] Invalid default value (got: [[1, "two"]]) provided for argument Query.f(x:) of type [[Int]]."#,
        )]
    );
}

#[test]
fn validates_non_null_list_of_non_null_with_null_element() {
    let schema = r#"
        extend schema @link(url: "https://specs.apollo.dev/federation/v2.0")
        type Query { f(x: [String!]! = [null]): Int }
    "#;

    let errors = build_for_errors_with_option(schema, BuildOption::AsIs);
    assert_errors!(
        errors,
        [(
            "INVALID_GRAPHQL",
            r#"[S] Invalid default value (got: [null]) provided for argument Query.f(x:) of type [String!]!."#,
        )]
    );
}

#[test]
fn accepts_empty_list() {
    let schema = r#"
        extend schema @link(url: "https://specs.apollo.dev/federation/v2.0")
        type Query { f(x: [String] = []): Int }
    "#;
    build_and_validate(schema);
}

#[test]
fn accepts_string_for_id_default() {
    let schema = r#"
        extend schema @link(url: "https://specs.apollo.dev/federation/v2.0")
        type Query { f(x: ID = "abc"): Int }
    "#;
    build_and_validate(schema);
}

#[test]
fn validates_float_rejected_for_id_default() {
    let schema = r#"
        extend schema @link(url: "https://specs.apollo.dev/federation/v2.0")
        type Query { f(x: ID = 3.14): Int }
    "#;

    let errors = build_for_errors_with_option(schema, BuildOption::AsIs);
    assert_errors!(
        errors,
        [(
            "INVALID_GRAPHQL",
            r#"[S] Invalid default value (got: 3.14) provided for argument Query.f(x:) of type ID."#,
        )]
    );
}

#[test]
fn validates_int_rejected_for_enum_default() {
    let schema = r#"
        extend schema @link(url: "https://specs.apollo.dev/federation/v2.0")
        type Query { f(x: Status = 42): Int }
        enum Status { ACTIVE  INACTIVE }
    "#;

    let errors = build_for_errors_with_option(schema, BuildOption::AsIs);
    assert_errors!(
        errors,
        [(
            "INVALID_GRAPHQL",
            r#"[S] Invalid default value (got: 42) provided for argument Query.f(x:) of type Status."#,
        )]
    );
}

#[test]
fn validates_nested_input_object_with_invalid_inner_value() {
    let schema = r#"
        extend schema @link(url: "https://specs.apollo.dev/federation/v2.0")
        type Query { f(x: Outer = { inner: { value: "not_int" } }): Int }
        input Outer { inner: Inner }
        input Inner { value: Int }
    "#;

    let errors = build_for_errors_with_option(schema, BuildOption::AsIs);
    assert_errors!(
        errors,
        [(
            "INVALID_GRAPHQL",
            r#"[S] Invalid default value (got: {inner: {value: "not_int"}}) provided for argument Query.f(x:) of type Outer."#,
        )]
    );
}

#[test]
fn collects_multiple_validation_errors() {
    let schema = r#"
        extend schema @link(url: "https://specs.apollo.dev/federation/v2.0")
        type Query { f(x: Int = "bad", y: Boolean = 42): Int }
    "#;

    let errors = build_for_errors_with_option(schema, BuildOption::AsIs);
    assert_errors!(
        errors,
        [
            (
                "INVALID_GRAPHQL",
                r#"[S] Invalid default value (got: "bad") provided for argument Query.f(x:) of type Int."#,
            ),
            (
                "INVALID_GRAPHQL",
                r#"[S] Invalid default value (got: 42) provided for argument Query.f(y:) of type Boolean."#,
            ),
        ]
    );
}
