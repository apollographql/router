//! Authorization plugin

use std::ops::ControlFlow;

use http::StatusCode;
use schemars::JsonSchema;
use serde::Deserialize;
use tower::BoxError;
use tower::ServiceBuilder;
use tower::ServiceExt;

use crate::graphql;
use crate::layers::ServiceBuilderExt;
use crate::plugin::Plugin;
use crate::plugin::PluginInit;
use crate::plugins::authentication::APOLLO_AUTHENTICATION_JWT_CLAIMS;
use crate::register_plugin;
use crate::services::supergraph;

/// Authorization plugin
#[derive(Clone, Debug, Default, Deserialize, JsonSchema)]
struct Conf {
    /// Reject unauthenticated requests
    require_authentication: bool,
}

struct AuthorizationPlugin {
    enabled: bool,
}

#[async_trait::async_trait]
impl Plugin for AuthorizationPlugin {
    type Config = Conf;

    async fn new(init: PluginInit<Self::Config>) -> Result<Self, BoxError> {
        Ok(AuthorizationPlugin {
            enabled: init.config.require_authentication,
        })
    }

    fn supergraph_service(&self, service: supergraph::BoxService) -> supergraph::BoxService {
        if self.enabled {
            ServiceBuilder::new()
                .checkpoint(move |request: supergraph::Request| {
                    if request
                        .context
                        .contains_key(APOLLO_AUTHENTICATION_JWT_CLAIMS)
                    {
                        Ok(ControlFlow::Continue(request))
                    } else {
                        // This is a metric and will not appear in the logs
                        tracing::info!(
                            monotonic_counter.apollo_require_authentication_failure_count = 1u64,
                        );
                        tracing::error!("rejecting unauthenticated request");
                        let response = supergraph::Response::error_builder()
                            .error(
                                graphql::Error::builder()
                                    .message("unauthenticated".to_string())
                                    .extension_code("AUTH_ERROR")
                                    .build(),
                            )
                            .status_code(StatusCode::UNAUTHORIZED)
                            .context(request.context)
                            .build()?;
                        Ok(ControlFlow::Break(response))
                    }
                })
                .service(service)
                .boxed()
        } else {
            service
        }
    }
}

// This macro allows us to use it in our plugin registry!
// register_plugin takes a group name, and a plugin name.
//
// In order to keep the plugin names consistent,
// we use using the `Reverse domain name notation`
register_plugin!("apollo", "authorization", AuthorizationPlugin);

#[cfg(test)]
mod tests {
    use serde_json_bytes::json;
    use tower::ServiceExt;

    use crate::plugin::test::MockSubgraph;
    use crate::services::supergraph;
    use crate::Context;
    use crate::MockedSubgraphs;
    use crate::TestHarness;

    const SCHEMA: &str = r#"schema
        @core(feature: "https://specs.apollo.dev/core/v0.1")
        @core(feature: "https://specs.apollo.dev/join/v0.1")
        @core(feature: "https://specs.apollo.dev/inaccessible/v0.1")
         {
        query: Query
   }
   directive @core(feature: String!) repeatable on SCHEMA
   directive @join__field(graph: join__Graph, requires: join__FieldSet, provides: join__FieldSet) on FIELD_DEFINITION
   directive @join__type(graph: join__Graph!, key: join__FieldSet) repeatable on OBJECT | INTERFACE
   directive @join__owner(graph: join__Graph!) on OBJECT | INTERFACE
   directive @join__graph(name: String!, url: String!) on ENUM_VALUE
   directive @inaccessible on OBJECT | FIELD_DEFINITION | INTERFACE | UNION
   scalar join__FieldSet
   enum join__Graph {
       USER @join__graph(name: "user", url: "http://localhost:4001/graphql")
       ORGA @join__graph(name: "orga", url: "http://localhost:4002/graphql")
   }
   type Query {
       currentUser: User @join__field(graph: USER)
       orga(id: ID): Organization @join__field(graph: ORGA)
   }
   type User
   @join__owner(graph: USER)
   @join__type(graph: ORGA, key: "id")
   @join__type(graph: USER, key: "id"){
       id: ID!
       name: String
       phone: String
       activeOrganization: Organization
   }
   type Organization
   @join__owner(graph: ORGA)
   @join__type(graph: ORGA, key: "id")
   @join__type(graph: USER, key: "id") {
       id: ID
       creatorUser: User
       name: String
       nonNullId: ID!
       suborga: [Organization]
   }"#;

