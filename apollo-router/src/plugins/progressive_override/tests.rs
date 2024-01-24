use tower::ServiceExt;

use crate::plugins::progressive_override::LABELS_TO_OVERRIDE;
use crate::services::supergraph;
use crate::Context;
use crate::TestHarness;

const SCHEMA: &str = r#"
  schema
    @link(url: "https://specs.apollo.dev/link/v1.0")
    @link(url: "https://specs.apollo.dev/join/v0.4", for: EXECUTION)
  {
    query: Query
  }

  directive @join__enumValue(graph: join__Graph!) repeatable on ENUM_VALUE

  directive @join__field(graph: join__Graph, requires: join__FieldSet, provides: join__FieldSet, type: String, external: Boolean, override: String, usedOverridden: Boolean, overrideLabel: String) repeatable on FIELD_DEFINITION | INPUT_FIELD_DEFINITION

  directive @join__graph(name: String!, url: String!) on ENUM_VALUE

  directive @join__implements(graph: join__Graph!, interface: String!) repeatable on OBJECT | INTERFACE

  directive @join__type(graph: join__Graph!, key: join__FieldSet, extension: Boolean! = false, resolvable: Boolean! = true, isInterfaceObject: Boolean! = false) repeatable on OBJECT | INTERFACE | UNION | ENUM | INPUT_OBJECT | SCALAR

  directive @join__unionMember(graph: join__Graph!, member: String!) repeatable on UNION

  directive @link(url: String, as: String, for: link__Purpose, import: [link__Import]) repeatable on SCHEMA

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

  type T
    @join__type(graph: SUBGRAPH1, key: "k")
    @join__type(graph: SUBGRAPH2, key: "k")
  {
    k: ID
    a: Int @join__field(graph: SUBGRAPH1, override: "Subgraph2", overrideLabel: "foo") @join__field(graph: SUBGRAPH2, overrideLabel: "foo")
    b: Int @join__field(graph: SUBGRAPH2)
  }
"#;

// TODO: unit tests around specifically router_service and supergraph_service lifecycle

async fn get_supergraph_service() -> supergraph::BoxCloneService {
    TestHarness::builder()
        .configuration_json(serde_json::json! {{
            "plugins": {
                "experimental.expose_query_plan": true
            }
        }})
        .unwrap()
        .schema(SCHEMA)
        .build_supergraph()
        .await
        .unwrap()
}

#[tokio::test]
async fn todo() {
    let request = supergraph::Request::fake_builder()
        .query("{ t { a } }")
        .header("Apollo-Expose-Query-Plan", "true")
        .build()
        .unwrap();

    let response = get_supergraph_service()
        .await
        .oneshot(request)
        .await
        .unwrap()
        .next_response()
        .await
        .unwrap();

    insta::assert_json_snapshot!(serde_json::to_value(response).unwrap());

    let context = Context::new();
    context
        .insert(LABELS_TO_OVERRIDE, vec!["foo".to_string()])
        .unwrap();

    let overridden_request = supergraph::Request::fake_builder()
        .query("{ t { a } }")
        .header("Apollo-Expose-Query-Plan", "true")
        .context(context)
        .build()
        .unwrap();

    let overridden_response = get_supergraph_service()
        .await
        .oneshot(overridden_request)
        .await
        .unwrap()
        .next_response()
        .await
        .unwrap();

    insta::assert_json_snapshot!(serde_json::to_value(overridden_response).unwrap());
}
