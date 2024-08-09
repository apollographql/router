use apollo_compiler::coord;
use apollo_compiler::schema::Value;
use apollo_compiler::Node;
use apollo_federation::Supergraph;

#[test]
fn can_extract_subgraph() {
    let schema = r#"
      schema
        @link(url: "https://specs.apollo.dev/link/v1.0")
        @link(url: "https://specs.apollo.dev/join/v0.3", for: EXECUTION)
      {
        query: Query
      }

      directive @join__enumValue(graph: join__Graph!) repeatable on ENUM_VALUE

      directive @join__field(graph: join__Graph, requires: join__FieldSet, provides: join__FieldSet, type: String, external: Boolean, override: String, usedOverridden: Boolean) repeatable on FIELD_DEFINITION | INPUT_FIELD_DEFINITION

      directive @join__graph(name: String!, url: String!) on ENUM_VALUE

      directive @join__implements(graph: join__Graph!, interface: String!) repeatable on OBJECT | INTERFACE

      directive @join__type(graph: join__Graph!, key: join__FieldSet, extension: Boolean! = false, resolvable: Boolean! = true, isInterfaceObject: Boolean! = false) repeatable on OBJECT | INTERFACE | UNION | ENUM | INPUT_OBJECT | SCALAR

      directive @join__unionMember(graph: join__Graph!, member: String!) repeatable on UNION

      directive @link(url: String, as: String, for: link__Purpose, import: [link__Import]) repeatable on SCHEMA

      enum E
        @join__type(graph: SUBGRAPH2)
      {
        V1 @join__enumValue(graph: SUBGRAPH2)
        V2 @join__enumValue(graph: SUBGRAPH2)
      }

      scalar join__FieldSet

      enum join__Graph {
        SUBGRAPH1 @join__graph(name: "Subgraph1", url: "https://Subgraph1")
        SUBGRAPH2 @join__graph(name: "Subgraph2", url: "https://Subgraph2")
      }

      scalar link__Import

      enum link__Purpose {
        """
        \`SECURITY\` features provide metadata necessary to securely resolve fields.
        """
        SECURITY

        """
        \`EXECUTION\` features provide metadata necessary for operation execution.
        """
        EXECUTION
      }

      type Query
        @join__type(graph: SUBGRAPH1)
        @join__type(graph: SUBGRAPH2)
      {
        t: T @join__field(graph: SUBGRAPH1)
      }

      type S
        @join__type(graph: SUBGRAPH1)
      {
        x: Int
      }

      type T
        @join__type(graph: SUBGRAPH1, key: "k")
        @join__type(graph: SUBGRAPH2, key: "k")
      {
        k: ID
        a: Int @join__field(graph: SUBGRAPH2)
        b: String @join__field(graph: SUBGRAPH2)
      }

      union U
        @join__type(graph: SUBGRAPH1)
        @join__unionMember(graph: SUBGRAPH1, member: "S")
        @join__unionMember(graph: SUBGRAPH1, member: "T")
       = S | T
    "#;

    let supergraph = Supergraph::new(schema).unwrap();
    let subgraphs = supergraph
        .extract_subgraphs()
        .expect("Should have been able to extract subgraphs");

    let mut snapshot = String::new();
    for (_name, subgraph) in subgraphs {
        use std::fmt::Write;

        _ = writeln!(
            &mut snapshot,
            "{}: {}\n---\n{}",
            subgraph.name,
            subgraph.url,
            subgraph.schema.schema()
        );
    }
    insta::assert_snapshot!(snapshot);
}