    #[tokio::test]
    async fn authenticated_request() {
        let subgraphs = MockedSubgraphs([
        ("user", MockSubgraph::builder().with_json(
                serde_json::json!{{
                    "query": "query($representations:[_Any!]!){_entities(representations:$representations){...on User{name phone}}}",
                    "variables": {
                        "representations": [
                            { "__typename": "User", "id":0 }
                        ],
                    }
                }},
                serde_json::json! {{
                    "data": {
                        "_entities":[
                            {
                                "name":"Ada",
                                "phone": "1234"
                            }
                        ]
                    }
                }},
            ).build()),
        ("orga", MockSubgraph::builder().with_json(
            serde_json::json!{{"query":"{orga(id:1){id creatorUser{__typename id}}}"}},
            serde_json::json!{{"data": {"orga": { "id": 1, "creatorUser": { "__typename": "User", "id": 0 } }}}}
        ).build())
    ].into_iter().collect());

        let service = TestHarness::builder()
            .configuration_json(serde_json::json!({
            "include_subgraph_errors": {
                "all": true
            },
            "authorization": {
                "require_authentication": true
            }}))
            .unwrap()
            .schema(SCHEMA)
            .extra_plugin(subgraphs)
            .build_supergraph()
            .await
            .unwrap();

        let context = Context::new();
        context
            .insert(
                "apollo_authentication::JWT::claims",
                "placeholder".to_string(),
            )
            .unwrap();
        let request = supergraph::Request::fake_builder()
            .query("query { orga(id: 1) { id creatorUser { id name phone } } }")
            .variables(
                json! {{ "isAuthenticated": true }}
                    .as_object()
                    .unwrap()
                    .clone(),
            )
            .context(context)
            .build()
            .unwrap();
        let response = service
            .oneshot(request)
            .await
            .unwrap()
            .next_response()
            .await
            .unwrap();

        insta::assert_json_snapshot!(response);
    }

    #[tokio::test]
    async fn unauthenticated_request() {
        let subgraphs = MockedSubgraphs([
        ("user", MockSubgraph::builder().with_json(
                serde_json::json!{{
                    "query": "query($representations:[_Any!]!){_entities(representations:$representations){...on User{name}}}",
                    "variables": {
                        "representations": [
                            { "__typename": "User", "id":0 }
                        ],
                    }
                }},
                serde_json::json! {{
                    "data": {
                        "_entities":[
                            {
                                "name":"Ada"
                            }
                        ]
                    }
                }},
            ).build()),
        ("orga", MockSubgraph::builder().with_json(
            serde_json::json!{{"query":"{orga(id:1){id creatorUser{__typename id}}}"}},
            serde_json::json!{{"data": {"orga": { "id": 1, "creatorUser": { "__typename": "User", "id": 0 } }}}}
        ).build())
    ].into_iter().collect());

        let service = TestHarness::builder()
            .configuration_json(serde_json::json!({
            "include_subgraph_errors": {
                "all": true
            },
            "authorization": {
                "require_authentication": true
            }}))
            .unwrap()
            .schema(SCHEMA)
            .extra_plugin(subgraphs)
            .build_supergraph()
            .await
            .unwrap();

        let context = Context::new();
        let request = supergraph::Request::fake_builder()
            .query("query { orga(id: 1) { id creatorUser { id name phone } } }")
            .variables(
                json! {{ "isAuthenticated": false }}
                    .as_object()
                    .unwrap()
                    .clone(),
            )
            .context(context)
            // Request building here
            .build()
            .unwrap();
        let response = service
            .oneshot(request)
            .await
            .unwrap()
            .next_response()
            .await
            .unwrap();

        insta::assert_json_snapshot!(response);
    }
}
