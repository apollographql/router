use super::*;
use crate::Context;
use futures::StreamExt;
use serde_json::json;
use serde_json_bytes::json as bjson;
use std::sync::Arc;

#[tokio::test]
async fn test_core_json_request_to_subgraph_request_conversion() {
    // Create a router core JSON request
    let mut extensions = apollo_router_core::Extensions::new();

    // Create HTTP parts by deconstructing a real request
    let temp_request = http::Request::builder()
        .method("POST")
        .uri("/graphql")
        .body(())
        .expect("building temp request should not fail");
    let (http_parts, _) = temp_request.into_parts();

    // Create default supergraph request
    let supergraph_request = Arc::new(
        http::Request::builder()
            .method("POST")
            .uri("/graphql")
            .body(graphql::Request::default())
            .expect("building default supergraph request should not fail")
    );

    let context = Context::new();
    context
        .insert("test_key", "test_value".to_string())
        .unwrap();

    // Create and store subgraph metadata
    let metadata = RequestMetadata {
        http_parts,
        supergraph_request,
        operation_kind: OperationKind::Query,
        context: context.clone(),
        subgraph_name: "test-subgraph".to_string(),
        subscription_stream: None,
        connection_closed_signal: None,
        query_hash: Arc::new(QueryHash::default()),
        authorization: Arc::new(CacheKeyMetadata::default()),
        executable_document: None,
        id: SubgraphRequestId::new(),
    };
    extensions.insert(Arc::new(metadata));

    let json_body = json!({
        "query": "{ hello }",
        "variables": {},
        "operationName": "TestOperation"
    });

    let core_request = CoreJsonRequest {
        extensions,
        body: json_body,
    };

    // Convert to subgraph request using async function
    let subgraph_request = core_json_request_to_subgraph_request(core_request).await.unwrap();

    // Verify the conversion preserved data
    assert_eq!(subgraph_request.operation_kind, OperationKind::Query);
    assert_eq!(subgraph_request.subgraph_name, "test-subgraph");

    // Verify context is preserved
    let context_value = subgraph_request
        .context
        .get::<_, String>("test_key")
        .unwrap()
        .unwrap();
    assert_eq!(context_value, "test_value");

    // Verify the GraphQL request was deserialized correctly
    assert_eq!(
        subgraph_request
            .subgraph_request
            .body()
            .query
            .as_ref()
            .unwrap(),
        "{ hello }"
    );
    assert_eq!(
        subgraph_request
            .subgraph_request
            .body()
            .operation_name
            .as_ref()
            .unwrap(),
        "TestOperation"
    );
}

#[tokio::test]
async fn test_subgraph_request_to_core_json_request_conversion() {
    // Create a router subgraph request
    let graphql_request = graphql::Request::builder()
        .query("{ user(id: \"123\") { name } }")
        .operation_name("GetUser")
        .build();

    let subgraph_request_http = http::Request::builder()
        .method("POST")
        .uri("/graphql")
        .body(graphql_request.clone())
        .unwrap();

    let supergraph_request = Arc::new(
        http::Request::builder()
            .method("POST")
            .uri("/graphql")
            .body(graphql_request)
            .unwrap(),
    );

    let context = Context::new();
    context.insert("user_id", "123".to_string()).unwrap();

    let subgraph_request = SubgraphRequest {
        supergraph_request,
        subgraph_request: subgraph_request_http,
        operation_kind: OperationKind::Query,
        context,
        subgraph_name: "users-service".to_string(),
        subscription_stream: None,
        connection_closed_signal: None,
        query_hash: Arc::new(QueryHash::default()),
        authorization: Arc::new(CacheKeyMetadata::default()),
        executable_document: None,
        id: SubgraphRequestId::new(),
    };

    // Convert to core JSON request using async function
    let core_request = subgraph_request_to_core_json_request(subgraph_request).await.unwrap();

    // Verify the JSON body contains the GraphQL request
    let query = core_request
        .body
        .get("query")
        .and_then(|v| v.as_str())
        .unwrap();
    assert_eq!(query, "{ user(id: \"123\") { name } }");

    let operation_name = core_request
        .body
        .get("operationName")
        .and_then(|v| v.as_str())
        .unwrap();
    assert_eq!(operation_name, "GetUser");

    // Verify subgraph metadata is preserved in extensions
    let metadata = core_request
        .extensions
        .get::<Arc<RequestMetadata>>()
        .unwrap();
    assert_eq!(metadata.operation_kind, OperationKind::Query);
    assert_eq!(metadata.subgraph_name, "users-service");
}