#[test]
fn preserve_default_values_of_input_fields() {
    let supergraph = Supergraph::new(r#"
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

    input Input
      @join__type(graph: SERVICE)
    {
      a: Int! = 1234
    }

    scalar join__FieldSet

    enum join__Graph {
      SERVICE @join__graph(name: "service", url: "")
    }

    scalar link__Import

    enum link__Purpose {
      """
      \`SECURITY\` features provide metadata necessary to securely resolve fields.
      """
      SECURITY

      """
      \`EXECUTION\` features provide metadata necessary for operation execution.
      """
      EXECUTION
    }

    type Query
      @join__type(graph: SERVICE)
    {
      field(input: Input!): String
    }
    "#).expect("should parse");

    let subgraphs = supergraph
        .extract_subgraphs()
        .expect("should extract subgraphs");

    let service = subgraphs
        .get("service")
        .expect("missing subgraph")
        .schema
        .schema();
    let field_a = coord!(Input.a).lookup_input_field(service).unwrap();
    assert_eq!(
        field_a.default_value,
        Some(Node::new(Value::Int(1234.into())))
    );
}

#[test]
fn erase_empty_types_due_to_overridden_fields() {
    let supergraph = Supergraph::new(r#"
    schema
      @link(url: "https://specs.apollo.dev/link/v1.0")
      @link(url: "https://specs.apollo.dev/join/v0.3", for: EXECUTION)
      @link(url: "https://specs.apollo.dev/tag/v0.3")
    {
      query: Query
    }

    directive @join__enumValue(graph: join__Graph!) repeatable on ENUM_VALUE

    directive @join__field(graph: join__Graph, requires: join__FieldSet, provides: join__FieldSet, type: String, external: Boolean, override: String, usedOverridden: Boolean) repeatable on FIELD_DEFINITION | INPUT_FIELD_DEFINITION

    directive @join__graph(name: String!, url: String!) on ENUM_VALUE

    directive @join__implements(graph: join__Graph!, interface: String!) repeatable on OBJECT | INTERFACE

    directive @join__type(graph: join__Graph!, key: join__FieldSet, extension: Boolean! = false, resolvable: Boolean! = true, isInterfaceObject: Boolean! = false) repeatable on OBJECT | INTERFACE | UNION | ENUM | INPUT_OBJECT | SCALAR

    directive @join__unionMember(graph: join__Graph!, member: String!) repeatable on UNION

    directive @link(url: String, as: String, for: link__Purpose, import: [link__Import]) repeatable on SCHEMA

    directive @tag(name: String!) repeatable on FIELD_DEFINITION | OBJECT | INTERFACE | UNION | ARGUMENT_DEFINITION | SCALAR | ENUM | ENUM_VALUE | INPUT_OBJECT | INPUT_FIELD_DEFINITION | SCHEMA
    input Input
      @join__type(graph: B)
    {
      a: Int! = 1234
    }

    scalar join__FieldSet

    enum join__Graph {
      A @join__graph(name: "a", url: "")
      B @join__graph(name: "b", url: "")
    }

    scalar link__Import

    enum link__Purpose {
      """
      \`SECURITY\` features provide metadata necessary to securely resolve fields.
      """
      SECURITY

      """
      \`EXECUTION\` features provide metadata necessary for operation execution.
      """
      EXECUTION
    }

    type Query
      @join__type(graph: A)
    {
      field: String
    }

    type User
    @join__type(graph: A)
    @join__type(graph: B)
    {
      foo: String @join__field(graph: A, override: "b")

      bar: String @join__field(graph: A)

      baz: String @join__field(graph: A)
    }
    "#).expect("should parse");

    let subgraphs = supergraph
        .extract_subgraphs()
        .expect("should extract subgraphs");

    let b = subgraphs
        .get("b")
        .expect("missing subgraph")
        .schema
        .schema();
    assert!(!b.types.contains_key("User"));
}

#[test]
fn extracts_demand_control_directives() {
    let subgraphs = Supergraph::new(r#"
        schema
          @link(url: "https://specs.apollo.dev/link/v1.0")
          @link(url: "https://specs.apollo.dev/join/v0.5", for: EXECUTION)
          @link(url: "https://specs.apollo.dev/cost/v0.1", import: ["@cost", "@listSize"])
        {
          query: Query
        }
        
        directive @cost(weight: Int!) on ARGUMENT_DEFINITION | ENUM | FIELD_DEFINITION | INPUT_FIELD_DEFINITION | OBJECT | SCALAR
        
        directive @cost__listSize(assumedSize: Int, slicingArguments: [String!], sizedFields: [String!], requireOneSlicingArgument: Boolean = true) on FIELD_DEFINITION
        
        directive @join__directive(graphs: [join__Graph!], name: String!, args: join__DirectiveArguments) repeatable on SCHEMA | OBJECT | INTERFACE | FIELD_DEFINITION
        
        directive @join__enumValue(graph: join__Graph!) repeatable on ENUM_VALUE
        
        directive @join__field(graph: join__Graph, requires: join__FieldSet, provides: join__FieldSet, type: String, external: Boolean, override: String, usedOverridden: Boolean, overrideLabel: String, contextArguments: [join__ContextArgument!]) repeatable on FIELD_DEFINITION | INPUT_FIELD_DEFINITION
        
        directive @join__graph(name: String!, url: String!) on ENUM_VALUE
        
        directive @join__implements(graph: join__Graph!, interface: String!) repeatable on OBJECT | INTERFACE
        
        directive @join__type(graph: join__Graph!, key: join__FieldSet, extension: Boolean! = false, resolvable: Boolean! = true, isInterfaceObject: Boolean! = false) repeatable on OBJECT | INTERFACE | UNION | ENUM | INPUT_OBJECT | SCALAR
        
        directive @join__unionMember(graph: join__Graph!, member: String!) repeatable on UNION
        
        directive @link(url: String, as: String, for: link__Purpose, import: [link__Import]) repeatable on SCHEMA
        
        directive @listSize(assumedSize: Int, slicingArguments: [String!], sizedFields: [String!], requireOneSlicingArgument: Boolean = true) on FIELD_DEFINITION
        
        enum AorB
          @join__type(graph: SUBGRAPHWITHCOST)
          @cost(weight: 15)
        {
          A @join__enumValue(graph: SUBGRAPHWITHCOST)
          B @join__enumValue(graph: SUBGRAPHWITHCOST)
        }
        
        scalar ExpensiveInt
          @join__type(graph: SUBGRAPHWITHCOST)
          @cost(weight: 30)
        
        type ExpensiveObject
          @join__type(graph: SUBGRAPHWITHCOST)
          @cost(weight: 40)
        {
          id: ID
        }
        
        type HasInts
          @join__type(graph: SUBGRAPHWITHLISTSIZE)
        {
          ints: [Int!]
        }
        
        input InputTypeWithCost
          @join__type(graph: SUBGRAPHWITHCOST)
        {
          somethingWithCost: Int @cost(weight: 20)
        }
        
        input join__ContextArgument {
          name: String!
          type: String!
          context: String!
          selection: join__FieldValue!
        }
        
        scalar join__DirectiveArguments
        
        scalar join__FieldSet
        
        scalar join__FieldValue
        
        enum join__Graph {
          SUBGRAPHWITHCOST @join__graph(name: "subgraphWithCost", url: "")
          SUBGRAPHWITHLISTSIZE @join__graph(name: "subgraphWithListSize", url: "")
        }
        
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
        
        type Query
          @join__type(graph: SUBGRAPHWITHCOST)
          @join__type(graph: SUBGRAPHWITHLISTSIZE)
        {
          fieldWithCost: Int @join__field(graph: SUBGRAPHWITHCOST) @cost(weight: 5)
          argWithCost(arg: Int @cost(weight: 10)): Int @join__field(graph: SUBGRAPHWITHCOST)
          enumWithCost: AorB @join__field(graph: SUBGRAPHWITHCOST)
          inputWithCost(someInput: InputTypeWithCost): Int @join__field(graph: SUBGRAPHWITHCOST)
          scalarWithCost: ExpensiveInt @join__field(graph: SUBGRAPHWITHCOST)
          objectWithCost: ExpensiveObject @join__field(graph: SUBGRAPHWITHCOST)
          fieldWithListSize: [String!] @join__field(graph: SUBGRAPHWITHLISTSIZE) @listSize(assumedSize: 2000, requireOneSlicingArgument: false)
          fieldWithDynamicListSize(first: Int!): HasInts @join__field(graph: SUBGRAPHWITHLISTSIZE) @listSize(slicingArguments: ["first"], sizedFields: ["ints"], requireOneSlicingArgument: true)
        }
    "#)
    .expect("is supergraph")
    .extract_subgraphs()
    .expect("extracts subgraphs");

    let mut snapshot = String::new();
    for (_name, subgraph) in subgraphs {
        use std::fmt::Write;

        _ = writeln!(
            &mut snapshot,
            "{}\n---\n{}",
            subgraph.name,
            subgraph.schema.schema()
        );
    }
    insta::assert_snapshot!(snapshot);
}

#[test]
fn extracts_renamed_demand_control_directives() {
    let subgraphs = Supergraph::new(r#"
      schema
        @link(url: "https://specs.apollo.dev/link/v1.0")
        @link(url: "https://specs.apollo.dev/join/v0.5", for: EXECUTION)
        @link(url: "https://specs.apollo.dev/cost/v0.1", import: [{name: "@cost", as: "@renamedCost"}, {name: "@listSize", as: "@renamedListSize"}])
      {
        query: Query
      }
      
      directive @cost__listSize(assumedSize: Int, slicingArguments: [String!], sizedFields: [String!], requireOneSlicingArgument: Boolean = true) on FIELD_DEFINITION
      
      directive @join__directive(graphs: [join__Graph!], name: String!, args: join__DirectiveArguments) repeatable on SCHEMA | OBJECT | INTERFACE | FIELD_DEFINITION
      
      directive @join__enumValue(graph: join__Graph!) repeatable on ENUM_VALUE
      
      directive @join__field(graph: join__Graph, requires: join__FieldSet, provides: join__FieldSet, type: String, external: Boolean, override: String, usedOverridden: Boolean, overrideLabel: String, contextArguments: [join__ContextArgument!]) repeatable on FIELD_DEFINITION | INPUT_FIELD_DEFINITION
      
      directive @join__graph(name: String!, url: String!) on ENUM_VALUE
      
      directive @join__implements(graph: join__Graph!, interface: String!) repeatable on OBJECT | INTERFACE
      
      directive @join__type(graph: join__Graph!, key: join__FieldSet, extension: Boolean! = false, resolvable: Boolean! = true, isInterfaceObject: Boolean! = false) repeatable on OBJECT | INTERFACE | UNION | ENUM | INPUT_OBJECT | SCALAR
      
      directive @join__unionMember(graph: join__Graph!, member: String!) repeatable on UNION
      
      directive @link(url: String, as: String, for: link__Purpose, import: [link__Import]) repeatable on SCHEMA
      
      directive @renamedCost(weight: Int!) on ARGUMENT_DEFINITION | ENUM | FIELD_DEFINITION | INPUT_FIELD_DEFINITION | OBJECT | SCALAR
      
      directive @renamedListSize(assumedSize: Int, slicingArguments: [String!], sizedFields: [String!], requireOneSlicingArgument: Boolean = true) on FIELD_DEFINITION
      
      enum AorB
        @join__type(graph: SUBGRAPHWITHCOST)
        @renamedCost(weight: 15)
      {
        A @join__enumValue(graph: SUBGRAPHWITHCOST)
        B @join__enumValue(graph: SUBGRAPHWITHCOST)
      }
      
      scalar ExpensiveInt
        @join__type(graph: SUBGRAPHWITHCOST)
        @renamedCost(weight: 30)
      
      type ExpensiveObject
        @join__type(graph: SUBGRAPHWITHCOST)
        @renamedCost(weight: 40)
      {
        id: ID
      }
      
      type HasInts
        @join__type(graph: SUBGRAPHWITHLISTSIZE)
      {
        ints: [Int!]
      }
      
      input InputTypeWithCost
        @join__type(graph: SUBGRAPHWITHCOST)
      {
        somethingWithCost: Int @renamedCost(weight: 20)
      }
      
      input join__ContextArgument {
        name: String!
        type: String!
        context: String!
        selection: join__FieldValue!
      }
      
      scalar join__DirectiveArguments
      
      scalar join__FieldSet
      
      scalar join__FieldValue
      
      enum join__Graph {
        SUBGRAPHWITHCOST @join__graph(name: "subgraphWithCost", url: "")
        SUBGRAPHWITHLISTSIZE @join__graph(name: "subgraphWithListSize", url: "")
      }
      
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
      
      type Query
        @join__type(graph: SUBGRAPHWITHCOST)
        @join__type(graph: SUBGRAPHWITHLISTSIZE)
      {
        fieldWithCost: Int @join__field(graph: SUBGRAPHWITHCOST) @renamedCost(weight: 5)
        argWithCost(arg: Int @renamedCost(weight: 10)): Int @join__field(graph: SUBGRAPHWITHCOST)
        enumWithCost: AorB @join__field(graph: SUBGRAPHWITHCOST)
        inputWithCost(someInput: InputTypeWithCost): Int @join__field(graph: SUBGRAPHWITHCOST)
        scalarWithCost: ExpensiveInt @join__field(graph: SUBGRAPHWITHCOST)
        objectWithCost: ExpensiveObject @join__field(graph: SUBGRAPHWITHCOST)
        fieldWithListSize: [String!] @join__field(graph: SUBGRAPHWITHLISTSIZE) @renamedListSize(assumedSize: 2000, requireOneSlicingArgument: false)
        fieldWithDynamicListSize(first: Int!): HasInts @join__field(graph: SUBGRAPHWITHLISTSIZE) @renamedListSize(slicingArguments: ["first"], sizedFields: ["ints"], requireOneSlicingArgument: true)
      }
  "#)
  .expect("parses")
  .extract_subgraphs()
  .expect("extracts");

    let mut snapshot = String::new();
    for (_name, subgraph) in subgraphs {
        use std::fmt::Write;

        _ = writeln!(
            &mut snapshot,
            "{}\n---\n{}",
            subgraph.name,
            subgraph.schema.schema()
        );
    }
    insta::assert_snapshot!(snapshot);
}

#[test]
fn does_not_extract_demand_control_directive_name_conflicts() {
    let subgraphs = Supergraph::new(r#"
        schema
          @link(url: "https://specs.apollo.dev/link/v1.0")
          @link(url: "https://specs.apollo.dev/join/v0.5", for: EXECUTION)
          @link(url: "https://example.com/myCustomDirective/v1.0", import: ["@cost"])
          @link(url: "https://example.com/myOtherCustomDirective/v1.0", import: ["@listSize"])
        {
          query: Query
        }
        
        directive @cost(name: String!) on FIELD_DEFINITION
        
        directive @join__directive(graphs: [join__Graph!], name: String!, args: join__DirectiveArguments) repeatable on SCHEMA | OBJECT | INTERFACE | FIELD_DEFINITION
        
        directive @join__enumValue(graph: join__Graph!) repeatable on ENUM_VALUE
        
        directive @join__field(graph: join__Graph, requires: join__FieldSet, provides: join__FieldSet, type: String, external: Boolean, override: String, usedOverridden: Boolean, overrideLabel: String, contextArguments: [join__ContextArgument!]) repeatable on FIELD_DEFINITION | INPUT_FIELD_DEFINITION
        
        directive @join__graph(name: String!, url: String!) on ENUM_VALUE
        
        directive @join__implements(graph: join__Graph!, interface: String!) repeatable on OBJECT | INTERFACE
        
        directive @join__type(graph: join__Graph!, key: join__FieldSet, extension: Boolean! = false, resolvable: Boolean! = true, isInterfaceObject: Boolean! = false) repeatable on OBJECT | INTERFACE | UNION | ENUM | INPUT_OBJECT | SCALAR
        
        directive @join__unionMember(graph: join__Graph!, member: String!) repeatable on UNION
        
        directive @link(url: String, as: String, for: link__Purpose, import: [link__Import]) repeatable on SCHEMA
        
        directive @listSize(name: String!) on FIELD_DEFINITION
        
        input join__ContextArgument {
          name: String!
          type: String!
          context: String!
          selection: join__FieldValue!
        }
        
        scalar join__DirectiveArguments
        
        scalar join__FieldSet
        
        scalar join__FieldValue
        
        enum join__Graph {
          SUBGRAPH_A @join__graph(name: "subgraph-a", url: "")
          SUBGRAPH_B @join__graph(name: "subgraph-b", url: "")
        }
        
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
        
        type Query
          @join__type(graph: SUBGRAPH_A)
          @join__type(graph: SUBGRAPH_B)
        {
          a: Int @join__field(graph: SUBGRAPH_A) @cost(name: "cost")
          b: [Int] @join__field(graph: SUBGRAPH_B) @listSize(name: "listSize")
        }
    "#)
    .expect("parses")
    .extract_subgraphs()
    .expect("extracts");

    let mut snapshot = String::new();
    for (_name, subgraph) in subgraphs {
        use std::fmt::Write;

        _ = writeln!(
            &mut snapshot,
            "{}\n---\n{}",
            subgraph.name,
            subgraph.schema.schema()
        );
    }
    insta::assert_snapshot!(snapshot);
}
