//! Custom `->` methods (`methods:`, `@method`) run through what router startup
//! does: compose (which validates, expands the connectors, and checks
//! satisfiability), then expand the connectors in the supergraph and build a
//! query planner over the result. Tests that only call `validate()` never
//! reach expansion, which is where a method-unaware shape used to leave a type
//! with no fields.

use apollo_federation::ApiSchemaOptions;
use apollo_federation::Supergraph;
use apollo_federation::connectors::expand::ExpansionResult;
use apollo_federation::connectors::expand::expand_connectors;
use apollo_federation::query_plan::query_planner::QueryPlanner;
use apollo_federation::query_plan::query_planner::QueryPlannerConfig;

use super::ServiceDefinition;
use super::compose_as_fed2_connectors_subgraphs;

/// Compose a single connectors subgraph, expand its connectors, and build a
/// query planner. Returns the expanded supergraph SDL.
fn compose_ok(type_defs: &str) -> String {
    let result = compose_as_fed2_connectors_subgraphs(&[ServiceDefinition {
        name: "methods",
        type_defs,
    }]);
    let supergraph = match result {
        Ok(supergraph) => supergraph.schema().schema().to_string(),
        Err(failure) => panic!("expected composition to succeed, got: {failure:?}"),
    };
    let ExpansionResult::Expanded { raw_sdl, .. } =
        expand_connectors(&supergraph, &ApiSchemaOptions::default())
            .expect("connectors should expand")
    else {
        panic!("expected the supergraph to contain connectors");
    };
    let expanded =
        Supergraph::new_with_router_specs(&raw_sdl).expect("expanded supergraph should be valid");
    QueryPlanner::new(&expanded, QueryPlannerConfig::default())
        .expect("query planner should build over the expanded supergraph");
    raw_sdl
}

#[test]
fn source_method_call_expands_the_type_fields() {
    let supergraph = compose_ok(
        r#"
        extend schema
          @link(url: "https://specs.apollo.dev/connect/v0.5", import: ["@connect", "@source"])
          @source(
            name: "api"
            http: { baseURL: "http://localhost" }
            methods: { User: "id name email" }
          )

        type User {
          id: ID!
          name: String
          email: String
        }

        type Query {
          user: User @connect(source: "api", http: { GET: "/user" }, selection: "$->User")
        }
        "#,
    );
    assert!(supergraph.contains("email: String"), "{supergraph}");
}

#[test]
fn method_directive_on_type_composes() {
    let supergraph = compose_ok(
        r#"
        extend schema
          @link(url: "https://specs.apollo.dev/connect/v0.5", import: ["@connect", "@source", "@method"])
          @source(name: "api", http: { baseURL: "http://localhost" })

        type User @method {
          id: ID!
          name: String
          email: String
        }

        type Query {
          user: User @connect(source: "api", http: { GET: "/user" }, selection: "$->User")
        }
        "#,
    );
    assert!(supergraph.contains("email: String"), "{supergraph}");
}

#[test]
fn method_directive_on_type_composes_when_unused() {
    compose_ok(
        r#"
        extend schema
          @link(url: "https://specs.apollo.dev/connect/v0.5", import: ["@connect", "@source", "@method"])
          @source(name: "api", http: { baseURL: "http://localhost" })

        type User @method {
          id: ID!
          name: String
          email: String
        }

        type Query {
          user: User @connect(source: "api", http: { GET: "/user" }, selection: "id name email")
        }
        "#,
    );
}
