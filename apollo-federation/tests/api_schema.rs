use apollo_compiler::coord;
use apollo_compiler::schema::ExtendedType;
use apollo_compiler::validation::Valid;
use apollo_compiler::Schema;
use apollo_federation::error::FederationError;
use apollo_federation::ApiSchemaOptions;
use apollo_federation::Supergraph;

// TODO(@goto-bus-stop): inaccessible is in theory a standalone spec,
// but is only tested here as part of API schema, unlike in the JS implementation.
// This means that all test inputs must be valid supergraphs.
// Ideally we would pull out the inaccessible tests to only apply
// `InaccessibleSpecDefinition::remove_inaccessible_elements` to a `FederationSchema`,
// and remove the supergraph-specific `@link`s (`join`) below.
const INACCESSIBLE_V02_HEADER: &str = r#"
    directive @link(url: String!, as: String, import: [link__Import], for: link__Purpose) repeatable on SCHEMA

    scalar link__Import

    enum link__Purpose {
      EXECUTION
      SECURITY
    }

    directive @inaccessible on FIELD_DEFINITION | OBJECT | INTERFACE | UNION | ARGUMENT_DEFINITION | SCALAR | ENUM | ENUM_VALUE | INPUT_OBJECT | INPUT_FIELD_DEFINITION

    schema
      @link(url: "https://specs.apollo.dev/link/v1.0")
      @link(url: "https://specs.apollo.dev/join/v0.2", for: EXECUTION)
      @link(url: "https://specs.apollo.dev/inaccessible/v0.2")
    {
      query: Query
    }
"#;

fn inaccessible_to_api_schema(input: &str) -> Result<Valid<Schema>, FederationError> {
    let sdl = format!("{INACCESSIBLE_V02_HEADER}{input}");
    let graph = Supergraph::new(&sdl)?;
    Ok(graph.to_api_schema(Default::default())?.schema().clone())
}

