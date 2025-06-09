use serde_json::json;
use serde_json::value::Value;
use tower::BoxError;
use wiremock::Mock;
use wiremock::ResponseTemplate;
use wiremock::matchers::method;
use wiremock::matchers::path;

use crate::integration::IntegrationTest;
use crate::integration::common::Query;
use crate::integration::common::graph_os_enabled;

const CONFIG: &str = r#"
include_subgraph_errors:
  all: true
"#;

enum ResponseType {
    Ok,
    Error(ErrorType),
}

enum ErrorType {
    Malformed,
    EmptyPath,
    Valid,
}

fn query() -> Query {
    let query_str =
        r#"query Q { topProducts { name inStock reviews { id author { username name } } } }"#;

    Query::builder()
        .traced(true)
        .body(json!({"query": query_str}))
        .build()
}

fn products_response(response_type: ResponseType) -> Value {
    match response_type {
        ResponseType::Ok => json!({
            "data": {
                "topProducts": [
                    { "__typename": "Product", "name": "Table", "upc": "1" },
                    { "__typename": "Product", "name": "Chair", "upc": "2" },
                ]
            },
        }),
        ResponseType::Error(ErrorType::Valid) => json!({
            "data": {
                "topProducts": [
                    { "__typename": "Product", "name": "Table", "upc": "1" },
                    null,
                ]
            },
            "errors": [{ "message": "products error", "path": ["topProducts", 1] }]
        }),
        ResponseType::Error(ErrorType::EmptyPath) => json!({
            "data": {
                "topProducts": [
                    { "__typename": "Product", "name": "Table", "upc": "1" },
                    null,
                ]
            },
            "errors": [{ "message": "products error", "path": [] }]
        }),
        ResponseType::Error(ErrorType::Malformed) => json!({"malformed": true}),
    }
}

fn inventory_response(response_type: ResponseType) -> Value {
    match response_type {
        ResponseType::Ok => json!({
            "data": {"_entities": [{"inStock": true}, {"inStock": false}]},
        }),
        ResponseType::Error(ErrorType::Valid) => json!({
            "data": {"_entities": [null, {"inStock": false}]},
            "errors": [{ "message": "inventory error", "path": ["_entities", 0] }]
        }),
        ResponseType::Error(ErrorType::EmptyPath) => json!({
            "data": {"_entities": [null, {"inStock": false}]},
            "errors": [{ "message": "inventory error", "path": [] }]
        }),
        ResponseType::Error(ErrorType::Malformed) => json!({"malformed": true}),
    }
}

fn reviews_response(response_type: ResponseType) -> Value {
    match response_type {
        ResponseType::Ok => json!({
            "data": {
                "_entities": [
                    {"reviews": [{"id": "1", "author": {"__typename": "User", "username": "@ada", "id": "1"}}, {"id": "1", "author": {"__typename": "User", "username": "@alan", "id": "2"}}]},
                    {"reviews": [{"id": "3", "author": {"__typename": "User", "username": "@alan", "id": "2"}}]},
                ]
            }
        }),
        ResponseType::Error(ErrorType::Valid) => json!({
            "data": {
                "_entities": [
                    {"reviews": [{"id": "1", "author": {"__typename": "User", "username": "@ada", "id": "1"}}, {"id": "1", "author": {"__typename": "User", "username": "@alan", "id": "2"}}]},
                    null,
                ]
            },
            "errors": [{ "message": "inventory error", "path": ["_entities", 1] }]
        }),
        ResponseType::Error(ErrorType::EmptyPath) => json!({
            "data": {
                "_entities": [
                    {"reviews": [{"id": "1", "author": {"__typename": "User", "username": "@ada", "id": "1"}}, {"id": "1", "author": {"__typename": "User", "username": "@alan", "id": "2"}}]},
                    null,
                ]
            },
            "errors": [{ "message": "inventory error", "path": [] }]
        }),
        ResponseType::Error(ErrorType::Malformed) => json!({"malformed": true}),
    }
}

