use anyhow::Result;

// `cargo run -- -s ../../graphql/supergraph.graphql -c ./router.yaml`
fn main() -> Result<()> {
    apollo_router::main()
}

#[cfg(test)]
mod tests {
    use apollo_router::graphql;
    use apollo_router::services::subgraph;
    use apollo_router::services::supergraph;
    use http::HeaderMap;
    use http::StatusCode;
    use tower::util::ServiceExt;

    async fn cache_control_header(
        header_one: Option<String>,
        header_two: Option<String>,
    ) -> Option<String> {
        let (mock_service1, mut handle1) =
            tower_test::mock::pair::<subgraph::Request, subgraph::Response>();
        let driver1 = tokio::spawn(async move {
            let (req, responder) = handle1.next_request().await.unwrap();
            let mut headers = HeaderMap::new();
            if let Some(value) = &header_one {
                headers.insert("cache-control", value.parse().unwrap());
            }
            responder.send_response(
                subgraph::Response::fake_builder()
                    .headers(headers)
                    .context(req.context)
                    .build(),
            );
        });

        let (mock_service2, mut handle2) =
            tower_test::mock::pair::<subgraph::Request, subgraph::Response>();
        let driver2 = tokio::spawn(async move {
            let (req, responder) = handle2.next_request().await.unwrap();
            let mut headers = HeaderMap::new();
            if let Some(value) = &header_two {
                headers.insert("cache-control", value.parse().unwrap());
            }
            responder.send_response(
                subgraph::Response::fake_builder()
                    .headers(headers)
                    .context(req.context)
                    .build(),
            );
        });

        let config = serde_json::json!({
            "rhai": {
              "scripts": "src",
              "main": "cache_control.rhai",
            }
        });

        let test_harness = apollo_router::TestHarness::builder()
            .configuration_json(config)
            .unwrap()
            .subgraph_hook(move |name, _| match name {
                "accounts" => mock_service1.clone().boxed_clone(),
                _ => mock_service2.clone().boxed_clone(),
            })
            // .log_level("DEBUG")
            .build_router()
            .await
            .unwrap();

        let query = "
            query TopProducts {
              me { id }
              topProducts { name }
            }
        ";

        let request = supergraph::Request::fake_builder()
            .query(query)
            .build()
            .expect("a valid SupergraphRequest");

        let mut service_response = test_harness
            .oneshot(request.try_into().unwrap())
            .await
            .unwrap();

        let response: graphql::Response = serde_json::from_slice(
            service_response
                .next_response()
                .await
                .unwrap()
                .unwrap()
                .to_vec()
                .as_slice(),
        )
        .unwrap();
        assert_eq!(response.errors, []);

        assert_eq!(StatusCode::OK, service_response.response.status());

        let result = service_response
            .response
            .headers()
            .get("cache-control")
            .map(|v| v.to_str().expect("can parse header value").to_string());
        for driver in [driver1, driver2] {
            tokio::time::timeout(std::time::Duration::from_secs(5), driver)
                .await
                .expect("mock driver timed out — service was not called within 5 s")
                .unwrap();
        }
        result
    }

    #[tokio::test]
    async fn test_cache_control_mixed() {
        assert_eq!(
            cache_control_header(
                Some("max-age=100, private".to_string()),
                Some("max-age=50, public".to_string())
            )
            .await,
            Some("max-age=50, private".to_owned())
        );
    }

    #[tokio::test]
    async fn test_cache_control_public() {
        assert_eq!(
            cache_control_header(
                Some("max-age=100, public".to_string()),
                Some("max-age=50, public".to_string())
            )
            .await,
            Some("max-age=50, public".to_owned())
        );
    }

    #[tokio::test]
    async fn test_cache_control_missing() {
        assert_eq!(
            cache_control_header(Some("max-age=100, private".to_string()), None).await,
            None
        );
    }

    #[tokio::test]
    async fn test_subgraph_cache_no_cache() {
        assert_eq!(
            cache_control_header(
                Some("max-age=100, private".to_string()),
                Some("no-cache".to_string())
            )
            .await,
            Some("no-cache".to_string())
        );
    }

    #[tokio::test]
    async fn test_subgraph_cache_no_store() {
        assert_eq!(
            cache_control_header(
                Some("max-age=100, private".to_string()),
                Some("no-store".to_string())
            )
            .await,
            Some("no-store".to_string())
        );
    }
}