#[test]
fn inaccessible_types_with_accessible_references() {
    let errors = inaccessible_to_api_schema(
        r#"
      # Query types can't be inaccessible
      type Query @inaccessible {
        someField: String
      }

      # Inaccessible object type
      type Object @inaccessible {
        someField: String
      }

      # Inaccessible object type can't be referenced by object field in the API
      # schema
      type Referencer1 implements Referencer2 {
        someField: Object!
      }

      # Inaccessible object type can't be referenced by interface field in the
      # API schema
      interface Referencer2 {
        someField: Object
      }

      # Inaccessible object type can't be referenced by union member with a
      # non-inaccessible parent and no non-inaccessible siblings
      union Referencer3 = Object
    "#,
    )
    .expect_err("should return validation errors");

    insta::assert_snapshot!(errors, @r###"
    The following errors occurred:
      - Type `Query` is @inaccessible but is the query root type, which must be in the API schema.
      - Type `Object` is @inaccessible but is referenced by `Referencer1.someField`, which is in the API schema.
      - Type `Object` is @inaccessible but is referenced by `Referencer2.someField`, which is in the API schema.
      - Type `Referencer3` is in the API schema but all of its members are @inaccessible.
    "###);
}

#[test]
fn removes_inaccessible_object_types() {
    let api_schema = inaccessible_to_api_schema(
        r#"
      extend schema {
        mutation: Mutation
        subscription: Subscription
      }

      # Non-inaccessible object type
      type Query {
        someField: String
      }

      # Inaccessible mutation types should be removed
      type Mutation @inaccessible {
        someObject: Object
      }

      # Inaccessible subscription types should be removed
      type Subscription @inaccessible {
        someField: String
      }

      # Inaccessible object type
      type Object @inaccessible {
        someField: String
      }

      # Inaccessible object type referenced by inaccessible object field
      type Referencer1 implements Referencer3 {
        someField: String
        privatefield: Object! @inaccessible
      }

      # Inaccessible object type referenced by non-inaccessible object field
      # with inaccessible parent
      type Referencer2 implements Referencer4 @inaccessible {
        privateField: [Object!]!
      }

      # Inaccessible object type referenced by inaccessible interface field
      interface Referencer3 {
        someField: String
        privatefield: Object @inaccessible
      }

      # Inaccessible object type referenced by non-inaccessible interface field
      # with inaccessible parent
      interface Referencer4 @inaccessible {
        privateField: [Object]
      }

      # Inaccessible object type referenced by union member with
      # non-inaccessible siblings and parent
      union Referencer5 = Query | Object

      # Inaccessible object type referenced by union member with no siblings
      # but with inaccessible parent
      union Referencer6 @inaccessible = Object
    "#,
    )
    .expect("should succeed");

    assert!(api_schema.types.contains_key("Query"));
    assert!(!api_schema.types.contains_key("Mutation"));
    assert!(!api_schema.types.contains_key("Subscription"));
    assert!(!api_schema.types.contains_key("Object"));
    assert!(coord!(Referencer1.someField).lookup(&api_schema).is_ok());
    assert!(coord!(Referencer1.privatefield)
        .lookup(&api_schema)
        .is_err());
    assert!(!api_schema.types.contains_key("Referencer2"));
    assert!(coord!(Referencer3.someField).lookup(&api_schema).is_ok());
    assert!(coord!(Referencer3.privatefield)
        .lookup(&api_schema)
        .is_err());
    assert!(!api_schema.types.contains_key("Referencer4"));

    let ExtendedType::Union(union_) = api_schema.types.get("Referencer5").unwrap() else {
        panic!("expected union");
    };
    assert!(union_.members.contains("Query"));
    assert!(!union_.members.contains("Object"));
    assert!(!api_schema.types.contains_key("Referencer6"));
}

#[test]
fn inaccessible_interface_with_accessible_references() {
    let errors = inaccessible_to_api_schema(
        r#"
      type Query {
        someField: String
      }

      # Inaccessible interface type
      interface Interface @inaccessible {
        someField: String
      }

      # Inaccessible interface type can't be referenced by object field in the
      # API schema
      type Referencer1 implements Referencer2 {
        someField: [Interface!]!
      }

      # Inaccessible interface type can't be referenced by interface field in
      # the API schema
      interface Referencer2 {
        someField: [Interface]
      }
    "#,
    )
    .expect_err("should return validation errors");

    insta::assert_snapshot!(errors, @r###"
    The following errors occurred:
      - Type `Interface` is @inaccessible but is referenced by `Referencer1.someField`, which is in the API schema.
      - Type `Interface` is @inaccessible but is referenced by `Referencer2.someField`, which is in the API schema.
    "###);
}

#[test]
fn removes_inaccessible_interface_types() {
    let api_schema = inaccessible_to_api_schema(
        r#"
      type Query {
        someField: String
      }

      # Non-inaccessible interface type
      interface VisibleInterface {
        someField: String
      }

      # Inaccessible interface type
      interface Interface @inaccessible {
        someField: String
      }

      # Inaccessible interface type referenced by inaccessible object field
      type Referencer1 implements Referencer3 {
        someField: String
        privatefield: Interface! @inaccessible
      }

      # Inaccessible interface type referenced by non-inaccessible object field
      # with inaccessible parent
      type Referencer2 implements Referencer4 @inaccessible {
        privateField: [Interface!]!
      }

      # Inaccessible interface type referenced by inaccessible interface field
      interface Referencer3 {
        someField: String
        privatefield: Interface @inaccessible
      }

      # Inaccessible interface type referenced by non-inaccessible interface
      # field with inaccessible parent
      interface Referencer4 @inaccessible {
        privateField: [Interface]
      }

      # Inaccessible interface type referenced by object type implements
      type Referencer5 implements VisibleInterface & Interface {
        someField: String
      }

      # Inaccessible interface type referenced by interface type implements
      interface Referencer6 implements VisibleInterface & Interface {
        someField: String
      }
    "#,
    )
    .expect("should succeed");

    assert!(api_schema.types.contains_key("VisibleInterface"));
    assert!(!api_schema.types.contains_key("Interface"));
    assert!(!api_schema.types.contains_key("Object"));
    assert!(api_schema.type_field("Referencer1", "someField").is_ok());
    assert!(api_schema
        .type_field("Referencer1", "privatefield")
        .is_err());
    assert!(!api_schema.types.contains_key("Referencer2"));
    assert!(api_schema.type_field("Referencer3", "someField").is_ok());
    assert!(api_schema
        .type_field("Referencer3", "privatefield")
        .is_err());
    assert!(!api_schema.types.contains_key("Referencer4"));

    let ExtendedType::Object(object) = api_schema.types.get("Referencer5").unwrap() else {
        panic!("expected object");
    };
    assert!(object.implements_interfaces.contains("VisibleInterface"));
    assert!(!object.implements_interfaces.contains("Interface"));

    let ExtendedType::Interface(interface) = api_schema.types.get("Referencer6").unwrap() else {
        panic!("expected interface");
    };
    assert!(interface.implements_interfaces.contains("VisibleInterface"));
    assert!(!interface.implements_interfaces.contains("Interface"));
}

#[test]
fn inaccessible_union_with_accessible_references() {
    let errors = inaccessible_to_api_schema(
        r#"
      type Query {
        someField: String
      }

      # Inaccessible union type
      union Union @inaccessible = Query

      # Inaccessible union type can't be referenced by object field in the API
      # schema
      type Referencer1 implements Referencer2 {
        someField: Union!
      }

      # Inaccessible union type can't be referenced by interface field in the
      # API schema
      interface Referencer2 {
        someField: Union
      }
    "#,
    )
    .expect_err("should return validation errors");

    insta::assert_snapshot!(errors, @r###"
    The following errors occurred:
      - Type `Union` is @inaccessible but is referenced by `Referencer1.someField`, which is in the API schema.
      - Type `Union` is @inaccessible but is referenced by `Referencer2.someField`, which is in the API schema.
    "###);
}

#[test]
fn removes_inaccessible_union_types() {
    let api_schema = inaccessible_to_api_schema(
        r#"
      type Query {
        someField: String
      }

      # Non-inaccessible union type
      union VisibleUnion = Query

      # Inaccessible union type
      union Union @inaccessible = Query

      # Inaccessible union type referenced by inaccessible object field
      type Referencer1 implements Referencer3 {
        someField: String
        privatefield: Union! @inaccessible
      }

      # Inaccessible union type referenced by non-inaccessible object field with
      # inaccessible parent
      type Referencer2 implements Referencer4 @inaccessible {
        privateField: [Union!]!
      }

      # Inaccessible union type referenced by inaccessible interface field
      interface Referencer3 {
        someField: String
        privatefield: Union @inaccessible
      }

      # Inaccessible union type referenced by non-inaccessible interface field
      # with inaccessible parent
      interface Referencer4 @inaccessible {
        privateField: [Union]
      }
    "#,
    )
    .expect("should succeed");

    assert!(api_schema.types.contains_key("VisibleUnion"));
    assert!(!api_schema.types.contains_key("Union"));
    assert!(!api_schema.types.contains_key("Object"));
    assert!(api_schema.type_field("Referencer1", "someField").is_ok());
    assert!(api_schema
        .type_field("Referencer1", "privatefield")
        .is_err());
    assert!(!api_schema.types.contains_key("Referencer2"));
    assert!(api_schema.type_field("Referencer3", "someField").is_ok());
    assert!(api_schema
        .type_field("Referencer3", "privatefield")
        .is_err());
    assert!(!api_schema.types.contains_key("Referencer4"));
}

#[test]
fn inaccessible_input_object_with_accessible_references() {
    let errors = inaccessible_to_api_schema(
        r#"
      type Query {
        someField: String
      }

      # Inaccessible input object type
      input InputObject @inaccessible {
        someField: String
      }

      # Inaccessible input object type can't be referenced by object field
      # argument in the API schema
      type Referencer1 implements Referencer2 {
        someField(someArg: InputObject): String
      }

      # Inaccessible input object type can't be referenced by interface field
      # argument in the API schema
      interface Referencer2 {
        someField(someArg: InputObject): String
      }

      # Inaccessible input object type can't be referenced by input object field
      # in the API schema
      input Referencer3 {
        someField: InputObject
      }

      # Inaccessible input object type can't be referenced by directive argument
      # in the API schema
      directive @referencer4(someArg: InputObject) on QUERY
    "#,
    )
    .expect_err("should return validation errors");

    insta::assert_snapshot!(errors, @r###"
    The following errors occurred:
      - Type `InputObject` is @inaccessible but is referenced by `Referencer3.someField`, which is in the API schema.
      - Type `InputObject` is @inaccessible but is referenced by `Referencer1.someField(someArg:)`, which is in the API schema.
      - Type `InputObject` is @inaccessible but is referenced by `Referencer2.someField(someArg:)`, which is in the API schema.
      - Type `InputObject` is @inaccessible but is referenced by `@referencer4(someArg:)`, which is in the API schema.
    "###);
}

#[test]
fn removes_inaccessible_input_object_types() {
    let api_schema = inaccessible_to_api_schema(
        r#"
      type Query {
        someField: String
      }

      # Non-inaccessible input object type
      input VisibleInputObject {
        someField: String
      }

      # Inaccessible input object type
      input InputObject @inaccessible {
        someField: String
      }

      # Inaccessible input object type referenced by inaccessible object field
      # argument
      type Referencer1 implements Referencer4 {
        someField(privateArg: InputObject @inaccessible): String
      }

      # Inaccessible input object type referenced by non-inaccessible object
      # field argument with inaccessible parent
      type Referencer2 implements Referencer5 {
        someField: String
        privateField(privateArg: InputObject!): String @inaccessible
      }

      # Inaccessible input object type referenced by non-inaccessible object
      # field argument with inaccessible grandparent
      type Referencer3 implements Referencer6 @inaccessible {
        privateField(privateArg: InputObject!): String
      }

      # Inaccessible input object type referenced by inaccessible interface
      # field argument
      interface Referencer4 {
        someField(privateArg: InputObject @inaccessible): String
      }

      # Inaccessible input object type referenced by non-inaccessible interface
      # field argument with inaccessible parent
      interface Referencer5 {
        someField: String
        privateField(privateArg: InputObject!): String @inaccessible
      }

      # Inaccessible input object type referenced by non-inaccessible interface
      # field argument with inaccessible grandparent
      interface Referencer6 @inaccessible {
        privateField(privateArg: InputObject!): String
      }

      # Inaccessible input object type referenced by inaccessible input object
      # field
      input Referencer7 {
        someField: String
        privateField: InputObject @inaccessible
      }

      # Inaccessible input object type referenced by non-inaccessible input
      # object field with inaccessible parent
      input Referencer8 @inaccessible {
        privateField: InputObject!
      }

      # Inaccessible input object type referenced by inaccessible directive
      # argument
      directive @referencer9(privateArg: InputObject @inaccessible) on FIELD
    "#,
    )
    .expect("should succeed");

    assert!(api_schema.types.contains_key("VisibleInputObject"));
    assert!(!api_schema.types.contains_key("InputObject"));
    assert!(coord!(Referencer1.someField).lookup(&api_schema).is_ok());
    assert!(coord!(Referencer1.someField(privateArg:))
        .lookup(&api_schema)
        .is_err());
    assert!(coord!(Referencer2.someField).lookup(&api_schema).is_ok());
    assert!(coord!(Referencer2.privateField)
        .lookup(&api_schema)
        .is_err());
    assert!(!api_schema.types.contains_key("Referencer3"));
    assert!(coord!(Referencer4.someField).lookup(&api_schema).is_ok());
    assert!(coord!(Referencer4.someField(privateArg:))
        .lookup(&api_schema)
        .is_err());
    assert!(coord!(Referencer4.someField).lookup(&api_schema).is_ok());
    assert!(coord!(Referencer4.privatefield)
        .lookup(&api_schema)
        .is_err());
    assert!(coord!(Referencer5.someField).lookup(&api_schema).is_ok());
    assert!(coord!(Referencer5.privateField)
        .lookup(&api_schema)
        .is_err());
    assert!(!api_schema.types.contains_key("Referencer6"));
    let ExtendedType::InputObject(input_object) = &api_schema.types["Referencer7"] else {
        panic!("expected input object");
    };
    assert!(input_object.fields.contains_key("someField"));
    assert!(!input_object.fields.contains_key("privatefield"));
    assert!(!api_schema.types.contains_key("Referencer8"));
    assert!(coord!(@referencer9).lookup(&api_schema).is_ok());
    assert!(coord!(@referencer9(privateArg:))
        .lookup(&api_schema)
        .is_err());
}

#[test]
fn inaccessible_enum_with_accessible_references() {
    let errors = inaccessible_to_api_schema(
        r#"
      type Query {
        someField: String
      }

      # Inaccessible enum type
      enum Enum @inaccessible {
        SOME_VALUE
      }

      # Inaccessible enum type can't be referenced by object field in the API
      # schema
      type Referencer1 implements Referencer2 {
        somefield: [Enum!]!
      }

      # Inaccessible enum type can't be referenced by interface field in the API
      # schema
      interface Referencer2 {
        somefield: [Enum]
      }

      # Inaccessible enum type can't be referenced by object field argument in
      # the API schema
      type Referencer3 implements Referencer4 {
        someField(someArg: Enum): String
      }

      # Inaccessible enum type can't be referenced by interface field argument
      # in the API schema
      interface Referencer4 {
        someField(someArg: Enum): String
      }

      # Inaccessible enum type can't be referenced by input object field in the
      # API schema
      input Referencer5 {
        someField: Enum
      }

      # Inaccessible enum type can't be referenced by directive argument in the
      # API schema
      directive @referencer6(someArg: Enum) on FRAGMENT_SPREAD
    "#,
    )
    .expect_err("should return validation errors");

    insta::assert_snapshot!(errors, @r###"
    The following errors occurred:
      - Type `Enum` is @inaccessible but is referenced by `Referencer1.somefield`, which is in the API schema.
      - Type `Enum` is @inaccessible but is referenced by `Referencer2.somefield`, which is in the API schema.
      - Type `Enum` is @inaccessible but is referenced by `Referencer5.someField`, which is in the API schema.
      - Type `Enum` is @inaccessible but is referenced by `Referencer3.someField(someArg:)`, which is in the API schema.
      - Type `Enum` is @inaccessible but is referenced by `Referencer4.someField(someArg:)`, which is in the API schema.
      - Type `Enum` is @inaccessible but is referenced by `@referencer6(someArg:)`, which is in the API schema.
    "###);
}

#[test]
fn removes_inaccessible_enum_types() {
    let api_schema = inaccessible_to_api_schema(
        r#"
      type Query {
        someField: String
      }

      # Non-inaccessible enum type
      enum VisibleEnum {
        SOME_VALUE
      }

      # Inaccessible enum type
      enum Enum @inaccessible {
        SOME_VALUE
      }

      # Inaccessible enum type referenced by inaccessible object field
      type Referencer1 implements Referencer3 {
        someField: String
        privatefield: Enum! @inaccessible
      }

      # Inaccessible enum type referenced by non-inaccessible object field with
      # inaccessible parent
      type Referencer2 implements Referencer4 @inaccessible {
        privateField: [Enum!]!
      }

      # Inaccessible enum type referenced by inaccessible interface field
      interface Referencer3 {
        someField: String
        privatefield: Enum @inaccessible
      }

      # Inaccessible enum type referenced by non-inaccessible interface field
      # with inaccessible parent
      interface Referencer4 @inaccessible {
        privateField: [Enum]
      }

      # Inaccessible enum type referenced by inaccessible object field argument
      type Referencer5 implements Referencer8 {
        someField(privateArg: Enum @inaccessible): String
      }

      # Inaccessible enum type referenced by non-inaccessible object field
      # argument with inaccessible parent
      type Referencer6 implements Referencer9 {
        someField: String
        privateField(privateArg: Enum!): String @inaccessible
      }

      # Inaccessible enum type referenced by non-inaccessible object field
      # argument with inaccessible grandparent
      type Referencer7 implements Referencer10 @inaccessible {
        privateField(privateArg: Enum!): String
      }

      # Inaccessible enum type referenced by inaccessible interface field
      # argument
      interface Referencer8 {
        someField(privateArg: Enum @inaccessible): String
      }

      # Inaccessible enum type referenced by non-inaccessible interface field
      # argument with inaccessible parent
      interface Referencer9 {
        someField: String
        privateField(privateArg: Enum!): String @inaccessible
      }

      # Inaccessible enum type referenced by non-inaccessible interface field
      # argument with inaccessible grandparent
      interface Referencer10 @inaccessible {
        privateField(privateArg: Enum!): String
      }

      # Inaccessible enum type referenced by inaccessible input object field
      input Referencer11 {
        someField: String
        privateField: Enum @inaccessible
      }

      # Inaccessible enum type referenced by non-inaccessible input object field
      # with inaccessible parent
      input Referencer12 @inaccessible {
        privateField: Enum!
      }

      # Inaccessible enum type referenced by inaccessible directive argument
      directive @referencer13(privateArg: Enum @inaccessible) on FRAGMENT_DEFINITION
    "#,
    )
    .expect("should succeed");

    assert!(api_schema.types.contains_key("VisibleEnum"));
    assert!(!api_schema.types.contains_key("Enum"));
    assert!(coord!(Referencer1.someField).lookup(&api_schema).is_ok());
    assert!(coord!(Referencer1.privatefield)
        .lookup(&api_schema)
        .is_err());
    assert!(coord!(Referencer1.someField(privateArg:))
        .lookup(&api_schema)
        .is_err());
    assert!(!api_schema.types.contains_key("Referencer2"));
    assert!(coord!(Referencer3.someField).lookup(&api_schema).is_ok());
    assert!(coord!(Referencer3.privatefield)
        .lookup(&api_schema)
        .is_err());
    assert!(!api_schema.types.contains_key("Referencer4"));
    assert!(coord!(Referencer5.someField).lookup(&api_schema).is_ok());
    assert!(coord!(Referencer5.someField(privateArg:))
        .lookup(&api_schema)
        .is_err());
    assert!(coord!(Referencer6.someField).lookup(&api_schema).is_ok());
    assert!(coord!(Referencer6.privateField)
        .lookup(&api_schema)
        .is_err());
    assert!(!api_schema.types.contains_key("Referencer7"));
    assert!(coord!(Referencer8.someField).lookup(&api_schema).is_ok());
    assert!(coord!(Referencer8.someField(privateArg:))
        .lookup(&api_schema)
        .is_err());
    assert!(coord!(Referencer9.privateField)
        .lookup(&api_schema)
        .is_err());
    assert!(!api_schema.types.contains_key("Referencer10"));
    let ExtendedType::InputObject(input_object) = &api_schema.types["Referencer11"] else {
        panic!("expected input object");
    };
    assert!(input_object.fields.contains_key("someField"));
    assert!(!input_object.fields.contains_key("privatefield"));
    assert!(!api_schema.types.contains_key("Referencer12"));
    assert!(api_schema
        .directive_definitions
        .contains_key("referencer13"));
    assert!(coord!(@referencer13(privateArg:))
        .lookup(&api_schema)
        .is_err());
}

#[test]
fn inaccessible_scalar_with_accessible_references() {
    let errors = inaccessible_to_api_schema(
        r#"
      type Query {
        someField: String
      }

      # Inaccessible scalar type
      scalar Scalar @inaccessible

      # Inaccessible scalar type can't be referenced by object field in the API
      # schema
      type Referencer1 implements Referencer2 {
        somefield: [[Scalar!]!]!
      }

      # Inaccessible scalar type can't be referenced by interface field in the
      # API schema
      interface Referencer2 {
        somefield: [[Scalar]]
      }

      # Inaccessible scalar type can't be referenced by object field argument in
      # the API schema
      type Referencer3 implements Referencer4 {
        someField(someArg: Scalar): String
      }

      # Inaccessible scalar type can't be referenced by interface field argument
      # in the API schema
      interface Referencer4 {
        someField(someArg: Scalar): String
      }

      # Inaccessible scalar type can't be referenced by input object field in
      # the API schema
      input Referencer5 {
        someField: Scalar
      }

      # Inaccessible scalar type can't be referenced by directive argument in
      # the API schema
      directive @referencer6(someArg: Scalar) on MUTATION
    "#,
    )
    .expect_err("should return validation errors");

    insta::assert_snapshot!(errors, @r###"
    The following errors occurred:
      - Type `Scalar` is @inaccessible but is referenced by `Referencer1.somefield`, which is in the API schema.
      - Type `Scalar` is @inaccessible but is referenced by `Referencer2.somefield`, which is in the API schema.
      - Type `Scalar` is @inaccessible but is referenced by `Referencer5.someField`, which is in the API schema.
      - Type `Scalar` is @inaccessible but is referenced by `Referencer3.someField(someArg:)`, which is in the API schema.
      - Type `Scalar` is @inaccessible but is referenced by `Referencer4.someField(someArg:)`, which is in the API schema.
      - Type `Scalar` is @inaccessible but is referenced by `@referencer6(someArg:)`, which is in the API schema.
    "###);
}

#[test]
fn removes_inaccessible_scalar_types() {
    let api_schema = inaccessible_to_api_schema(
        r#"
      type Query {
        someField: String
      }

      # Non-inaccessible scalar type
      scalar VisibleScalar

      # Inaccessible scalar type
      scalar Scalar @inaccessible

      # Inaccessible scalar type referenced by inaccessible object field
      type Referencer1 implements Referencer3 {
        someField: String
        privatefield: Scalar! @inaccessible
      }

      # Inaccessible scalar type referenced by non-inaccessible object field
      # with inaccessible parent
      type Referencer2 implements Referencer4 @inaccessible {
        privateField: [Scalar!]!
      }

      # Inaccessible scalar type referenced by inaccessible interface field
      interface Referencer3 {
        someField: String
        privatefield: Scalar @inaccessible
      }

      # Inaccessible scalar type referenced by non-inaccessible interface field
      # with inaccessible parent
      interface Referencer4 @inaccessible {
        privateField: [Scalar]
      }

      # Inaccessible scalar type referenced by inaccessible object field
      # argument
      type Referencer5 implements Referencer8 {
        someField(privateArg: Scalar @inaccessible): String
      }

      # Inaccessible scalar type referenced by non-inaccessible object field
      # argument with inaccessible parent
      type Referencer6 implements Referencer9 {
        someField: String
        privateField(privateArg: Scalar!): String @inaccessible
      }

      # Inaccessible scalar type referenced by non-inaccessible object field
      # argument with inaccessible grandparent
      type Referencer7 implements Referencer10 @inaccessible {
        privateField(privateArg: Scalar!): String
      }

      # Inaccessible scalar type referenced by inaccessible interface field
      # argument
      interface Referencer8 {
        someField(privateArg: Scalar @inaccessible): String
      }

      # Inaccessible scalar type referenced by non-inaccessible interface field
      # argument with inaccessible parent
      interface Referencer9 {
        someField: String
        privateField(privateArg: Scalar!): String @inaccessible
      }

      # Inaccessible scalar type referenced by non-inaccessible interface field
      # argument with inaccessible grandparent
      interface Referencer10 @inaccessible {
        privateField(privateArg: Scalar!): String
      }

      # Inaccessible scalar type referenced by inaccessible input object field
      input Referencer11 {
        someField: String
        privateField: Scalar @inaccessible
      }

      # Inaccessible scalar type referenced by non-inaccessible input object
      # field with inaccessible parent
      input Referencer12 @inaccessible {
        privateField: Scalar!
      }

      # Inaccessible scalar type referenced by inaccessible directive argument
      directive @referencer13(privateArg: Scalar @inaccessible) on INLINE_FRAGMENT
    "#,
    )
    .expect("should succeed");

    assert!(coord!(VisibleScalar).lookup(&api_schema).is_ok());
    assert!(coord!(Scalar).lookup(&api_schema).is_err());
    assert!(coord!(Referencer1.someField).lookup(&api_schema).is_ok());
    assert!(coord!(Referencer1.privatefield)
        .lookup(&api_schema)
        .is_err());
    assert!(coord!(Referencer2).lookup(&api_schema).is_err());
    assert!(coord!(Referencer3.someField).lookup(&api_schema).is_ok());
    assert!(coord!(Referencer3.privatefield)
        .lookup(&api_schema)
        .is_err());
    assert!(coord!(Referencer4).lookup(&api_schema).is_err());
    assert!(coord!(Referencer5.someField).lookup(&api_schema).is_ok());
    assert!(coord!(Referencer5.someField(privateArg:))
        .lookup(&api_schema)
        .is_err());
    assert!(coord!(Referencer6.someField).lookup(&api_schema).is_ok());
    assert!(coord!(Referencer6.privateField)
        .lookup(&api_schema)
        .is_err());
    assert!(coord!(Referencer7).lookup(&api_schema).is_err());
    assert!(coord!(Referencer8.someField).lookup(&api_schema).is_ok());
    assert!(coord!(Referencer8.someField(privateArg:))
        .lookup(&api_schema)
        .is_err());
    assert!(coord!(Referencer9.someField).lookup(&api_schema).is_ok());
    assert!(coord!(Referencer9.privateField)
        .lookup(&api_schema)
        .is_err());
    assert!(coord!(Referencer10).lookup(&api_schema).is_err());
    assert!(coord!(Referencer11).lookup(&api_schema).is_ok());
    assert!(coord!(Referencer11.someField).lookup(&api_schema).is_ok());
    assert!(coord!(Referencer11.privatefield)
        .lookup(&api_schema)
        .is_err());
    assert!(coord!(Referencer12).lookup(&api_schema).is_err());
    assert!(coord!(@referencer13).lookup(&api_schema).is_ok());
    assert!(coord!(@referencer13(privateArg:))
        .lookup(&api_schema)
        .is_err());
}

#[test]
fn inaccessible_object_field_with_accessible_references() {
    let errors = inaccessible_to_api_schema(
        r#"
      extend schema {
        mutation: Mutation
        subscription: Subscription
      }

      # Inaccessible object field can't have a non-inaccessible parent query
      # type and no non-inaccessible siblings
      type Query {
        privateField: String @inaccessible
        otherPrivateField: Float @inaccessible
      }

      # Inaccessible object field can't have a non-inaccessible parent mutation
      # type and no non-inaccessible siblings
      type Mutation {
        privateField: String @inaccessible
        otherPrivateField: Float @inaccessible
      }

      # Inaccessible object field can't have a non-inaccessible parent
      # subscription type and no non-inaccessible siblings
      type Subscription {
        privateField: String @inaccessible
        otherPrivateField: Float @inaccessible
      }

      # Inaccessible object field
      type Object implements Referencer1 {
        someField: String
        privateField: String @inaccessible
      }

      # Inaccessible object field can't be referenced by interface field in the
      # API schema
      interface Referencer1 {
        privateField: String
      }

      # Inaccessible object field can't have a non-inaccessible parent object
      # type and no non-inaccessible siblings
      type Referencer2 {
        privateField: String @inaccessible
        otherPrivateField: Float @inaccessible
      }
    "#,
    )
    .expect_err("should return validation errors");

    insta::assert_snapshot!(errors, @r###"
    The following errors occurred:
      - Type `Query` is in the API schema but all of its members are @inaccessible.
      - Type `Mutation` is in the API schema but all of its members are @inaccessible.
      - Type `Subscription` is in the API schema but all of its members are @inaccessible.
      - Field `Object.privateField` is @inaccessible but implements the interface field `Referencer1.privateField`, which is in the API schema.
      - Type `Referencer2` is in the API schema but all of its members are @inaccessible.
    "###);
}

#[test]
fn removes_inaccessible_object_fields() {
    let api_schema = inaccessible_to_api_schema(
        r#"
      extend schema {
        mutation: Mutation
        subscription: Subscription
      }

      # Inaccessible object field on query type
      type Query {
        someField: String
        privateField: String @inaccessible
      }

      # Inaccessible object field on mutation type
      type Mutation {
        someField: String
        privateField: String @inaccessible
      }

      # Inaccessible object field on subscription type
      type Subscription {
        someField: String
        privateField: String @inaccessible
      }

      # Inaccessible (and non-inaccessible) object field
      type Object implements Referencer1 & Referencer2 {
        someField: String
        privateField: String @inaccessible
      }

      # Inaccessible object field referenced by inaccessible interface field
      interface Referencer1 {
        someField: String
        privateField: String @inaccessible
      }

      # Inaccessible object field referenced by non-inaccessible interface field
      # with inaccessible parent
      interface Referencer2 @inaccessible {
        privateField: String
      }

      # Inaccessible object field with an inaccessible parent and no
      # non-inaccessible siblings
      type Referencer3 @inaccessible {
        privateField: String @inaccessible
        otherPrivateField: Float @inaccessible
      }
    "#,
    )
    .expect("should succeed");

    assert!(coord!(Query.someField).lookup(&api_schema).is_ok());
    assert!(coord!(Query.privateField).lookup(&api_schema).is_err());
    assert!(coord!(Mutation.someField).lookup(&api_schema).is_ok());
    assert!(coord!(Mutation.privateField).lookup(&api_schema).is_err());
    assert!(coord!(Subscription.someField).lookup(&api_schema).is_ok());
    assert!(coord!(Subscription.privateField)
        .lookup(&api_schema)
        .is_err());
    let ExtendedType::Object(object_type) = &api_schema.types["Object"] else {
        panic!("should be object");
    };
    assert!(object_type.implements_interfaces.contains("Referencer1"));
    assert!(!object_type.implements_interfaces.contains("Referencer2"));
    assert!(coord!(Object.someField).lookup(&api_schema).is_ok());
    assert!(coord!(Object.privateField).lookup(&api_schema).is_err());
    assert!(coord!(Referencer1.someField).lookup(&api_schema).is_ok());
    assert!(coord!(Referencer1.privatefield)
        .lookup(&api_schema)
        .is_err());
    assert!(api_schema.types.get("Referencer2").is_none());
    assert!(api_schema.types.get("Referencer3").is_none());
}

#[test]
fn inaccessible_interface_field_with_accessible_references() {
    let errors = inaccessible_to_api_schema(
        r#"
      type Query {
        someField: String
      }

      # Inaccessible interface field
      interface Interface implements Referencer1 {
        someField: String
        privateField: String @inaccessible
      }

      # Inaccessible interface field can't be referenced by interface field in
      # the API schema
      interface Referencer1 {
        privateField: String
      }

      # Inaccessible interface field can't have a non-inaccessible parent object
      # type and no non-inaccessible siblings
      interface Referencer2 {
        privateField: String @inaccessible
        otherPrivateField: Float @inaccessible
      }
    "#,
    )
    .expect_err("should return validation errors");

    insta::assert_snapshot!(errors, @r###"
    The following errors occurred:
      - Field `Interface.privateField` is @inaccessible but implements the interface field `Referencer1.privateField`, which is in the API schema.
      - Type `Referencer2` is in the API schema but all of its members are @inaccessible.
    "###);
}

#[test]
fn removes_inaccessible_interface_fields() {
    let api_schema = inaccessible_to_api_schema(
        r#"
      type Query {
        someField: String
      }

      # Inaccessible (and non-inaccessible) interface field
      interface Interface implements Referencer1 & Referencer2 {
        someField: String
        privateField: String @inaccessible
      }

      # Inaccessible interface field referenced by inaccessible interface field
      interface Referencer1 {
        someField: String
        privateField: String @inaccessible
      }

      # Inaccessible interface field referenced by non-inaccessible interface
      # field with inaccessible parent
      interface Referencer2 @inaccessible {
        privateField: String
      }

      # Inaccessible interface field with an inaccessible parent and no
      # non-inaccessible siblings
      interface Referencer3 @inaccessible {
        privateField: String @inaccessible
        otherPrivateField: Float @inaccessible
      }
    "#,
    )
    .expect("should succeed");

    let ExtendedType::Interface(interface_type) = &api_schema.types["Interface"] else {
        panic!("should be interface");
    };
    assert!(interface_type.implements_interfaces.contains("Referencer1"));
    assert!(!interface_type.implements_interfaces.contains("Referencer2"));
    assert!(coord!(Interface.someField).lookup(&api_schema).is_ok());
    assert!(coord!(Interface.privateField).lookup(&api_schema).is_err());
    assert!(coord!(Referencer1.someField).lookup(&api_schema).is_ok());
    assert!(coord!(Referencer1.privatefield)
        .lookup(&api_schema)
        .is_err());
    assert!(api_schema.types.get("Referencer2").is_none());
    assert!(api_schema.types.get("Referencer3").is_none());
}

#[test]
fn inaccessible_object_field_arguments_with_accessible_references() {
    let errors = inaccessible_to_api_schema(
        r#"
      type Query {
        someField(someArg: String): String
      }

      # Inaccessible object field argument
      type Object implements Referencer1 {
        someField(privateArg: String @inaccessible): String
      }

      # Inaccessible object field argument can't be referenced by interface
      # field argument in the API schema
      interface Referencer1 {
        someField(privateArg: String): String
      }

      # Inaccessible object field argument can't be a required argument
      type ObjectRequired {
        someField(privateArg: String! @inaccessible): String
      }
    "#,
    )
    .expect_err("should return validation errors");

    insta::assert_snapshot!(errors, @r###"
    The following errors occurred:
      - Argument `Object.someField(privateArg:)` is @inaccessible but implements the interface argument `Referencer1.someField(privateArg:)` which is in the API schema.
      - Argument `ObjectRequired.someField(privateArg:)` is @inaccessible but is a required argument of its field.
    "###);
}

#[test]
fn removes_inaccessible_object_field_arguments() {
    let api_schema = inaccessible_to_api_schema(
        r#"
      # Inaccessible object field argument in query type
      type Query {
        someField(privateArg: String @inaccessible): String
      }

      # Inaccessible object field argument in mutation type
      type Mutation {
        someField(privateArg: String @inaccessible): String
      }

      # Inaccessible object field argument in subscription type
      type Subscription {
        someField(privateArg: String @inaccessible): String
      }

      # Inaccessible (and non-inaccessible) object field argument
      type Object implements Referencer1 & Referencer2 & Referencer3 {
        someField(
          someArg: String,
          privateArg: String @inaccessible
        ): String
        someOtherField: Float
      }

      # Inaccessible object field argument referenced by inaccessible interface
      # field argument
      interface Referencer1 {
        someField(
          someArg: String,
          privateArg: String @inaccessible
        ): String
      }

      # Inaccessible object field argument referenced by non-inaccessible
      # interface field argument with inaccessible parent
      interface Referencer2 {
        someField(
          someArg: String,
          privateArg: String
        ): String @inaccessible
        someOtherField: Float
      }

      # Inaccessible object field argument referenced by non-inaccessible
      # interface field argument with inaccessible grandparent
      interface Referencer3 @inaccessible {
        someField(
          someArg: String,
          privateArg: String
        ): String
      }

      # Inaccessible non-nullable object field argument with default
      type ObjectDefault {
        someField(privateArg: String! = "default" @inaccessible): String
      }
    "#,
    )
    .expect("should succeed");

    assert!(coord!(Query.someField).lookup(&api_schema).is_ok());
    assert!(coord!(Query.someField(privateArg:))
        .lookup(&api_schema)
        .is_err());
    assert!(coord!(Mutation.someField).lookup(&api_schema).is_ok());
    assert!(coord!(Mutation.someField(privateArg:))
        .lookup(&api_schema)
        .is_err());
    assert!(coord!(Subscription.someField).lookup(&api_schema).is_ok());
    assert!(coord!(Subscription.someField(privateArg:))
        .lookup(&api_schema)
        .is_err());
    let ExtendedType::Object(object_type) = &api_schema.types["Object"] else {
        panic!("expected object");
    };
    assert!(object_type.implements_interfaces.contains("Referencer1"));
    assert!(object_type.implements_interfaces.contains("Referencer2"));
    assert!(!object_type.implements_interfaces.contains("Referencer3"));
    assert!(coord!(Object.someField(someArg:))
        .lookup(&api_schema)
        .is_ok());
    assert!(coord!(Object.someField(privateArg:))
        .lookup(&api_schema)
        .is_err());
    assert!(coord!(Referencer1.someField).lookup(&api_schema).is_ok());
    assert!(coord!(Referencer1.someField(privateArg:))
        .lookup(&api_schema)
        .is_err());
    assert!(coord!(Referencer2).lookup(&api_schema).is_ok());
    assert!(coord!(Referencer2.someField).lookup(&api_schema).is_err());
    assert!(!api_schema.types.contains_key("Referencer3"));
    assert!(coord!(ObjectDefault.someField).lookup(&api_schema).is_ok());
    assert!(coord!(ObjectDefault.someField(privateArg:))
        .lookup(&api_schema)
        .is_err());
}

#[test]
fn inaccessible_interface_field_arguments_with_accessible_references() {
    let errors = inaccessible_to_api_schema(
        r#"
      type Query {
        someField(someArg: String): String
      }

      # Inaccessible interface field argument
      interface Interface implements Referencer1 {
        someField(privateArg: String! = "default" @inaccessible): String
      }

      # Inaccessible interface field argument can't be referenced by interface
      # field argument in the API schema
      interface Referencer1 {
        someField(privateArg: String! = "default"): String
      }

      # Inaccessible object field argument can't be a required argument
      type InterfaceRequired {
        someField(privateArg: String! @inaccessible): String
      }

      # Inaccessible object field argument can't be implemented by a required
      # object field argument in the API schema
      type Referencer2 implements Interface & Referencer1 {
        someField(privateArg: String!): String
      }

      # Inaccessible object field argument can't be implemented by a required
      # interface field argument in the API schema
      interface Referencer3 implements Interface & Referencer1 {
        someField(privateArg: String!): String
      }
    "#,
    )
    .expect_err("should return validation errors");

    insta::assert_snapshot!(errors, @r###"
    The following errors occurred:
      - Argument `Interface.someField(privateArg:)` is @inaccessible but implements the interface argument `Referencer1.someField(privateArg:)` which is in the API schema.
      - Argument `InterfaceRequired.someField(privateArg:)` is @inaccessible but is a required argument of its field.
      - Argument `Interface.someField(privateArg:)` is @inaccessible but is implemented by the argument `Referencer2.someField(privateArg:)` which is in the API schema.
      - Argument `Interface.someField(privateArg:)` is @inaccessible but is implemented by the argument `Referencer3.someField(privateArg:)` which is in the API schema.
    "###);
}

#[test]
fn removes_inaccessible_interface_field_arguments() {
    let api_schema = inaccessible_to_api_schema(
        r#"
      type Query {
        someField: String
      }

      # Inaccessible (and non-inaccessible) interface field argument
      interface Interface implements Referencer1 & Referencer2 & Referencer3 {
        someField(
          someArg: String,
          privateArg: String @inaccessible
        ): String
        someOtherField: Float
      }

      # Inaccessible interface field argument referenced by inaccessible
      # interface field argument
      interface Referencer1 {
        someField(
          someArg: String,
          privateArg: String @inaccessible
        ): String
      }

      # Inaccessible interface field argument referenced by non-inaccessible
      # interface field argument with inaccessible parent
      interface Referencer2 {
        someField(
          someArg: String,
          privateArg: String
        ): String @inaccessible
        someOtherField: Float
      }

      # Inaccessible interface field argument referenced by non-inaccessible
      # interface field argument with inaccessible grandparent
      interface Referencer3 @inaccessible {
        someField(
          someArg: String,
          privateArg: String
        ): String
      }

      # Inaccessible non-nullable interface field argument with default
      interface InterfaceDefault {
        someField(privateArg: String! = "default" @inaccessible): String
      }

      # Inaccessible interface field argument referenced by non-inaccessible
      # non-required object field argument
      type Referencer4 implements InterfaceDefault {
        someField(privateArg: String! = "default"): String
      }

      # Inaccessible interface field argument referenced by non-inaccessible
      # required object field argument with inaccessible grandparent
      type Referencer5 implements InterfaceDefault @inaccessible {
        someField(privateArg: String!): String
      }

      # Inaccessible interface field argument referenced by non-inaccessible
      # non-required interface field argument
      interface Referencer6 implements InterfaceDefault {
        someField(privateArg: String! = "default"): String
      }

      # Inaccessible interface field argument referenced by non-inaccessible
      # required interface field argument with inaccessible grandparent
      interface Referencer7 implements InterfaceDefault @inaccessible {
        someField(privateArg: String!): String
      }
    "#,
    )
    .expect("should succeed");

    let ExtendedType::Interface(interface_type) = &api_schema.types["Interface"] else {
        panic!("expected interface");
    };
    assert!(interface_type.implements_interfaces.contains("Referencer1"));
    assert!(interface_type.implements_interfaces.contains("Referencer2"));
    assert!(!interface_type.implements_interfaces.contains("Referencer3"));
    assert!(coord!(Interface.someField(someArg:))
        .lookup(&api_schema)
        .is_ok());
    assert!(coord!(Interface.someField(privateArg:))
        .lookup(&api_schema)
        .is_err());
    assert!(coord!(Referencer1.someField).lookup(&api_schema).is_ok());
    assert!(coord!(Referencer1.someField(privateArg:))
        .lookup(&api_schema)
        .is_err());
    assert!(api_schema.types.contains_key("Referencer2"));
    assert!(coord!(Referencer2.someField).lookup(&api_schema).is_err());
    assert!(!api_schema.types.contains_key("Referencer3"));
    assert!(coord!(Interface.someField(privateArg:))
        .lookup(&api_schema)
        .is_err());
    let object_argument = coord!(Referencer4.someField(privateArg:))
        .lookup(&api_schema)
        .unwrap();
    assert!(!object_argument.is_required());
    assert!(!api_schema.types.contains_key("Referencer5"));
    let interface_argument = coord!(Referencer4.someField(privateArg:))
        .lookup(&api_schema)
        .unwrap();
    assert!(!interface_argument.is_required());
    assert!(!api_schema.types.contains_key("Referencer7"));
}

#[test]
fn inaccessible_input_object_fields_with_accessible_references() {
    let errors = inaccessible_to_api_schema(
        r#"
      type Query {
        someField: String
      }

      # Inaccessible input object field
      input InputObject {
        someField: String
        privateField: String @inaccessible
      }

      # Inaccessible input object field can't be referenced by default value of
      # object field argument in the API schema
      type Referencer1 implements Referencer2 {
        someField(someArg: InputObject = { privateField: "" }): String
      }

      # Inaccessible input object field can't be referenced by default value of
      # interface field argument in the API schema
      interface Referencer2 {
        someField(someArg: InputObject = { privateField: "" }): String
      }

      # Inaccessible input object field can't be referenced by default value of
      # input object field in the API schema
      input Referencer3 {
        someField: InputObject = { privateField: "" }
      }

      # Inaccessible input object field can't be referenced by default value of
      # directive argument in the API schema
      directive @referencer4(
        someArg: InputObject = { privateField: "" }
      ) on FIELD

      # Inaccessible input object field can't have a non-inaccessible parent
      # and no non-inaccessible siblings
      input Referencer5 {
        privateField: String @inaccessible
        otherPrivateField: Float @inaccessible
      }

      # Inaccessible input object field can't be a required field
      input InputObjectRequired {
        someField: String
        privateField: String! @inaccessible
      }
    "#,
    )
    .expect_err("should return validation errors");

    insta::assert_snapshot!(errors, @r###"
    The following errors occurred:
      - Input field `InputObject.privateField` is @inaccessible but is used in the default value of `Referencer1.someField(someArg:)`, which is in the API schema.
      - Input field `InputObject.privateField` is @inaccessible but is used in the default value of `Referencer2.someField(someArg:)`, which is in the API schema.
      - Input field `InputObject.privateField` is @inaccessible but is used in the default value of `Referencer3.someField`, which is in the API schema.
      - Type `Referencer5` is in the API schema but all of its input fields are @inaccessible.
      - Input field `InputObjectRequired` is @inaccessible but is a required input field of its type.
      - Input field `InputObject.privateField` is @inaccessible but is used in the default value of `@referencer4(someArg:)`, which is in the API schema.
    "###);
}

#[test]
fn removes_inaccessible_input_object_fields() {
    let schema = inaccessible_to_api_schema(
        r#"
      type Query {
        someField: String
      }

      # Inaccessible (and non-inaccessible) input object field
      input InputObject {
        someField: String
        privateField: String @inaccessible
      }

      # Inaccessible input object field referenced by default value of
      # inaccessible object field argument
      type Referencer1 implements Referencer4 {
        someField(
          privateArg: InputObject = { privateField: "" } @inaccessible
        ): String
      }

      # Inaccessible input object field referenced by default value of
      # non-inaccessible object field argument with inaccessible parent
      type Referencer2 implements Referencer5 {
        someField: String
        privateField(
          privateArg: InputObject! = { privateField: "" }
        ): String @inaccessible
      }

      # Inaccessible input object field referenced by default value of
      # non-inaccessible object field argument with inaccessible grandparent
      type Referencer3 implements Referencer6 @inaccessible {
        privateField(privateArg: InputObject! = { privateField: "" }): String
      }

      # Inaccessible input object field referenced by default value of
      # inaccessible interface field argument
      interface Referencer4 {
        someField(
          privateArg: InputObject = { privateField: "" } @inaccessible
        ): String
      }

      # Inaccessible input object field referenced by default value of
      # non-inaccessible interface field argument with inaccessible parent
      interface Referencer5 {
        someField: String
        privateField(
          privateArg: InputObject! = { privateField: "" }
        ): String @inaccessible
      }

      # Inaccessible input object field referenced by default value of
      # non-inaccessible interface field argument with inaccessible grandparent
      interface Referencer6 @inaccessible {
        privateField(privateArg: InputObject! = { privateField: "" }): String
      }

      # Inaccessible input object field referenced by default value of
      # inaccessible input object field
      input Referencer7 {
        someField: String
        privateField: InputObject = { privateField: "" } @inaccessible
      }

      # Inaccessible input object field referenced by default value of
      # non-inaccessible input object field with inaccessible parent
      input Referencer8 @inaccessible {
        privateField: InputObject! = { privateField: "" }
      }

      # Inaccessible input object field referenced by default value of
      # inaccessible directive argument
      directive @referencer9(
        privateArg: InputObject = { privateField: "" } @inaccessible
      ) on SUBSCRIPTION

      # Inaccessible input object field not referenced (but type is referenced)
      # by default value of object field argument in the API schema
      type Referencer10 {
        someField(privateArg: InputObject = { someField: "" }): String
      }

      # Inaccessible input object field with an inaccessible parent and no
      # non-inaccessible siblings
      input Referencer11 @inaccessible {
        privateField: String @inaccessible
        otherPrivateField: Float @inaccessible
      }

      # Inaccessible non-nullable input object field with default
      input InputObjectDefault {
        someField: String
        privateField: String! = "default" @inaccessible
      }
    "#,
    )
    .expect("should succeed");

    assert!(coord!(InputObject.someField)
        .lookup_input_field(&schema)
        .is_ok());
    assert!(coord!(InputObject.privateField)
        .lookup_input_field(&schema)
        .is_err());
    assert!(coord!(Referencer1.someField).lookup(&schema).is_ok());
    assert!(coord!(Referencer1.someField(privateArg:))
        .lookup(&schema)
        .is_err());
    assert!(coord!(Referencer2.someField).lookup(&schema).is_ok());
    assert!(coord!(Referencer2.privateField).lookup(&schema).is_err());
    assert!(!schema.types.contains_key("Referencer3"));
    assert!(coord!(Referencer4.someField).lookup(&schema).is_ok());
    assert!(coord!(Referencer4.someField(privateArg:))
        .lookup(&schema)
        .is_err());
    assert!(coord!(Referencer5.someField).lookup(&schema).is_ok());
    assert!(coord!(Referencer5.privateField).lookup(&schema).is_err());
    assert!(!schema.types.contains_key("Referencer6"));
    assert!(schema
        .get_input_object("Referencer7")
        .unwrap()
        .fields
        .contains_key("someField"));
    assert!(!schema
        .get_input_object("Referencer7")
        .unwrap()
        .fields
        .contains_key("privatefield"));
    assert!(!schema.types.contains_key("Referencer8"));
    assert!(schema.directive_definitions.contains_key("referencer9"));
    assert!(coord!(@referencer9(privateArg:)).lookup(&schema).is_err());
    assert!(coord!(Referencer10.someField(privateArg:))
        .lookup(&schema)
        .is_ok());
    assert!(!schema.types.contains_key("Referencer11"));
    assert!(coord!(InputObjectDefault.someField)
        .lookup_input_field(&schema)
        .is_ok());
    assert!(coord!(InputObjectDefault.privatefield)
        .lookup_input_field(&schema)
        .is_err());
}

#[test]
fn inaccessible_enum_values_with_accessible_references() {
    let errors = inaccessible_to_api_schema(
        r#"
      type Query {
        someField: String
      }

      # Inaccessible enum value
      enum Enum {
        SOME_VALUE
        PRIVATE_VALUE @inaccessible
      }

      # Inaccessible enum value can't be referenced by default value of object
      # field argument in the API schema
      type Referencer1 implements Referencer2 {
        someField(someArg: Enum = PRIVATE_VALUE): String
      }

      # Inaccessible enum value can't be referenced by default value of
      # interface field argument in the API schema
      interface Referencer2 {
        someField(someArg: Enum = PRIVATE_VALUE): String
      }

      # Inaccessible enum value can't be referenced by default value of input
      # object field in the API schema
      input Referencer3 {
        someField: Enum = PRIVATE_VALUE
      }

      # Inaccessible input enum value can't be referenced by default value of
      # directive argument in the API schema
      directive @referencer4(someArg: Enum = PRIVATE_VALUE) on INLINE_FRAGMENT

      # Inaccessible enum value can't have a non-inaccessible parent and no
      # non-inaccessible siblings
      enum Referencer5 {
        PRIVATE_VALUE @inaccessible
        OTHER_PRIVATE_VALUE @inaccessible
      }
    "#,
    )
    .expect_err("should return validation errors");

    insta::assert_snapshot!(errors, @r###"
    The following errors occurred:
      - Enum value `Enum.PRIVATE_VALUE` is @inaccessible but is used in the default value of `Referencer1.someField(someArg:)`, which is in the API schema.
      - Enum value `Enum.PRIVATE_VALUE` is @inaccessible but is used in the default value of `Referencer2.someField(someArg:)`, which is in the API schema.
      - Enum value `Enum.PRIVATE_VALUE` is @inaccessible but is used in the default value of `Referencer3.someField`, which is in the API schema.
      - Type `Referencer5` is in the API schema but all of its members are @inaccessible.
      - Enum value `Enum.PRIVATE_VALUE` is @inaccessible but is used in the default value of `@referencer4(someArg:)`, which is in the API schema.
    "###);
}

#[test]
fn removes_inaccessible_enum_values() {
    let api_schema = inaccessible_to_api_schema(
        r#"
      type Query {
        someField: String
      }

      # Inaccessible (and non-inaccessible) enum value
      enum Enum {
        SOME_VALUE
        PRIVATE_VALUE @inaccessible
      }

      # Inaccessible enum value referenced by default value of inaccessible
      # object field argument
      type Referencer1 implements Referencer4 {
        someField(
          privateArg: Enum = PRIVATE_VALUE @inaccessible
        ): String
      }

      # Inaccessible enum value referenced by default value of non-inaccessible
      # object field argument with inaccessible parent
      type Referencer2 implements Referencer5 {
        someField: String
        privateField(
          privateArg: Enum! = PRIVATE_VALUE
        ): String @inaccessible
      }

      # Inaccessible enum value referenced by default value of non-inaccessible
      # object field argument with inaccessible grandparent
      type Referencer3 implements Referencer6 @inaccessible {
        privateField(privateArg: Enum! = PRIVATE_VALUE): String
      }

      # Inaccessible enum value referenced by default value of inaccessible
      # interface field argument
      interface Referencer4 {
        someField(
          privateArg: Enum = PRIVATE_VALUE @inaccessible
        ): String
      }

      # Inaccessible enum value referenced by default value of non-inaccessible
      # interface field argument with inaccessible parent
      interface Referencer5 {
        someField: String
        privateField(
          privateArg: Enum! = PRIVATE_VALUE
        ): String @inaccessible
      }

      # Inaccessible enum value referenced by default value of non-inaccessible
      # interface field argument with inaccessible grandparent
      interface Referencer6 @inaccessible {
        privateField(privateArg: Enum! = PRIVATE_VALUE): String
      }

      # Inaccessible enum value referenced by default value of inaccessible
      # input object field
      input Referencer7 {
        someField: String
        privateField: Enum = PRIVATE_VALUE @inaccessible
      }

      # Inaccessible enum value referenced by default value of non-inaccessible
      # input object field with inaccessible parent
      input Referencer8 @inaccessible {
        privateField: Enum! = PRIVATE_VALUE
      }

      # Inaccessible enum value referenced by default value of inaccessible
      # directive argument
      directive @referencer9(
        privateArg: Enum = PRIVATE_VALUE @inaccessible
      ) on FRAGMENT_SPREAD

      # Inaccessible enum value not referenced (but type is referenced) by
      # default value of object field argument in the API schema
      type Referencer10 {
        someField(privateArg: Enum = SOME_VALUE): String
      }

      # Inaccessible enum value with an inaccessible parent and no
      # non-inaccessible siblings
      enum Referencer11 @inaccessible {
        PRIVATE_VALUE @inaccessible
        OTHER_PRIVATE_VALUE @inaccessible
      }
    "#,
    )
    .expect("should succeed");

    assert!(coord!(Enum.SOME_VALUE)
        .lookup_enum_value(&api_schema)
        .is_ok());
    assert!(coord!(Enum.PRIVATE_VALUE)
        .lookup_enum_value(&api_schema)
        .is_err());
    assert!(coord!(Referencer1.someField).lookup(&api_schema).is_ok());
    assert!(coord!(Referencer1.someField(privateArg:))
        .lookup(&api_schema)
        .is_err());
    assert!(coord!(Referencer2.someField).lookup(&api_schema).is_ok());
    assert!(coord!(Referencer2.privateField)
        .lookup(&api_schema)
        .is_err());
    assert!(!api_schema.types.contains_key("Referencer3"));
    assert!(coord!(Referencer4.someField).lookup(&api_schema).is_ok());
    assert!(coord!(Referencer4.someField(privateArg:))
        .lookup(&api_schema)
        .is_err());
    assert!(coord!(Referencer5.someField).lookup(&api_schema).is_ok());
    assert!(coord!(Referencer5.privateField)
        .lookup(&api_schema)
        .is_err());
    assert!(!api_schema.types.contains_key("Referencer6"));
    assert!(coord!(Referencer7.someField)
        .lookup_input_field(&api_schema)
        .is_ok());
    assert!(coord!(Referencer7.privatefield)
        .lookup_input_field(&api_schema)
        .is_err());
    assert!(!api_schema.types.contains_key("Referencer8"));
    assert!(coord!(@referencer9).lookup(&api_schema).is_ok());
    assert!(coord!(@referencer9(privateArg:))
        .lookup(&api_schema)
        .is_err());
    assert!(coord!(Referencer10.someField(privateArg:))
        .lookup(&api_schema)
        .is_ok());
    assert!(!api_schema.types.contains_key("Referencer11"));
}

#[test]
fn inaccessible_complex_default_values() {
    let errors = inaccessible_to_api_schema(
        r#"
      type Query {
        someField(arg1: [[RootInputObject!]]! = [
          {
            foo: {
              # 2 references (with nesting)
              privateField: [PRIVATE_VALUE]
            }
            bar: SOME_VALUE
            # 0 references since scalar
            baz: { privateField: PRIVATE_VALUE }
          },
          [{
            foo: [{
              someField: "foo"
            }]
            # 1 reference
            bar: PRIVATE_VALUE
          }]
        ]): String
      }

      input RootInputObject {
        foo: [NestedInputObject]
        bar: Enum!
        baz: Scalar! = { default: 4 }
      }

      input NestedInputObject {
        someField: String
        privateField: [Enum!] @inaccessible
      }

      enum Enum {
        SOME_VALUE
        PRIVATE_VALUE @inaccessible
      }

      scalar Scalar
    "#,
    )
    .expect_err("should return validation errors");

    insta::assert_snapshot!(errors, @r###"
    The following errors occurred:
      - Input field `NestedInputObject.privateField` is @inaccessible but is used in the default value of `Query.someField(arg1:)`, which is in the API schema.
      - Enum value `Enum.PRIVATE_VALUE` is @inaccessible but is used in the default value of `Query.someField(arg1:)`, which is in the API schema.
      - Enum value `Enum.PRIVATE_VALUE` is @inaccessible but is used in the default value of `Query.someField(arg1:)`, which is in the API schema.
    "###);
}

/// It's not GraphQL-spec-compliant to allow a string for an enum value, but
/// since we're allowing it, we need to make sure this logic keeps working
/// until we're allowed to make breaking changes and remove it.
#[test]
fn inaccessible_enum_value_as_string() {
    let errors = inaccessible_to_api_schema(
        r#"
      type Query {
        someField(arg1: Enum! = "PRIVATE_VALUE"): String
      }

      enum Enum {
        SOME_VALUE
        PRIVATE_VALUE @inaccessible
      }
    "#,
    )
    .expect_err("should return validation errors");

    insta::assert_snapshot!(errors, @r###"
    The following errors occurred:
      - Enum value `Enum.PRIVATE_VALUE` is @inaccessible but is used in the default value of `Query.someField(arg1:)`, which is in the API schema.
    "###);
}

#[test]
fn inaccessible_directive_arguments_with_accessible_references() {
    let errors = inaccessible_to_api_schema(
        r#"
      type Query {
        someField: String
      }

      # Inaccessible directive argument
      directive @directive(privateArg: String @inaccessible) on SUBSCRIPTION

      # Inaccessible directive argument can't be a required field
      directive @directiveRequired(
        someArg: String
        privateArg: String! @inaccessible
      ) on FRAGMENT_DEFINITION
    "#,
    )
    .expect_err("should return validation errors");

    insta::assert_snapshot!(errors, @r###"
    The following errors occurred:
      - Argument `@directiveRequired(privateArg:)` is @inaccessible but is a required argument of its directive.
    "###);
}

#[test]
fn removes_inaccessible_directive_arguments() {
    let api_schema = inaccessible_to_api_schema(
        r#"
      type Query {
        someField: String
      }

      # Inaccessible (and non-inaccessible) directive argument
      directive @directive(
        someArg: String
        privateArg: String @inaccessible
      ) on QUERY

      # Inaccessible non-nullable directive argument with default
      directive @directiveDefault(
        someArg: String
        privateArg: String! = "default" @inaccessible
      ) on MUTATION
    "#,
    )
    .expect("should succeed");

    assert!(coord!(@directive(someArg:)).lookup(&api_schema).is_ok());
    assert!(coord!(@directive(privateArg:)).lookup(&api_schema).is_err());
    assert!(coord!(@directiveDefault(someArg:))
        .lookup(&api_schema)
        .is_ok());
    assert!(coord!(@directiveDefault(privateArg:))
        .lookup(&api_schema)
        .is_err());
}

#[test]
fn inaccessible_directive_on_schema_elements() {
    let errors = inaccessible_to_api_schema(
        r#"
      type Query {
        someField: String
      }

      directive @foo(arg1: String @inaccessible) repeatable on OBJECT

      directive @bar(arg2: String, arg3: String @inaccessible) repeatable on SCHEMA | FIELD
    "#,
    )
    .expect_err("should return validation errors");

    insta::assert_snapshot!(errors, @r###"
    The following errors occurred:
      - Directive `@foo` cannot use @inaccessible because it may be applied to these type-system locations: OBJECT
      - Directive `@bar` cannot use @inaccessible because it may be applied to these type-system locations: SCHEMA
    "###);
}

#[test]
fn inaccessible_on_builtins() {
    let errors = inaccessible_to_api_schema(
        r#"
      type Query {
        someField: String
      }

      # Built-in scalar
      scalar String @inaccessible

      # Built-in directive
      directive @deprecated(
        reason: String = "No longer supported" @inaccessible
      ) on FIELD_DEFINITION | ENUM_VALUE
    "#,
    )
    .expect_err("should return validation errors");

    // Note this is different from the JS implementation
    insta::assert_snapshot!(errors, @r###"
    The following errors occurred:
      - built-in scalar definitions must be omitted
    "###);
}

#[test]
fn inaccessible_on_imported_elements() {
    // TODO(@goto-bus-stop): this `@link`s the join spec but doesn't use it, just because the
    // testing code goes through the Supergraph API. See comment at top of file
    let graph = Supergraph::new(
        r#"
      directive @link(url: String!, as: String, import: [link__Import] @inaccessible, for: link__Purpose) repeatable on SCHEMA

      scalar link__Import

      enum link__Purpose {
        EXECUTION @inaccessible
        SECURITY
      }

      directive @inaccessible on FIELD_DEFINITION | OBJECT | INTERFACE | UNION | ARGUMENT_DEFINITION | SCALAR | ENUM | ENUM_VALUE | INPUT_OBJECT | INPUT_FIELD_DEFINITION

      schema
        @link(url: "https://specs.apollo.dev/link/v1.0")
        @link(url: "https://specs.apollo.dev/join/v0.2")
        @link(url: "https://specs.apollo.dev/inaccessible/v0.2")
        @link(url: "http://localhost/foo/v1.0")
      {
        query: Query
      }

      type Query {
        someField: String!
      }

      # Object type
      type foo__Object1 @inaccessible {
        foo__Field: String!
      }

      # Object field
      type foo__Object2 implements foo__Interface2 {
        foo__Field: foo__Enum1! @inaccessible
      }

      # Object field argument
      type foo__Object3 {
        someField(someArg: foo__Enum1 @inaccessible): foo__Enum2!
      }

      # Interface type
      interface foo__Interface1 @inaccessible {
        foo__Field: String!
      }

      # Interface field
      interface foo__Interface2 {
        foo__Field: foo__Enum1! @inaccessible
      }

      # Interface field argument
      interface foo__Interface3 {
        someField(someArg: foo__InputObject1 @inaccessible): foo__Enum2!
      }

      # Union type
      union foo__Union @inaccessible = foo__Object1 | foo__Object2 | foo__Object3

      # Input object type
      input foo__InputObject1 @inaccessible {
        someField: foo__Enum1
      }

      # Input object field
      input foo__InputObject2 {
        someField: foo__Scalar @inaccessible
      }

      # Enum type
      enum foo__Enum1 @inaccessible {
        someValue
      }

      # Enum value
      enum foo__Enum2 {
        someValue @inaccessible
      }

      # Scalar type
      scalar foo__Scalar @inaccessible

      # Directive argument
      directive @foo(arg: foo__InputObject2 @inaccessible) repeatable on OBJECT
    "#,
    )
    .unwrap();

    let errors = graph
        .to_api_schema(Default::default())
        .expect_err("should return validation errors");

    insta::assert_snapshot!(errors, @r###"
    The following errors occurred:
      - Core feature type `link__Purpose` cannot use @inaccessible.
      - Core feature type `foo__Object1` cannot use @inaccessible.
      - Core feature type `foo__Object2` cannot use @inaccessible.
      - Core feature type `foo__Object3` cannot use @inaccessible.
      - Core feature type `foo__Interface1` cannot use @inaccessible.
      - Core feature type `foo__Interface2` cannot use @inaccessible.
      - Core feature type `foo__Interface3` cannot use @inaccessible.
      - Core feature type `foo__Union` cannot use @inaccessible.
      - Core feature type `foo__InputObject1` cannot use @inaccessible.
      - Core feature type `foo__InputObject2` cannot use @inaccessible.
      - Core feature type `foo__Enum1` cannot use @inaccessible.
      - Core feature type `foo__Enum2` cannot use @inaccessible.
      - Core feature type `foo__Scalar` cannot use @inaccessible.
      - Core feature directive `@link` cannot use @inaccessible.
      - Core feature directive `@foo` cannot use @inaccessible.
    "###);
}

#[test]
fn propagates_default_input_values() {
    let api_schema = inaccessible_to_api_schema(
        r#"
        type Query {
            field(input: Input = { one: 0, nested: { one: 2 } }): Int
        }
        input Input {
            one: Int! = 1
            two: Int! = 2
            three: Int! = 3
            object: InputObject = { value: 2 }
            nested: Nested
            nestedWithDefault: Nested! = {}
        }
        input InputObject {
            value: Int
        }
        input Nested {
            noDefault: String
            one: Int! = 1
            two: Int! = 2
            default: String = "default"
        }
        "#,
    )
    .expect("should succeed");

    insta::assert_snapshot!(api_schema, @r###"
    type Query {
      field(input: Input = {one: 0, nested: {one: 2, two: 2, default: "default"}, two: 2, three: 3, object: {value: 2}, nestedWithDefault: {one: 1, two: 2, default: "default"}}): Int
    }

    input Input {
      one: Int! = 1
      two: Int! = 2
      three: Int! = 3
      object: InputObject = {
        value: 2,
      }
      nested: Nested
      nestedWithDefault: Nested! = {
        one: 1,
        two: 2,
        default: "default",
      }
    }

    input InputObject {
      value: Int
    }

    input Nested {
      noDefault: String
      one: Int! = 1
      two: Int! = 2
      default: String = "default"
    }
    "###);
}

#[test]
fn matches_graphql_js_directive_applications() {
    let api_schema = inaccessible_to_api_schema(
        r#"
        type Query {
            a: Int @deprecated
            b: Int @deprecated(reason: null)
            c: Int @deprecated(reason: "Reason")
            d: Int @deprecated(reason: "No longer supported")
        }
        "#,
    )
    .expect("should succeed");

    insta::assert_snapshot!(api_schema, @r###"
        type Query {
          a: Int @deprecated
          b: Int
          c: Int @deprecated(reason: "Reason")
          d: Int @deprecated
        }
    "###);
}

#[test]
fn matches_graphql_js_default_value_propagation() {
    let api_schema = inaccessible_to_api_schema(
        r#"
        type Query {
          defaultShouldBeRemoved(arg: OneRequiredOneDefault = {}): Int
          defaultShouldHavePropagatedValues(arg: OneOptionalOneDefault = {}): Int
        }

        input OneOptionalOneDefault {
          notDefaulted: Int
          defaulted: Boolean = false
        }

        input OneRequiredOneDefault {
          notDefaulted: Int!
          defaulted: Boolean = false
        }
        "#,
    )
    .expect("should succeed");

    insta::assert_snapshot!(api_schema, @r###"
    type Query {
      defaultShouldBeRemoved(arg: OneRequiredOneDefault): Int
      defaultShouldHavePropagatedValues(arg: OneOptionalOneDefault = {defaulted: false}): Int
    }

    input OneOptionalOneDefault {
      notDefaulted: Int
      defaulted: Boolean = false
    }

    input OneRequiredOneDefault {
      notDefaulted: Int!
      defaulted: Boolean = false
    }
    "###);
}

#[test]
fn remove_referencing_directive_argument() {
    let api_schema = inaccessible_to_api_schema(
        r#"
        extend schema @link(url: "https://example.com/directives/v0.0", as: "d")

        # Set up a chain of core feature directives
        # that are referenced in each other's arguments
        # to make sure we remove directives safely
        directive @d__example_2(
            arg1: Int @d__example(arg: 1)
            arg: Int @d__arg
        ) on ARGUMENT_DEFINITION | FIELD
        directive @d__arg(
            arg: Int
        ) on ARGUMENT_DEFINITION | FIELD
        directive @d__example(
            arg: Int! @d__arg
        ) on ARGUMENT_DEFINITION | FIELD
        directive @d__example_3(
            arg: Int! @d__example_2
        ) on ARGUMENT_DEFINITION | FIELD

        type Query {
            a: Int
        }
    "#,
    )
    .expect("should succeed");

    insta::assert_snapshot!(api_schema, @r###"
    type Query {
      a: Int
    }
    "###);
}

#[test]
fn include_supergraph_directives() -> Result<(), FederationError> {
    let sdl = format!(
        "
      {INACCESSIBLE_V02_HEADER}
      type Query {{
        a: Int
      }}
    "
    );
    let graph = Supergraph::new(&sdl)?;
    let api_schema = graph.to_api_schema(ApiSchemaOptions {
        include_defer: true,
        include_stream: true,
    })?;

    insta::assert_snapshot!(api_schema.schema(), @r###"
    directive @defer(label: String, if: Boolean! = true) on FRAGMENT_SPREAD | INLINE_FRAGMENT

    directive @stream(label: String, if: Boolean! = true, initialCount: Int = 0) on FIELD

    type Query {
      a: Int
    }
    "###);

    Ok(())
}

#[test]
fn supports_core_directive_supergraph() {
    let sdl = r#"
schema
  @core(feature: "https://specs.apollo.dev/core/v0.2")
  @core(feature: "https://specs.apollo.dev/join/v0.2")
{
  query: Query
}

directive @core(feature: String!, as: String) repeatable on SCHEMA

directive @join__field(
  graph: join__Graph
  requires: join__FieldSet
  provides: join__FieldSet
) on FIELD_DEFINITION

directive @join__type(
  graph: join__Graph!
  key: join__FieldSet
) repeatable on OBJECT | INTERFACE

directive @join__owner(graph: join__Graph!) on OBJECT | INTERFACE

directive @join__graph(name: String!, url: String!) on ENUM_VALUE

scalar join__FieldSet

enum join__Graph {
  ACCOUNTS @join__graph(name: "accounts", url: "http://localhost:4001/graphql")
}

type Query {
  me: String
}
    "#;

    let graph = Supergraph::new(sdl).expect("should succeed");
    let api_schema = graph
        .to_api_schema(Default::default())
        .expect("should succeed");

    insta::assert_snapshot!(api_schema.schema(), @r###"
    type Query {
      me: String
    }
    "###);
}
