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

fn query() -> Query {
    let query_str =
        r#"query Q { topProducts { name inStock reviews { id author { username name } } } }"#;

    Query::builder()
        .traced(true)
        .body(json!({"query": query_str}))
        .build()
}

fn products_response(errors: bool) -> Value {
    if errors {
        json!({"errors": [{ "message": "products error", "path": [] }]})
    } else {
        json!({
            "data": {
                "topProducts": [
                    { "__typename": "Product", "name": "Table", "upc": "1" },
                    { "__typename": "Product", "name": "Chair", "upc": "2" },
                ]
            },
        })
    }
}

fn inventory_response(errors: bool) -> Value {
    if errors {
        json!({"errors": [{ "message": "inventory error", "path": [] }]})
    } else {
        json!({"data": {"_entities": [{"inStock": true}, {"inStock": false}]}})
    }
}

fn reviews_response(errors: bool) -> Value {
    if errors {
        json!({"errors": [{ "message": "reviews error", "path": [] }]})
    } else {
        json!({
            "data": {
                "_entities": [
                    {"reviews": [{"id": "1", "author": {"__typename": "User", "username": "@ada", "id": "1"}}, {"id": "1", "author": {"__typename": "User", "username": "@alan", "id": "2"}}]},
                    {"reviews": [{"id": "3", "author": {"__typename": "User", "username": "@alan", "id": "2"}}]},
                ]
            }
        })
    }
}

fn accounts_response(errors: bool) -> Value {
    if errors {
        json!({"malformed": true})
    } else {
        json!({"data": {"_entities": [{"name": "Ada"}, {"name": "Alan"}]}})
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
        response_template(products_response(false)),
        response_template(inventory_response(false)),
        response_template(reviews_response(false)),
        response_template(accounts_response(false)),
    )
    .await?;
    assert!(response.get("errors").is_none());

    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn test_first_response_failure() -> Result<(), BoxError> {
    if !graph_os_enabled() {
        return Ok(());
    }

    let response = send_query_to_router(
        query(),
        response_template(products_response(true)),
        response_template(inventory_response(false)),
        response_template(reviews_response(false)),
        response_template(accounts_response(false)),
    )
    .await?;
    let errors = response.get("errors").expect("errors should be present");
    assert_no_at_in_path(errors);

    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn test_second_response_failure() -> Result<(), BoxError> {
    if !graph_os_enabled() {
        return Ok(());
    }

    let response = send_query_to_router(
        query(),
        response_template(products_response(false)),
        response_template(inventory_response(true)),
        response_template(reviews_response(false)),
        response_template(accounts_response(false)),
    )
    .await?;
    let errors = response.get("errors").expect("errors should be present");
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
        response_template(products_response(false)),
        response_template(inventory_response(false)),
        response_template(reviews_response(false)),
        response_template(accounts_response(true)),
    )
    .await?;
    let errors = response.get("errors").expect("errors should be present");
    assert_no_at_in_path(errors); // FAILS: [String("topProducts"), String("@"), String("reviews"), String("@"), String("author")]

    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn test_second_and_nested_response_failures() -> Result<(), BoxError> {
    if !graph_os_enabled() {
        return Ok(());
    }

    let response = send_query_to_router(
        query(),
        response_template(products_response(false)),
        response_template(inventory_response(true)),
        response_template(reviews_response(false)),
        response_template(accounts_response(true)),
    )
    .await?;
    let errors = response.get("errors").expect("errors should be present");
    assert_no_at_in_path(errors); // FAILS: [String("topProducts"), String("@")]

    Ok(())
}
