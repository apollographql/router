use std::sync::Arc;
use std::sync::atomic::AtomicU32;
use std::sync::atomic::Ordering;

use apollo_compiler::parser::Parser;
use apollo_router::TestHarness;
use apollo_router::graphql;
use apollo_router::layers::ServiceExt as _;
use apollo_router::services::execution;
use apollo_router::services::supergraph;
use serde_json::json;
use tower::BoxError;
use tower::ServiceExt;
use tracing_test::internal;

use crate::integration::IntegrationTest;
use crate::integration::common::Query;

#[tokio::test(flavor = "multi_thread")]
async fn test_response_errors() {
    let (mut service, execution_count) = build_test_harness(json!({
        "max_root_fields": 1,
        "max_aliases": 2,
        "max_depth": 3,
        "max_height": 4,
    }))
    .await;
    macro_rules! expect_errors {
        ($query: expr, $expected_error_codes: expr) => {
            expect_errors(
                run_request(&mut service, $query).await,
                $expected_error_codes,
            )
        };
    }

    assert_eq!(execution_count(), 0);
    expect_errors!("{ me { id }}", &[]);
    assert_eq!(execution_count(), 1);

    // This query is just under each limit
    let query = "{
            topProducts {
                productName: name
                reviews {
                    reviewBody: body
                }
            }
        }";
    expect_errors!(query, &[]);
    assert_eq!(execution_count(), 2);

    // Exceeding any one limit is sufficient for the request to be rejected
    let query = "{
            me { id }
            topProducts { name }
        }";
    expect_errors!(query, &["MAX_ROOT_FIELDS_LIMIT"]);
    assert_eq!(execution_count(), 2); // no execution

    let query = "{
            topProducts {
                productName: name
                productReviews: reviews {
                    reviewBody: body
                }
            }
        }";
    expect_errors!(query, &["MAX_ALIASES_LIMIT"]);
    assert_eq!(execution_count(), 2);

    // Max depth in a regular query
    let query = "{
            topProducts {
                reviews {
                    author {
                        name
                    }
                }
            }
        }";
    expect_errors!(query, &["MAX_DEPTH_LIMIT"]);
    assert_eq!(execution_count(), 2);

    // Max depth with a fragment
    let query = "{
            topProducts {
                reviews {
                    ... on Review {
                       author {
                           name
                       }
                    }
                }
            }
        }";
    expect_errors!(query, &["MAX_DEPTH_LIMIT"]);
    assert_eq!(execution_count(), 2);

    // Max height with a fragment
    let query = "{
            topProducts {
                name
                reviews {
                    ...reviewBody
                }
            }
        }
        fragment reviewBody on Review {
            body
            id
        }
        ";
    expect_errors!(query, &["MAX_HEIGHT_LIMIT"]);
    assert_eq!(execution_count(), 2);

    // If multiple limits are exceeded, as many errors are emitted
    expect_errors!(
        "{
                topProducts {
                    productName: name
                    productReviews: reviews {
                        reviewAuthor: author {
                            name
                        }
                    }
                }
            }",
        &["MAX_DEPTH_LIMIT", "MAX_HEIGHT_LIMIT", "MAX_ALIASES_LIMIT"]
    );
    assert_eq!(execution_count(), 2);

    // Rejecting errors does not break the server
    expect_errors!("{ me { id }}", &[]);
    assert_eq!(execution_count(), 3); // new execution

    // Aliases still contribute to height
    let query = "{
        topProducts {
            productName: name
            similarProduct: name
            name
            reviews {
                body
            }
        }
    }";
    expect_errors!(query, &["MAX_HEIGHT_LIMIT"]);
    assert_eq!(execution_count(), 3);

    // Depth, height, and alias limits should be exceeded in this query with
    // inline and named fragments.
    let query = "
    query getProduct{
        topProducts {
            ... on Product {
                poorReviews: reviews {
                    ...reviewsFragment
                }
                averageReviews: reviews {
                    ...reviewsFragment
                }
            } 
        }
    }

    fragment reviewsFragment on Review {
        body
        author {
            penname: name
        }
    } 
    ";
    expect_errors!(
        query,
        &["MAX_DEPTH_LIMIT", "MAX_HEIGHT_LIMIT", "MAX_ALIASES_LIMIT"]
    );
    assert_eq!(execution_count(), 3);

    // Depth, height, and alias limits should be exceeded in this query with
    // inline and named fragments.
    let query = "
    query getProduct{
        topProducts {
            ... on Product {
                poorReviews: reviews {
                    ...reviewsFragment
                }
                averageReviews: reviews {
                    ...reviewsFragment
                }
            } 
        }
    }

    fragment reviewsFragment on Review {
        body
        author {
            penname: name
        }
    } 
    ";
    expect_errors!(
        query,
        &["MAX_DEPTH_LIMIT", "MAX_HEIGHT_LIMIT", "MAX_ALIASES_LIMIT"]
    );
    assert_eq!(execution_count(), 3);
}

