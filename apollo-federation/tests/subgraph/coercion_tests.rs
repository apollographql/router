use apollo_compiler::Name;
use apollo_compiler::Node;
use apollo_compiler::ast::Value;
use apollo_compiler::schema::ExtendedType;
use apollo_federation::subgraph::test_utils::build_and_validate;

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
fn coerces_field_argument_default_values() {
    // Test that field argument default values are coerced correctly.
    // The field argument expects String! but the default is a list ["id"]
    // which gets removed during coercion (invalid defaults are stripped).
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
    let name_field = t_type.fields.get("name").expect("name field exists");
    let arg = name_field
        .argument_by_name("arg")
        .expect("arg argument exists");

    // Invalid list default is removed during coercion
    assert_eq!(arg.default_value, None);
}

#[test]
fn coerces_input_field_default_values() {
    // Test that input object field default values are coerced correctly.
    // - `name` has an enum-like default value `Anonymous` which should be coerced to string
    // - `age` expects Int but the default is a list [18] which gets removed (invalid defaults stripped)
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

    let subgraph = build_and_validate(schema);
    let user_input = subgraph
        .validated_schema()
        .schema()
        .types
        .get("UserInput")
        .and_then(|ty| {
            if let ExtendedType::InputObject(i) = ty {
                Some(i)
            } else {
                None
            }
        })
        .expect("UserInput type not found");

    // Enum literal coerced to string
    let name_field = user_input.fields.get("name").expect("name field exists");
    assert_eq!(
        name_field.default_value,
        Some(Node::new(Value::String("Anonymous".into())))
    );

    // Invalid list default is removed during coercion
    let age_field = user_input.fields.get("age").expect("age field exists");
    assert_eq!(age_field.default_value, None);
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
