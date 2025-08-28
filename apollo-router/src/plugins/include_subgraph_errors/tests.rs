use futures::StreamExt;
use insta::with_settings;
use serde_json::Map;
use serde_json::Value;
use serde_json::json;
use tower::BoxError;

use super::*; // Import items from mod.rs
use crate::graphql;
use crate::plugins::test::PluginTestHarness;
use crate::services::supergraph; // Required for collect

const PRODUCT_ERROR_RESPONSE: &[&str] = &[
    r#"{"data":{"topProducts":null},"errors":[{"message":"Could not query products","path":[],"extensions":{"test":"value","code":"FETCH_ERROR", "apollo.private.subgraph.name": "products"}}]}"#,
];
const ACCOUNT_ERROR_RESPONSE: &[&str] = &[
    r#"{"data":null,"errors":[{"message":"Account service error","path":[],"extensions":{"code":"ACCOUNT_FAIL", "apollo.private.subgraph.name": "accounts"}}]}"#,
];
const VALID_RESPONSE: &[&str] = &[
    r#"{"data":{"topProducts":[{"upc":"1","name":"Table","reviews":[{"id":"1","product":{"name":"Table"},"author":{"id":"1","name":"Ada Lovelace"}},{"id":"4","product":{"name":"Table"},"author":{"id":"2","name":"Alan Turing"}}]},{"upc":"2","name":"Couch","reviews":[{"id":"2","product":{"name":"Couch"},"author":{"id":"1","name":"Ada Lovelace"}}]}]}}"#,
];
const NON_SUBGRAPH_ERROR: &[&str] = &[
    r#"{"data":{"topProducts":null},"errors":[{"message":"Authentication error","path":[],"extensions":{"test":"value","code":"AUTH_ERROR"}}]}"#,
];
const INCREMENTAL_RESPONSE: &[&str] = &[
    r#"{"data":{"topProducts":null},"errors":[{"message":"Main errors error","path":[],"extensions":{"test":"value","code":"MAIN_ERROR", "apollo.private.subgraph.name": "products"}}]}"#,
    r#"{"incremental":[{"data":{"topProducts":null},"errors":[{"message":"Incremental error","path":[],"extensions":{"test":"value","code":"INCREMENTAL_ERROR", "apollo.private.subgraph.name": "products"}}]}]}"#,
];

async fn build_harness(
    plugin_config: &Value,
) -> Result<PluginTestHarness<IncludeSubgraphErrors>, BoxError> {
    let mut config = Map::new();
    config.insert("include_subgraph_errors".to_string(), plugin_config.clone());
    let config = serde_yaml::to_string(&config).expect("config to yaml");
    PluginTestHarness::builder().config(&config).build().await
}

async fn run_test_case(
    config: &Value,
    mock_responses: &[&str], // The array of responses
    snapshot_suffix: &str,   // Suffix for the snapshot file name
) {
    let harness = build_harness(config).await.expect("plugin should load");

    let mock_response_elements = mock_responses
        .iter()
        .map(|response| {
            serde_json::from_str(response).expect("Failed to parse mock response bytes")
        })
        .collect::<Vec<_>>();

    let service = harness.supergraph_service(move |req| {
        let mock_response_elements = mock_response_elements.clone();
        async {
            supergraph::Response::fake_stream_builder()
                .responses(mock_response_elements)
                .context(req.context)
                .build()
        }
    });
    let mut response = service.call_default().await.unwrap();

    // Collect the actual response body (potentially modified by the plugin)
    let actual_responses: Vec<graphql::Response> = response.response.body_mut().collect().await;

    let config = serde_yaml::to_string(config).expect("config to yaml");
    let parsed_responses = mock_responses
        .iter()
        .map(|response| serde_json::from_str(response).expect("request"))
        .collect::<Vec<Value>>();
    let request = serde_json::to_string_pretty(&parsed_responses).expect("request to json");

    let description = format!("CONFIG:\n{config}\n\nREQUEST:\n{request}");
    with_settings!({
        description => description,
    }, {
        // Assert the collected body against a snapshot
        insta::assert_yaml_snapshot!(snapshot_suffix, actual_responses);
    });
}