#[tokio::test]
async fn test_subgraph_response_to_core_json_response_conversion() {
    // Create a router subgraph response
    let graphql_response = graphql::Response::builder()
        .data(json!({ "user": { "name": "Alice" } }))
        .build();

    let http_response = http::Response::builder()
        .status(200)
        .header("content-type", "application/json")
        .body(graphql_response)
        .unwrap();

    let context = Context::new();
    context.insert("request_id", "req-123".to_string()).unwrap();

    let subgraph_response = SubgraphResponse {
        response: http_response,
        context,
        subgraph_name: "users-service".to_string(),
        id: SubgraphRequestId::new(),
    };

    // Convert to core JSON response using async function
    let core_response = subgraph_response_to_core_json_response(subgraph_response).await.unwrap();

    // Verify subgraph response metadata is preserved in extensions
    let metadata = core_response
        .extensions
        .get::<Arc<ResponseMetadata>>()
        .unwrap();
    assert_eq!(metadata.subgraph_name, "users-service");

    // Verify context is preserved
    let request_id = metadata.context.get::<_, String>("request_id").unwrap().unwrap();
    assert_eq!(request_id, "req-123");

    // Verify the response stream contains the serialized GraphQL response
    let mut stream = core_response.responses;
    let first_response = stream.next().await.unwrap().unwrap();

    let data = first_response.get("data").unwrap();
    let user_name = data
        .get("user")
        .unwrap()
        .get("name")
        .unwrap()
        .as_str()
        .unwrap();
    assert_eq!(user_name, "Alice");
}

#[tokio::test]
async fn test_round_trip_request_conversion() {
    // Create original core JSON request with subgraph metadata
    let mut extensions = apollo_router_core::Extensions::new();

    // Create HTTP parts by deconstructing a real request
    let temp_request = http::Request::builder()
        .method("POST")
        .uri("/graphql")
        .body(())
        .expect("building temp request should not fail");
    let (http_parts, _) = temp_request.into_parts();

    // Create default supergraph request
    let supergraph_request = Arc::new(
        http::Request::builder()
            .method("POST")
            .uri("/graphql")
            .body(graphql::Request::default())
            .expect("building default supergraph request should not fail")
    );

    let context = Context::new();
    context.insert("trace_id", "trace-xyz".to_string()).unwrap();
    context
        .insert("user_roles", vec!["admin".to_string(), "user".to_string()])
        .unwrap();

    let metadata = RequestMetadata {
        http_parts,
        supergraph_request,
        operation_kind: OperationKind::Mutation,
        context: context.clone(),
        subgraph_name: "payment-service".to_string(),
        subscription_stream: None,
        connection_closed_signal: None,
        query_hash: Arc::new(QueryHash::default()),
        authorization: Arc::new(CacheKeyMetadata::default()),
        executable_document: None,
        id: SubgraphRequestId::new(),
    };
    extensions.insert(Arc::new(metadata));

    let json_body = json!({
        "query": "mutation CreatePayment($amount: Float!) { createPayment(amount: $amount) { id } }",
        "variables": { "amount": 99.99 },
        "operationName": "CreatePayment"
    });

    let original_request = CoreJsonRequest {
        extensions,
        body: json_body,
    };

    // Store reference values for comparison
    let original_query = original_request
        .body
        .get("query")
        .unwrap()
        .as_str()
        .unwrap()
        .to_string();
    let original_amount = original_request
        .body
        .get("variables")
        .unwrap()
        .get("amount")
        .unwrap()
        .as_f64()
        .unwrap();

    // Round trip: Core -> Subgraph -> Core
    let subgraph_request = core_json_request_to_subgraph_request(original_request).await.unwrap();
    let final_request = subgraph_request_to_core_json_request(subgraph_request).await.unwrap();

    // Verify round trip preserved all properties
    let final_query = final_request.body.get("query").unwrap().as_str().unwrap();
    assert_eq!(final_query, original_query);

    let final_amount = final_request
        .body
        .get("variables")
        .unwrap()
        .get("amount")
        .unwrap()
        .as_f64()
        .unwrap();
    assert_eq!(final_amount, original_amount);

    let operation_name = final_request
        .body
        .get("operationName")
        .unwrap()
        .as_str()
        .unwrap();
    assert_eq!(operation_name, "CreatePayment");

    // Verify subgraph metadata is preserved
    let metadata = final_request
        .extensions
        .get::<Arc<RequestMetadata>>()
        .unwrap();
    assert_eq!(metadata.operation_kind, OperationKind::Mutation);
    assert_eq!(metadata.subgraph_name, "payment-service");

    // Verify context is preserved in the metadata
    let trace_id = metadata.context.get::<_, String>("trace_id").unwrap().unwrap();
    assert_eq!(trace_id, "trace-xyz");

    let user_roles = metadata.context
        .get::<_, Vec<String>>("user_roles")
        .unwrap()
        .unwrap();
    assert_eq!(user_roles, vec!["admin".to_string(), "user".to_string()]);
}

