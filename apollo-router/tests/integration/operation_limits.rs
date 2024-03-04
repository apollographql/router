use std::sync::atomic::AtomicU32;
use std::sync::atomic::Ordering;
use std::sync::Arc;

use apollo_router::graphql;
use apollo_router::services::execution;
use apollo_router::services::supergraph;
use apollo_router::TestHarness;
use serde_json::json;
use tower::ServiceExt;

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

async fn build_test_harness(
    limits_config: serde_json::Value,
) -> (supergraph::BoxCloneService, impl Fn() -> u32) {
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
            .boxed()
        })
        .build_supergraph()
        .await
        .unwrap();
    (service, get_execution_count)
}

async fn run_request(service: &mut supergraph::BoxCloneService, query: &str) -> graphql::Response {
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
