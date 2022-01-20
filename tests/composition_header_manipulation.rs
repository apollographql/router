use http::HeaderValue;
#[cfg(test)]
use http::Request;

#[cfg(test)]
use apollo_router_rs::{graphql, ApolloRouter};

// This would live inside the users codebase
mod various_plugins_mod {
    use std::str::FromStr;

    use http::header::HeaderName;
    use http::HeaderValue;

    use tower::util::BoxService;
    use tower::{BoxError, ServiceBuilder, ServiceExt};

    use apollo_router_rs::{Plugin, RouterResponse, ServiceBuilderExt, SubgraphRequest};

    #[derive(Default)]
    pub struct MyPlugin;
    impl Plugin for MyPlugin {
        fn subgraph_service(
            &mut self,
            name: &str,
            service: BoxService<SubgraphRequest, RouterResponse, BoxError>,
        ) -> BoxService<SubgraphRequest, RouterResponse, BoxError> {
            let all_rules = ServiceBuilder::new()
                .remove_header("C")
                .insert_header("D", HeaderValue::from(5));

            if name == "books" {
                all_rules
                    .propagate_header("A") //Propagate using our helper
                    .propagate_or_default_header("B", HeaderValue::from(2))
                    .map_request(|mut r: SubgraphRequest| {
                        // Demonstrate some manual propagation that could contain fancy logic
                        if let Some(value) = r
                            .frontend_request
                            .headers()
                            .get(HeaderName::from_str("SomeHeader").unwrap())
                        {
                            r.backend_request.headers_mut().insert("B", value.clone());
                        }
                        r
                    })
                    .service(service)
                    .boxed()
            } else {
                all_rules.service(service).boxed()
            }
        }
    }
}

// This would be cfg(test) gated, within the user's codebase
#[cfg(test)]
mod my_test_harness {
    use apollo_router_rs::SubgraphRequest;
    use http::HeaderValue;

    #[derive(Clone, Default, Debug)]
    pub struct PresenceRequirements {
        pub must_be_present: bool,
        pub header_name: String,
        pub must_have_value: Option<HeaderValue>,
    }

    impl PresenceRequirements {
        // assert_complies panics if the found headers doesn't match the requirement
        pub fn assert_complies(&self, request: &SubgraphRequest) {
            let header = request.backend_request.headers().get(&self.header_name);

            if self.must_be_present {
                assert!(
                    header.is_some(),
                    "header {} must be present but hasn't been found",
                    self.header_name.as_str(),
                );
            } else {
                assert!(
                    header.is_none(),
                    "header {} must be absent but was been found",
                    self.header_name.as_str(),
                );
            }

            if let Some(expected_value) = &self.must_have_value {
                let actual = header.clone().unwrap();
                assert_eq!(
                    actual,
                    expected_value,
                    "header {} value missmatch: expected `{:?}` found `{:?}`",
                    self.header_name.as_str(),
                    expected_value,
                    actual
                );
            }
        }
    }
}

// ----------------------

use crate::my_test_harness::PresenceRequirements;
use crate::various_plugins_mod::MyPlugin;

#[tokio::test]
async fn header_propagation() {
    let books_requirements = vec![
        PresenceRequirements {
            must_be_present: true,
            header_name: "A".to_string(),
            must_have_value: Some(HeaderValue::from_static("this is a test on header A")),
        },
        PresenceRequirements {
            must_be_present: true,
            header_name: "B".to_string(),
            must_have_value: Some(HeaderValue::from_static("this is a test on header B")),
        },
    ];

    let all_requirements = vec![
        // C must be absent from any service
        PresenceRequirements {
            must_be_present: false,
            header_name: "C".to_string(),
            must_have_value: None,
        },
        // Created / Overidden by our plugin
        PresenceRequirements {
            must_be_present: true,
            header_name: "D".to_string(),
            must_have_value: Some(HeaderValue::from(5)),
        },
    ];

    let router = ApolloRouter::builder()
        .with_after_query_planning(|planned_request| {
            dbg!("got query plan", &planned_request.query_plan);
            planned_request
        })
        .with_before_any_subgraph(move |request| {
            for requirement in all_requirements.iter() {
                requirement.assert_complies(&request)
            }
            request
        })
        .with_before_subgraph("books".to_string(), move |request| {
            for requirement in books_requirements.iter() {
                requirement.assert_complies(&request)
            }
            request
        })
        .with_plugin(MyPlugin::default())
        .build();

    router
        .call(
            Request::builder()
                .header("A", "this is a test on header A")
                .header("B", "this is a test on header B")
                .header("C", "MyPlugin should have removed this one")
                .body(graphql::Request {
                    body: "Hello1".to_string(),
                })
                .unwrap(),
        )
        .await
        .unwrap();
}