#[tokio::test]
async fn it_returns_valid_response() {
    run_test_case(
        &json!({ "all": false }),
        VALID_RESPONSE,   // Mock stream input
        "valid_response", // Snapshot suffix
    )
    .await;
}

#[tokio::test]
async fn it_redacts_all_subgraphs_explicit_redact() {
    run_test_case(
        &json!({ "all": false }),
        PRODUCT_ERROR_RESPONSE, // Mock original error
        "redact_all_explicit",  // Snapshot suffix
    )
    .await;
}

#[tokio::test]
async fn it_redacts_all_subgraphs_implicit_redact() {
    run_test_case(
        &json!({}), // Default is all: false
        PRODUCT_ERROR_RESPONSE,
        "redact_all_implicit",
    )
    .await;
}

#[tokio::test]
async fn it_does_not_redact_all_subgraphs_explicit_allow() {
    run_test_case(
        &json!({ "all": true }),
        PRODUCT_ERROR_RESPONSE, // Mock original error
        "allow_all_explicit",   // Snapshot suffix
    )
    .await;
}

#[tokio::test]
async fn it_does_not_redact_all_implicit_redact_product_explicit_allow_for_product_query() {
    run_test_case(
        &json!({ "subgraphs": {"products": true }}), // Default all: false
        PRODUCT_ERROR_RESPONSE,
        "allow_product_override_implicit_redact",
    )
    .await;
}

#[tokio::test]
async fn it_does_redact_all_implicit_redact_product_explicit_allow_for_review_query() {
    run_test_case(
        &json!({ "subgraphs": {"reviews": true }}), // Allows reviews, defaults products to redact
        PRODUCT_ERROR_RESPONSE,                     // Mock original error for products
        "redact_product_when_review_allowed",
    )
    .await;
}

#[tokio::test]
async fn it_does_not_redact_all_explicit_allow_review_explicit_redact_for_product_query() {
    run_test_case(
        &json!({ "all": true, "subgraphs": {"reviews": false }}), // Global allow, reviews redact
        PRODUCT_ERROR_RESPONSE,                                   // Mock original
        "allow_product_when_review_redacted",
    )
    .await;
}

#[tokio::test]
async fn it_does_redact_all_explicit_allow_product_explicit_redact_for_product_query() {
    run_test_case(
        &json!({ "all": true, "subgraphs": {"products": false }}), // Global allow, products redact
        PRODUCT_ERROR_RESPONSE,                                    // Mock original
        "redact_product_override_explicit_allow",
    )
    .await;
}

#[tokio::test]
async fn it_does_not_redact_all_explicit_allow_account_explicit_redact_for_product_query() {
    run_test_case(
        &json!({ "all": true, "subgraphs": {"accounts": false }}), // Global allow, accounts redact
        PRODUCT_ERROR_RESPONSE,                                    // Mock original
        "allow_product_when_account_redacted",
    )
    .await;
}

#[tokio::test]
async fn it_does_redact_all_explicit_allow_account_explicit_redact_for_account_query() {
    run_test_case(
        &json!({ "all": true, "subgraphs": {"accounts": false }}), // Global allow, accounts redact
        ACCOUNT_ERROR_RESPONSE,                                    // Mock original account error
        "redact_account_override_explicit_allow",
    )
    .await;
}

#[tokio::test]
async fn it_does_not_allow_both_allow_and_deny_list_in_global_config() {
    let config_json = json!({
        "all": {
            "redact_message": false,
            "allow_extensions_keys": [],
            "deny_extensions_keys": []
        }
    });
    let result = build_harness(&config_json).await;
    assert_eq!(
        result.expect_err("expected error").to_string(),
        "Global config cannot have both allow_extensions_keys and deny_extensions_keys"
    );
}

