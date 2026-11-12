use apollo_compiler::schema::ExtendedType;
use apollo_federation::subgraph::Subgraph;
use apollo_federation::subgraph::typestate;

#[test]
fn can_parse_and_expand() -> Result<(), String> {
    let schema = r#"
        extend schema
          @link(url: "https://specs.apollo.dev/federation/v2.3", import: [ "@key" ])

        type Query {
            t: T
        }

        type T @key(fields: "id") {
            id: ID!
            x: Int
        }
        "#;

    let subgraph = Subgraph::parse_and_expand("S1", "http://s1", schema).map_err(|e| {
        println!("{e}");
        String::from("failed to parse and expand the subgraph, see errors above for details")
    })?;
    assert!(subgraph.schema.types.contains_key("T"));
    assert!(subgraph.schema.directive_definitions.contains_key("key"));
    assert!(
        subgraph
            .schema
            .directive_definitions
            .contains_key("federation__requires")
    );
    Ok(())
}

#[test]
fn can_parse_and_expand_with_renames() -> Result<(), String> {
    let schema = r#"
        extend schema
          @link(url: "https://specs.apollo.dev/federation/v2.3", import: [ { name: "@key", as: "@myKey" }, "@provides" ])

        type Query {
            t: T @provides(fields: "x")
        }

        type T @myKey(fields: "id") {
            id: ID!
            x: Int
        }
        "#;

    let subgraph = Subgraph::parse_and_expand("S1", "http://s1", schema).map_err(|e| {
        println!("{e}");
        String::from("failed to parse and expand the subgraph, see errors above for details")
    })?;
    assert!(subgraph.schema.directive_definitions.contains_key("myKey"));
    assert!(
        subgraph
            .schema
            .directive_definitions
            .contains_key("provides")
    );
    Ok(())
}

#[test]
fn can_parse_and_expand_with_namespace() -> Result<(), String> {
    let schema = r#"
        extend schema
          @link(url: "https://specs.apollo.dev/federation/v2.3", import: [ "@key" ], as: "fed" )

        type Query {
            t: T
        }

        type T @key(fields: "id") {
            id: ID!
            x: Int
        }
        "#;

    let subgraph = Subgraph::parse_and_expand("S1", "http://s1", schema).map_err(|e| {
        println!("{e}");
        String::from("failed to parse and expand the subgraph, see errors above for details")
    })?;
    assert!(subgraph.schema.directive_definitions.contains_key("key"));
    assert!(
        subgraph
            .schema
            .directive_definitions
            .contains_key("fed__requires")
    );
    Ok(())
}

#[test]
fn can_parse_and_expand_preserves_user_definitions() -> Result<(), String> {
    let schema = r#"
        extend schema
          @link(url: "https://specs.apollo.dev/link/v1.0", import: ["Import", "Purpose"])
          @link(url: "https://specs.apollo.dev/federation/v2.3", import: [ "@key" ])

        type Query {
            t: T
        }

        type T @key(fields: "id") {
            id: ID!
            x: Int
        }

        enum Purpose {
            SECURITY
            EXECUTION
        }

        scalar Import

        directive @link(url: String, as: String, import: [Import], for: Purpose) repeatable on SCHEMA
        "#;

    let subgraph = Subgraph::parse_and_expand("S1", "http://s1", schema).map_err(|e| {
        println!("{e}");
        String::from("failed to parse and expand the subgraph, see errors above for details")
    })?;
    assert!(subgraph.schema.types.contains_key("Purpose"));
    Ok(())
}

#[test]
fn can_parse_and_expand_works_with_fed_v1() -> Result<(), String> {
    let schema = r#"
        type Query {
            t: T
        }

        type T @key(fields: "id") {
            id: ID!
            x: Int
        }
        "#;

    let subgraph = Subgraph::parse_and_expand("S1", "http://s1", schema).map_err(|e| {
        println!("{e}");
        String::from("failed to parse and expand the subgraph, see errors above for details")
    })?;
    assert!(subgraph.schema.types.contains_key("T"));
    assert!(subgraph.schema.directive_definitions.contains_key("key"));
    Ok(())
}

#[test]
fn can_parse_and_expand_will_fail_when_importing_same_spec_twice() {
    let schema = r#"
        extend schema
          @link(url: "https://specs.apollo.dev/federation/v2.3", import: [ "@key" ] )
          @link(url: "https://specs.apollo.dev/federation/v2.3", import: [ "@provides" ] )

        type Query {
            t: T
        }

        type T @key(fields: "id") {
            id: ID!
            x: Int
        }
        "#;

    let result = Subgraph::parse_and_expand("S1", "http://s1", schema)
        .expect_err("importing same specification twice should fail");
    assert_eq!(
        "Invalid use of @link in schema: invalid graphql schema - multiple @link imports for the federation specification are not supported",
        result.to_string()
    );
}

#[test]
fn can_parse_and_expand_with_keyed_interface() {
    // Since federation v2.3, `@key` may be applied to interfaces. Interfaces cannot be union
    // members, so `_Entity` must contain only the keyed object types.
    let schema = r#"
        extend schema
          @link(url: "https://specs.apollo.dev/federation/v2.5", import: [ "@key" ])

        type Query {
            i: I
        }

        interface I @key(fields: "id") {
            id: ID!
        }

        type P implements I @key(fields: "id") {
            id: ID!
        }
        "#;

    let subgraph = Subgraph::parse_and_expand("S1", "http://s1", schema)
        .unwrap_or_else(|e| panic!("keyed interface subgraph should expand:\n{e}"));
    let entity_members = |schema: &apollo_compiler::Schema| {
        let Some(ExtendedType::Union(entity)) = schema.types.get("_Entity") else {
            panic!("expected `_Entity` to be a union type");
        };
        entity
            .members
            .iter()
            .map(|member| member.name.to_string())
            .collect::<Vec<_>>()
    };
    assert_eq!(entity_members(&subgraph.schema), ["P"]);
    assert!(subgraph.schema.type_field("Query", "_entities").is_ok());

    // The federation subgraph pipeline (which mirrors the JS implementation) produces the same
    // `_Entity` union for this schema.
    let expanded = typestate::Subgraph::parse("S1", "http://s1", schema)
        .expect("subgraph parses")
        .expand_links()
        .expect("subgraph expands")
        .validate()
        .expect("subgraph is valid");
    assert_eq!(
        entity_members(expanded.validated_schema().schema()),
        entity_members(&subgraph.schema)
    );
}