#[tokio::test(flavor = "multi_thread")]
async fn test_warn_only() {
    let (mut service, execution_count) = build_test_harness(json!({
        "max_root_fields": 1,
        "max_depth": 2,
        "warn_only": true,
    }))
    .await;

    // no limit exceedeed
    expect_errors(run_request(&mut service, "{me { id }}").await, &[]);
    assert_eq!(execution_count(), 1);

    // exceeds limits, but still executed with a warning logged.
    // no error in the response.
    let query = "{
        me { id }
        topProducts { reviews { body } }
    }";
    expect_errors(run_request(&mut service, query).await, &[]);
    assert_eq!(execution_count(), 2);
}

#[tokio::test(flavor = "current_thread")]
async fn test_warn_only_in_memory_cache_logs_twice() {
    internal::global_buf().lock().unwrap().clear();
    let mock_writer = internal::MockWriter::new(internal::global_buf());
    let subscriber = internal::get_subscriber(mock_writer, "apollo_router=warn");
    let _guard = tracing::dispatcher::set_default(&subscriber);

    let (mut service, execution_count) = build_test_harness(json!({
        "max_aliases": 1,
        "warn_only": true,
    }))
    .await;

    let raw_query = "{
        topProducts {
            productName: name
            productReviews: reviews {
                reviewBody: body
            }
        }
    }";
    let query = Parser::new()
        .parse_ast(raw_query, "query.graphql")
        .expect("valid query")
        .to_string();

    expect_errors(run_request(&mut service, &query).await, &[]);
    expect_errors(run_request(&mut service, &query).await, &[]);

    assert_eq!(execution_count(), 2);
    let logs = String::from_utf8(internal::global_buf().lock().unwrap().to_vec()).unwrap();
    let warning_count = logs
        .lines()
        .filter(|line| line.contains("request exceeded complexity limits"))
        .count();
    assert_eq!(warning_count, 2);
}

#[cfg(any(not(feature = "ci"), all(target_arch = "x86_64", target_os = "linux")))]
#[tokio::test(flavor = "multi_thread")]
async fn test_warn_only_reload_cached_plan_enforces_limits() -> Result<(), BoxError> {
    let base_config = r#"
supergraph:
  query_planning:
    cache:
      in_memory:
        limit: 1
      redis:
        required_to_start: true
        urls:
          - redis://localhost:6379
        ttl: 10s
limits:
  max_aliases: 1
"#;

    let config_warn_only = format!("{base_config}\n  warn_only: true");

    let config_enforce = format!("{base_config}\n  warn_only: false");

    let mut router = IntegrationTest::builder()
        .config(config_warn_only)
        .build()
        .await;
    router.start().await;
    router.assert_started().await;

    let query = "query Test { topProducts { name1: name name2: name } }";

    let request = Query::builder()
        .body(json!({"query": query, "variables": {}}))
        .build();

    let (_, response) = router.execute_query(request.clone()).await;
    let body: serde_json::Value = response.json().await.unwrap();
    assert!(
        body.get("errors").is_none(),
        "expected no errors with warn_only, got: {body:?}"
    );
    assert!(body.get("data").is_some());

    router.update_config(&config_enforce).await;
    router.assert_reloaded().await;

    let (_, response) = router.execute_query(request).await;
    let body: serde_json::Value = response.json().await.unwrap();

    let errors = body
        .get("errors")
        .and_then(|value| value.as_array())
        .expect("expected errors after enforcement");
    let error_codes: Vec<&str> = errors
        .iter()
        .filter_map(|error| {
            error
                .get("extensions")
                .and_then(|ext| ext.get("code"))
                .and_then(|code| code.as_str())
        })
        .collect();
    assert!(
        error_codes.contains(&"MAX_ALIASES_LIMIT"),
        "expected MAX_ALIASES_LIMIT, got: {error_codes:?}"
    );

    router.graceful_shutdown().await;
    Ok(())
}