#[tokio::test]
async fn it_does_not_allow_both_allow_and_deny_list_in_a_subgraph_config() {
    let config_json = json!({
        "all": { // Global must be object type if subgraph is object type
            "redact_message": false,
            "allow_extensions_keys": [],
        },
        "subgraphs": {
            "products": {
                "redact_message": false,
                "allow_extensions_keys": [],
                "deny_extensions_keys": []
            }
        }
    });
    let result = build_harness(&config_json).await;
    assert_eq!(
        result.expect_err("expected error").to_string(),
        "A subgraph config cannot have both allow_extensions_keys and deny_extensions_keys"
    );
}

#[tokio::test]
async fn it_does_not_allow_subgraph_config_with_object_when_global_is_boolean() {
    let config_json = json!({
        "all": false, // Global is boolean
        "subgraphs": {
            "products": { // Subgraph is object
                "redact_message": true
            }
        }
    });
    let result = build_harness(&config_json).await;
    assert_eq!(
        result.expect_err("expected error").to_string(),
        "Subgraph 'products' must use boolean config when global config is boolean"
    );
}

#[tokio::test]
async fn it_allows_subgraph_config_with_boolean_when_global_is_object() {
    let config_json = json!({
        "all": {
            "redact_message": true,
            "deny_extensions_keys": ["code"]
        },
        "subgraphs": {
            "products": true // Boolean subgraph config is allowed
        }
    });
    let result = build_harness(&config_json).await;
    assert!(result.is_ok()); // Check plugin creation succeeded
}

#[tokio::test]
async fn it_allows_any_subgraph_config_type_when_global_is_object() {
    let config_json = json!({
        "all": {
            "redact_message": true,
            "deny_extensions_keys": ["code"] // Global deny list
        },
        "subgraphs": {
            "products": {
                "allow_extensions_keys": ["code"]  // Subgraph allow overrides global deny
            },
            "reviews": {
                "deny_extensions_keys": ["reason"]  // Subgraph deny extends global deny
            },
            "inventory": {
                "exclude_global_keys": ["code"]  // CommonOnly inherits global deny, but excludes 'code'
            },
            "accounts": true  // Boolean overrides global object config
        }
    });
    let result = build_harness(&config_json).await;
    assert!(result.is_ok());
}

#[tokio::test]
async fn it_filters_extensions_based_on_global_allow_list_and_redacts_message() {
    run_test_case(
        &json!({
            "all": {
                "redact_message": true,
                "allow_extensions_keys": ["code", "service"] // Allow 'code' and 'service'
            }
        }),
        PRODUCT_ERROR_RESPONSE,
        "filter_global_allow_redact_msg",
    )
    .await;
}

#[tokio::test]
async fn it_filters_extensions_based_on_global_allow_list_keeps_message() {
    run_test_case(
        &json!({
            "all": {
                "redact_message": false,
                "allow_extensions_keys": ["code", "service"] // Allow 'code' and 'service'
            }
        }),
        PRODUCT_ERROR_RESPONSE,
        "filter_global_allow_keep_msg",
    )
    .await;
}

#[tokio::test]
async fn it_allows_subgraph_bool_override_global_deny_config() {
    run_test_case(
        &json!({
            "all": {
                "redact_message": true,
                "deny_extensions_keys": ["code"],
            },
            "subgraphs": { "products": true }
        }),
        PRODUCT_ERROR_RESPONSE,
        "subgraph_bool_true_override_global_deny",
    )
    .await;
}

#[tokio::test]
async fn it_allows_subgraph_bool_override_global_allow_config() {
    run_test_case(
        &json!({
            "all": {
                "redact_message": true,
                "allow_extensions_keys": ["code"],
            },
            "subgraphs": { "products": false }
        }),
        PRODUCT_ERROR_RESPONSE,
        "subgraph_bool_false_override_global_allow",
    )
    .await;
}

#[tokio::test]
async fn it_allows_subgraph_object_to_override_global_redaction() {
    run_test_case(
        &json!({
            "all": {
                "redact_message": false,
                "allow_extensions_keys": ["code", "service"],
            },
            "subgraphs": {
                "products": { "redact_message": true } // Override redaction
            }
        }),
        PRODUCT_ERROR_RESPONSE,
        "subgraph_obj_override_redaction",
    )
    .await;
}