fn accounts_response(response_type: ResponseType) -> Value {
    match response_type {
        ResponseType::Ok => json!({
            "data": {"_entities": [{"name": "Ada"}, {"name": "Alan"}]}
        }),
        ResponseType::Error(ErrorType::Valid) => json!({
            "data": {"_entities": [{"name": "Ada"}, null]},
            "errors": [{ "message": "inventory error", "path": ["_entities", 1] }]
        }),
        ResponseType::Error(ErrorType::EmptyPath) => json!({
            "data": {"_entities": [{"name": "Ada"}, null]},
            "errors": [{ "message": "inventory error", "path": [] }]
        }),
        ResponseType::Error(ErrorType::Malformed) => json!({"malformed": true}),
    }
}

fn response_template(response_json: Value) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_json(response_json)
}

fn assert_no_at_in_path(errors: &Value) {
    for error in errors.as_array().unwrap() {
        let error = error.as_object().unwrap();
        let path = error.get("path").unwrap().as_array().unwrap();
        assert!(
            !path.contains(&Value::String("@".into())),
            "`@` in path! path = {path:?}, message = {:?}",
            error.get("message").unwrap()
        );
    }
}

async fn send_query_to_router(
    query: Query,
    subgraph_response_products: ResponseTemplate,
    subgraph_response_inventory: ResponseTemplate,
    subgraph_response_reviews: ResponseTemplate,
    subgraph_response_accounts: ResponseTemplate,
) -> Result<Value, BoxError> {
    let mock_products = wiremock::MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/"))
        .respond_with(subgraph_response_products)
        .mount(&mock_products)
        .await;

    let mock_inventory = wiremock::MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/"))
        .respond_with(subgraph_response_inventory)
        .mount(&mock_inventory)
        .await;

    let mock_reviews = wiremock::MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/"))
        .respond_with(subgraph_response_reviews)
        .mount(&mock_reviews)
        .await;

    let mock_accounts = wiremock::MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/"))
        .respond_with(subgraph_response_accounts)
        .mount(&mock_accounts)
        .await;

    let mut router = IntegrationTest::builder()
        .config(CONFIG)
        .subgraph_override("products", mock_products.uri())
        .subgraph_override("inventory", mock_inventory.uri())
        .subgraph_override("reviews", mock_reviews.uri())
        .subgraph_override("accounts", mock_accounts.uri())
        .build()
        .await;
    router.start().await;
    router.assert_started().await;

    let (_, response) = router.execute_query(query).await;
    assert_eq!(response.status(), 200);
    let parsed_response = serde_json::from_str(&response.text().await?)?;
    Ok(parsed_response)
}