async fn build_test_harness(
    limits_config: serde_json::Value,
) -> (supergraph::BoxCloneSyncService, impl Fn() -> u32) {
    let execution_count = Arc::new(AtomicU32::new(0));
    let execution_count_2 = execution_count.clone();
    let get_execution_count = move || execution_count_2.load(Ordering::Acquire);
    let service = TestHarness::builder()
        .configuration_json(json!({
            "limits": limits_config,
            "include_subgraph_errors": { "all": true },
        }))
        .unwrap()
        // .log_level("warn")
        .execution_hook(move |_inner_service| {
            // Don’t actually execute (ignore the inner execution service),
            // instead keep track of which requests were about to be executed
            // with a counter and a marker in the dummy response.
            let execution_count = execution_count.clone();
            tower::service_fn(move |request: execution::Request| {
                let execution_count = execution_count.clone();
                async move {
                    execution_count.fetch_add(1, Ordering::Release);
                    Ok(execution::Response::builder()
                        .data(json!({"reached execution": true})) // No error
                        .context(request.context)
                        .build()
                        .unwrap())
                }
            })
            .boxed_clone_sync()
        })
        .build_supergraph()
        .await
        .unwrap();
    (service, get_execution_count)
}

async fn run_request(
    service: &mut supergraph::BoxCloneSyncService,
    query: &str,
) -> graphql::Response {
    let request = supergraph::Request::fake_builder()
        .query(query)
        .build()
        .unwrap();
    service
        .oneshot(request)
        .await
        .unwrap()
        .next_response()
        .await
        .unwrap()
}

#[track_caller]
fn expect_errors(response: graphql::Response, expected_error_codes: &[&str]) {
    let errors = response.errors;
    if !errors
        .iter()
        .map(|err| err.extensions.get("code")?.as_str())
        .eq(expected_error_codes.iter().map(|&code| Some(code)))
    {
        panic!("expected errors with codes {expected_error_codes:#?}, got {errors:#?}")
    }
    if expected_error_codes.is_empty() {
        let reached_execution = response
            .data
            .expect("expected a response with data")
            .get("reached execution")
            .expect("expected data with a 'reached execution' key")
            .as_bool();
        assert!(reached_execution.unwrap());
    } else {
        assert!(response.data.is_none())
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn test_request_bytes_limit_with_coprocessor() -> Result<(), BoxError> {
    let mut router = IntegrationTest::builder()
        .config(include_str!(
            "fixtures/request_bytes_limit_with_coprocessor.router.yaml"
        ))
        .build()
        .await;
    router.start().await;
    router.assert_started().await;
    let (_, resp) = router
        .execute_query(Query::default().with_huge_query())
        .await;
    assert_eq!(resp.status(), 413);
    router.graceful_shutdown().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn test_request_bytes_limit() -> Result<(), BoxError> {
    let mut router = IntegrationTest::builder()
        .config(include_str!("fixtures/request_bytes_limit.router.yaml"))
        .build()
        .await;
    router.start().await;
    router.assert_started().await;
    let (_, resp) = router
        .execute_query(Query::default().with_huge_query())
        .await;
    assert_eq!(resp.status(), 413);
    router.graceful_shutdown().await;
    Ok(())
}