#[tokio::test]
async fn it_allows_subgraph_to_exclude_key_from_global_allow_list() {
    run_test_case(
        &json!({
            "all": {
                "redact_message": false,
                "allow_extensions_keys": ["code", "test", "service"]
            },
            "subgraphs": {
                "products": { "exclude_global_keys": ["test"] } // Exclude 'test'
            }
        }),
        PRODUCT_ERROR_RESPONSE,
        "subgraph_exclude_global_allow",
    )
    .await;
}

#[tokio::test]
async fn it_allows_subgraph_deny_list_to_override_global_allow_list() {
    run_test_case(
        &json!({
            "all": {
                "redact_message": false,
                "allow_extensions_keys": ["code", "test", "service"]
            },
            "subgraphs": {
                "products": { "deny_extensions_keys": ["test", "service"] } // Deny overrides global allow
            }
        }),
        PRODUCT_ERROR_RESPONSE,
        "subgraph_deny_override_global_allow",
    )
    .await;
}

#[tokio::test]
async fn it_allows_subgraph_allow_list_to_override_global_deny_list() {
    run_test_case(
        &json!({
            "all": {
                "redact_message": false,
                "deny_extensions_keys": ["test", "service"]
            },
            "subgraphs": {
                "products": { "allow_extensions_keys": ["code", "test"] } // Allow overrides global deny for 'test'
            }
        }),
        PRODUCT_ERROR_RESPONSE,
        "subgraph_allow_override_global_deny",
    )
    .await;
}

#[tokio::test]
async fn it_allows_subgraph_deny_list_to_extend_global_deny_list() {
    run_test_case(
        &json!({
            "all": {
                "redact_message": true,
                "deny_extensions_keys": ["test"]
            },
            "subgraphs": {
                "products": { "deny_extensions_keys": ["code"] } // Extends global deny
            }
        }),
        PRODUCT_ERROR_RESPONSE,
        "subgraph_deny_extend_global_deny",
    )
    .await;
}

#[tokio::test]
async fn it_allows_subgraph_allow_list_to_extend_global_allow_list() {
    run_test_case(
        &json!({
            "all": {
                "redact_message": false,
                "allow_extensions_keys": ["test", "service"]
            },
            "subgraphs": {
                "products": { "allow_extensions_keys": ["code"] } // Extends global allow
            }
        }),
        PRODUCT_ERROR_RESPONSE,
        "subgraph_allow_extend_global_allow",
    )
    .await;
}

#[tokio::test]
async fn it_redacts_service_extension_if_denied() {
    run_test_case(
        &json!({
            "all": {
                "redact_message": false,
                "allow_extensions_keys": ["code", "test", "service"] // Allow globally initially
            },
            "subgraphs": {
                "products": { "deny_extensions_keys": ["service"] } // Deny service specifically
            }
        }),
        PRODUCT_ERROR_RESPONSE,
        "subgraph_deny_service",
    )
    .await;
}

#[tokio::test]
async fn it_includes_service_extension_if_allowed() {
    run_test_case(
        &json!({
            "all": {
                "redact_message": true,
                "deny_extensions_keys": ["code", "test"]
            },
            "subgraphs": {
                "products": { "allow_extensions_keys": ["service"] } // Allow service specifically
            }
        }),
        PRODUCT_ERROR_RESPONSE,
        "subgraph_allow_service",
    )
    .await;
}

#[tokio::test]
async fn it_does_not_add_service_extension_for_non_subgraph_errors() {
    run_test_case(
        &json!({
            "all": true,
        }),
        NON_SUBGRAPH_ERROR,
        "non_subgraph_error",
    )
    .await;
}

#[tokio::test]
async fn it_processes_incremental_responses() {
    run_test_case(
        &json!({
            "all": true,
        }),
        INCREMENTAL_RESPONSE,
        "incremental_response",
    )
    .await;
}