#[tokio::test(flavor = "multi_thread")]
async fn test_all_successful() -> Result<(), BoxError> {
    if !graph_os_enabled() {
        return Ok(());
    }

    let response = send_query_to_router(
        query(),
        response_template(products_response(ResponseType::Ok)),
        response_template(inventory_response(ResponseType::Ok)),
        response_template(reviews_response(ResponseType::Ok)),
        response_template(accounts_response(ResponseType::Ok)),
    )
    .await?;
    assert!(response.get("errors").is_none());

    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn test_top_level_response_failure() -> Result<(), BoxError> {
    if !graph_os_enabled() {
        return Ok(());
    }

    let response = send_query_to_router(
        query(),
        response_template(products_response(ResponseType::Error(ErrorType::Valid))),
        response_template(inventory_response(ResponseType::Ok)),
        response_template(reviews_response(ResponseType::Ok)),
        response_template(accounts_response(ResponseType::Ok)),
    )
    .await?;
    let errors = response.get("errors").expect("errors should be present");
    assert_no_at_in_path(errors);

    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn test_top_level_response_failure_malformed() -> Result<(), BoxError> {
    if !graph_os_enabled() {
        return Ok(());
    }

    let response = send_query_to_router(
        query(),
        response_template(products_response(ResponseType::Error(ErrorType::Malformed))),
        response_template(inventory_response(ResponseType::Ok)),
        response_template(reviews_response(ResponseType::Ok)),
        response_template(accounts_response(ResponseType::Ok)),
    )
    .await?;
    let errors = response.get("errors").expect("errors should be present");
    assert_no_at_in_path(errors); // PASSES, but path = []

    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn test_second_level_response_failure() -> Result<(), BoxError> {
    if !graph_os_enabled() {
        return Ok(());
    }

    let response = send_query_to_router(
        query(),
        response_template(products_response(ResponseType::Ok)),
        response_template(inventory_response(ResponseType::Error(ErrorType::Valid))),
        response_template(reviews_response(ResponseType::Ok)),
        response_template(accounts_response(ResponseType::Ok)),
    )
    .await?;
    let errors = response.get("errors").expect("errors should be present");
    eprintln!("{response:?}");
    assert_no_at_in_path(errors);

    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn test_second_level_response_failure_malformed() -> Result<(), BoxError> {
    if !graph_os_enabled() {
        return Ok(());
    }

    let response = send_query_to_router(
        query(),
        response_template(products_response(ResponseType::Ok)),
        response_template(inventory_response(ResponseType::Error(
            ErrorType::Malformed,
        ))),
        response_template(reviews_response(ResponseType::Ok)),
        response_template(accounts_response(ResponseType::Ok)),
    )
    .await?;
    let errors = response.get("errors").expect("errors should be present");
    eprintln!("{response:?}");
    assert_no_at_in_path(errors); // FAILS: [String("topProducts"), String("@")]

    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn test_second_level_response_failure_empty_path() -> Result<(), BoxError> {
    if !graph_os_enabled() {
        return Ok(());
    }

    let response = send_query_to_router(
        query(),
        response_template(products_response(ResponseType::Ok)),
        response_template(inventory_response(ResponseType::Error(
            ErrorType::EmptyPath,
        ))),
        response_template(reviews_response(ResponseType::Ok)),
        response_template(accounts_response(ResponseType::Ok)),
    )
    .await?;
    let errors = response.get("errors").expect("errors should be present");
    eprintln!("{response:?}");
    assert_no_at_in_path(errors); // FAILS: [String("topProducts"), String("@")]

    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn test_nested_response_failure() -> Result<(), BoxError> {
    if !graph_os_enabled() {
        return Ok(());
    }

    let response = send_query_to_router(
        query(),
        response_template(products_response(ResponseType::Ok)),
        response_template(inventory_response(ResponseType::Ok)),
        response_template(reviews_response(ResponseType::Ok)),
        response_template(accounts_response(ResponseType::Error(ErrorType::Valid))),
    )
    .await?;
    let errors = response.get("errors").expect("errors should be present");
    eprintln!("{response:?}");
    assert_no_at_in_path(errors); // FAILS: [String("topProducts"), String("@"), String("reviews"), String("@"), String("author")]

    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn test_nested_response_failure_malformed() -> Result<(), BoxError> {
    if !graph_os_enabled() {
        return Ok(());
    }

    let response = send_query_to_router(
        query(),
        response_template(products_response(ResponseType::Ok)),
        response_template(inventory_response(ResponseType::Ok)),
        response_template(reviews_response(ResponseType::Ok)),
        response_template(accounts_response(ResponseType::Error(ErrorType::Malformed))),
    )
    .await?;
    let errors = response.get("errors").expect("errors should be present");
    eprintln!("{response:?}");
    assert_no_at_in_path(errors); // FAILS: [String("topProducts"), String("@"), String("reviews"), String("@"), String("author")]

    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn test_nested_response_failure_404() -> Result<(), BoxError> {
    if !graph_os_enabled() {
        return Ok(());
    }

    let response = send_query_to_router(
        query(),
        response_template(products_response(ResponseType::Ok)),
        response_template(inventory_response(ResponseType::Ok)),
        response_template(reviews_response(ResponseType::Ok)),
        ResponseTemplate::new(404),
    )
    .await?;
    let errors = response.get("errors").expect("errors should be present");
    eprintln!("{response:?}");
    assert_no_at_in_path(errors); // FAILS: [String("topProducts"), String("@"), String("reviews"), String("@"), String("author")]

    Ok(())
}
