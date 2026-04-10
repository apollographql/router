use std::fs::read_to_string;

use insta::assert_debug_snapshot;
use insta::assert_snapshot;
use insta::glob;

use crate::ApiSchemaOptions;
use crate::connectors::expand::ExpansionResult;
use crate::connectors::expand::expand_connectors;

/// Verify that expansion fails when a connector returns a nested entity type
/// without including key fields in its selection.
#[test]
fn nested_entity_missing_key_fields_fails_expansion() {
    // Same structure as circular_reference.graphql, but friends selection
    // is "name" instead of "id name" — missing the key field "id".
    let supergraph = r#"
schema
  @link(url: "https://specs.apollo.dev/link/v1.0")
  @link(url: "https://specs.apollo.dev/join/v0.5", for: EXECUTION)
  @link(url: "https://specs.apollo.dev/connect/v0.1", for: EXECUTION)
  @join__directive(graphs: [CONNECTORS], name: "link", args: {url: "https://specs.apollo.dev/connect/v0.1", import: ["@connect", "@source"]})
  @join__directive(graphs: [CONNECTORS], name: "source", args: {name: "api", http: {baseURL: "http://localhost"}})
{
  query: Query
}

directive @join__directive(graphs: [join__Graph!], name: String!, args: join__DirectiveArguments) repeatable on SCHEMA | OBJECT | INTERFACE | FIELD_DEFINITION

directive @join__enumValue(graph: join__Graph!) repeatable on ENUM_VALUE

directive @join__field(graph: join__Graph, requires: join__FieldSet, provides: join__FieldSet, type: String, external: Boolean, override: String, usedOverridden: Boolean, overrideLabel: String, contextArguments: [join__ContextArgument!]) repeatable on FIELD_DEFINITION | INPUT_FIELD_DEFINITION

directive @join__graph(name: String!, url: String!) on ENUM_VALUE

directive @join__implements(graph: join__Graph!, interface: String!) repeatable on OBJECT | INTERFACE

directive @join__type(graph: join__Graph!, key: join__FieldSet, extension: Boolean! = false, resolvable: Boolean! = true, isInterfaceObject: Boolean! = false) repeatable on OBJECT | INTERFACE | UNION | ENUM | INPUT_OBJECT | SCALAR

directive @join__unionMember(graph: join__Graph!, member: String!) repeatable on UNION

directive @link(url: String, as: String, for: link__Purpose, import: [link__Import]) repeatable on SCHEMA

input join__ContextArgument {
  name: String!
  type: String!
  context: String!
  selection: join__FieldValue!
}

scalar join__DirectiveArguments
scalar join__FieldSet
scalar join__FieldValue
scalar link__Import

enum join__Graph {
  CONNECTORS @join__graph(name: "connectors", url: "none")
}

enum link__Purpose {
  SECURITY
  EXECUTION
}

type Query @join__type(graph: CONNECTORS) {
  user(id: ID!): User
    @join__field(graph: CONNECTORS)
    @join__directive(graphs: [CONNECTORS], name: "connect", args: {source: "api", http: {GET: "/users/{$args.id}"}, selection: "id name friends { name }"})
}

type User @join__type(graph: CONNECTORS, key: "id") {
  id: ID!
  name: String @join__field(graph: CONNECTORS)
  friends: [User]
    @join__field(graph: CONNECTORS)
    @join__directive(graphs: [CONNECTORS], name: "connect", args: {source: "api", http: {GET: "/users/{$this.id}/friends"}, selection: "name"})
}
    "#;

    let result = expand_connectors(supergraph, &Default::default());
    let err = match result {
        Err(e) => e,
        Ok(_) => panic!("expansion should fail when nested entity is missing key fields"),
    };
    let msg = err.to_string();
    assert!(
        msg.contains("missing key field"),
        "Expected error about missing key fields, got: {msg}"
    );
    assert!(
        msg.contains("friends"),
        "Error should mention the field path, got: {msg}"
    );
}

#[test]
fn it_expand_supergraph() {
    insta::with_settings!({prepend_module_to_snapshot => false}, {
        glob!("schemas/expand", "*.graphql", |path| {
            let to_expand = read_to_string(path).unwrap();
            let ExpansionResult::Expanded {
                raw_sdl,
                api_schema,
                connectors,
            } = expand_connectors(&to_expand, &ApiSchemaOptions { include_defer: true, ..Default::default() }).unwrap()
            else {
                panic!("expected expansion to actually expand subgraphs for {path:?}");
            };

            assert_snapshot!("api", api_schema);
            assert_debug_snapshot!("connectors", connectors.by_service_name);
            assert_snapshot!("supergraph", raw_sdl);
        });
    });
}

#[test]
fn it_ignores_supergraph() {
    insta::with_settings!({prepend_module_to_snapshot => false}, {
        glob!("schemas/ignore", "*.graphql", |path| {
            let to_ignore = read_to_string(path).unwrap();
            let ExpansionResult::Unchanged = expand_connectors(&to_ignore, &ApiSchemaOptions::default()).unwrap() else {
                panic!("expected expansion to ignore non-connector supergraph for {path:?}");
            };
        });
    });
}