#[tokio::test]
async fn test_round_trip_response_conversion() {
    // Create original subgraph response with complex data
    let graphql_response = graphql::Response::builder()
        .data(json!({
            "user": {
                "id": "user-123",
                "name": "Bob",
                "posts": [
                    { "id": "post-1", "title": "Hello World" },
                    { "id": "post-2", "title": "GraphQL is Great" }
                ]
            }
        }))
        .errors(vec![])
        .extensions(bjson!({ "cost": 15 }).as_object().unwrap().clone())
        .build();

    let http_response = http::Response::builder()
        .status(200)
        .header("content-type", "application/json")
        .header("x-custom-header", "custom-value")
        .body(graphql_response)
        .unwrap();

    let context = Context::new();
    context.insert("execution_time", 250u64).unwrap();
    context.insert("cache_hit", true).unwrap();

    let original_response = SubgraphResponse {
        response: http_response,
        context,
        subgraph_name: "posts-service".to_string(),
        id: SubgraphRequestId::new(),
    };

    // Store reference values for comparison
    let _original_user_name = original_response
        .response
        .body()
        .data
        .as_ref()
        .unwrap()
        .get("user")
        .unwrap()
        .get("name")
        .unwrap()
        .as_str()
        .unwrap()
        .to_string();
    let original_execution_time = original_response
        .context
        .get::<_, u64>("execution_time")
        .unwrap()
        .unwrap();

    // Round trip: Subgraph -> Core -> Subgraph
    let core_response = subgraph_response_to_core_json_response(original_response).await.unwrap();
    let final_response = core_json_response_to_subgraph_response(core_response).await.unwrap();

    // Verify HTTP properties are preserved (though some defaults are applied)
    assert_eq!(final_response.response.status(), 200);

    // Verify context data is preserved
    let execution_time = final_response
        .context
        .get::<_, u64>("execution_time")
        .unwrap()
        .unwrap();
    assert_eq!(execution_time, original_execution_time);

    let cache_hit = final_response
        .context
        .get::<_, bool>("cache_hit")
        .unwrap()
        .unwrap();
    assert_eq!(cache_hit, true);

    // Verify subgraph name is preserved
    assert_eq!(final_response.subgraph_name, "posts-service");

    // Note: This test demonstrates that the round-trip conversion works for the data
    // that can be preserved, though some HTTP-specific details may be lost due to
    // the different nature of the streaming JSON response format in router core.
}


