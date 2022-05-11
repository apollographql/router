use crate::{register_plugin, Plugin, RouterRequest, RouterResponse, ServiceBuilderExt};
use http::header;
use http::{HeaderMap, StatusCode};
use schemars::JsonSchema;
use serde::Deserialize;
use std::ops::ControlFlow;
use tower::util::BoxService;
use tower::{BoxError, ServiceBuilder, ServiceExt};

#[derive(Deserialize, Debug, Clone, JsonSchema)]
#[serde(deny_unknown_fields)]
struct CSRFConfig {
    /// CSRFConfig is enabled by default;
    /// set disabled = true to disable the plugin behavior
    #[serde(default)]
    disabled: bool,
    /// Override the headers to check for by setting
    /// custom_headers
    /// Note that if you set required_headers here,
    /// you may also want to have a look at your `CORS` configuration,
    /// and make sure you either:
    /// - did not set any `allow_headers` list (so it defaults to `mirror_request`)
    /// - added your required headers to the allow_headers list, as shown in the
    /// `examples/cors-and-csrf/*.router.yaml` files.
    #[serde(default = "apollo_custom_preflight_headers")]
    required_headers: Vec<String>,
}

fn apollo_custom_preflight_headers() -> Vec<String> {
    vec![
        "x-apollo-operation-name".to_string(),
        "apollo-require-preflight".to_string(),
    ]
}

static NON_PREFLIGHTED_CONTENT_TYPES: &[&str] = &[
    "application/x-www-form-urlencoded",
    "multipart/form-data",
    "text/plain",
];

#[derive(Debug, Clone)]
struct Csrf {
    config: CSRFConfig,
}

#[async_trait::async_trait]
impl Plugin for Csrf {
    type Config = CSRFConfig;

    async fn new(config: Self::Config) -> Result<Self, BoxError> {
        Ok(Csrf { config })
    }

    fn router_service(
        &mut self,
        service: BoxService<RouterRequest, RouterResponse, BoxError>,
    ) -> BoxService<RouterRequest, RouterResponse, BoxError> {
        if !self.config.disabled {
            let required_headers = self.config.required_headers.clone();
            ServiceBuilder::new()
                .checkpoint(move |req: RouterRequest| {
                    if should_accept(&req, required_headers.as_slice()) {
                        Ok(ControlFlow::Continue(req))
                    } else {
                        let error = crate::Error {
                            message: format!("This operation has been blocked as a potential Cross-Site Request Forgery (CSRF). \
                            Please either specify a 'content-type' header (with a mime-type that is not one of {}) \
                            or provide one of the following headers: {}", 
                            NON_PREFLIGHTED_CONTENT_TYPES.join(", "),
                            required_headers.join(", ")),
                            locations: Default::default(),
                            path: Default::default(),
                            extensions: Default::default(),
                        };
                        let res = RouterResponse::builder()
                            .error(error)
                            .status_code(StatusCode::BAD_REQUEST)
                            .context(req.context)
                            .build()?;
                        Ok(ControlFlow::Break(res))
                    }
                })
                .service(service)
                .boxed()
        } else {
            service
        }
    }
}

fn should_accept(req: &RouterRequest, required_headers: &[String]) -> bool {
    let headers = req.originating_request.headers();
    content_type_requires_preflight(headers)
        || recommended_header_is_provided(headers, required_headers)
}

fn recommended_header_is_provided(headers: &HeaderMap, required_headers: &[String]) -> bool {
    required_headers
        .iter()
        .any(|header| headers.get(header).is_some())
}

fn content_type_requires_preflight(headers: &HeaderMap) -> bool {
    let joined_content_type_header_value = if let Ok(combined_headers) = headers
        .get_all(header::CONTENT_TYPE)
        .iter()
        .map(|header_value| {
            // https://github.com/apollographql/router/pull/1006#discussion_r869777439
            header_value
                .to_str()
                .map(|as_str| as_str.trim().replace('\u{0009}', "\u{0020}")) // replace tab with space
        })
        .collect::<Result<Vec<_>, _>>()
    {
        // https://fetch.spec.whatwg.org/#concept-header-list-combine
        combined_headers.join("\u{002C}\u{0020}") // ', '
    } else {
        // We couldn't parse a header value, let's err on the side of caution here
        return false;
    };

    dbg!("'\u{002C}\u{0020}'");

    if let Ok(mime_type) = joined_content_type_header_value.parse::<mime::Mime>() {
        // If we get here, this means that we couldn't parse the content-type value into
        // a valid mime type... which would be safe enough for us to assume preflight was triggered if the `mime`
        // crate followed the fetch specification, but it unfortunately doesn't (see comment above).
        //
        // Better safe than sorry, we will claim we don't have solid enough reasons
        // to believe the request will have triggered preflight

        !NON_PREFLIGHTED_CONTENT_TYPES.contains(&mime_type.essence_str())
    } else {
        false
    }
}

register_plugin!("apollo", "csrf", Csrf);

#[cfg(test)]
mod csrf_tests {

    #[tokio::test]
    async fn plugin_registered() {
        crate::plugins()
            .get("apollo.csrf")
            .expect("Plugin not found")
            .create_instance(&serde_json::json!({ "disabled": true }))
            .await
            .unwrap();

        crate::plugins()
            .get("apollo.csrf")
            .expect("Plugin not found")
            .create_instance(&serde_json::json!({}))
            .await
            .unwrap();
    }

    use super::*;
    use crate::{plugin::utils::test::MockRouterService, ResponseBody};
    use serde_json_bytes::json;
    use tower::{Service, ServiceExt};

    #[tokio::test]
    async fn it_lets_preflighted_request_pass_through() {
        let expected_response_data = json!({ "test": 1234 });
        let expected_response_data2 = expected_response_data.clone();
        let mut mock_service = MockRouterService::new();
        mock_service.expect_call().times(2).returning(move |_| {
            RouterResponse::fake_builder()
                .data(expected_response_data2.clone())
                .build()
        });

        let mock = mock_service.build();

        let mut service_stack = Csrf::new(CSRFConfig {
            disabled: false,
            required_headers: apollo_custom_preflight_headers(),
        })
        .await
        .unwrap()
        .router_service(mock.boxed());

        let with_preflight_content_type = RouterRequest::fake_builder()
            .headers(
                [("content-type".into(), "application/json".into())]
                    .into_iter()
                    .collect(),
            )
            .build()
            .unwrap();

        let res = service_stack
            .ready()
            .await
            .unwrap()
            .call(with_preflight_content_type)
            .await
            .unwrap();

        assert_data(res, expected_response_data.clone());

        let with_preflight_header = RouterRequest::fake_builder()
            .headers(
                [("apollo-require-preflight".into(), "this-is-a-test".into())]
                    .into_iter()
                    .collect(),
            )
            .build()
            .unwrap();

        let res = service_stack.oneshot(with_preflight_header).await.unwrap();

        assert_data(res, expected_response_data);
    }

    #[tokio::test]
    async fn it_rejects_non_preflighted_headers_request() {
        let mock = MockRouterService::new().build();

        let service_stack = Csrf::new(CSRFConfig {
            disabled: false,
            required_headers: apollo_custom_preflight_headers(),
        })
        .await
        .unwrap()
        .router_service(mock.boxed());

        let non_preflighted_request = RouterRequest::fake_builder().build().unwrap();

        let res = service_stack
            .oneshot(non_preflighted_request)
            .await
            .unwrap();

        assert_error(res);
    }

    #[tokio::test]
    async fn it_rejects_non_preflighted_content_type_request() {
        let mock = MockRouterService::new().build();

        let service_stack = Csrf::new(CSRFConfig {
            disabled: false,
            required_headers: apollo_custom_preflight_headers(),
        })
        .await
        .unwrap()
        .router_service(mock.boxed());

        let non_preflighted_request = RouterRequest::fake_builder()
            .headers(
                [("content-type".into(), "text/plain".into())]
                    .into_iter()
                    .collect(),
            )
            .build()
            .unwrap();

        let res = service_stack
            .oneshot(non_preflighted_request)
            .await
            .unwrap();

        assert_error(res);
    }

    #[tokio::test]
    async fn it_accepts_non_preflighted_headers_request_when_plugin_is_disabled() {
        let expected_response_data = json!({ "test": 1234 });
        let expected_response_data2 = expected_response_data.clone();
        let mut mock_service = MockRouterService::new();
        mock_service.expect_call().times(1).returning(move |_| {
            RouterResponse::fake_builder()
                .data(expected_response_data2.clone())
                .build()
        });

        let mock = mock_service.build();

        let service_stack = Csrf::new(CSRFConfig {
            disabled: true,
            required_headers: apollo_custom_preflight_headers(),
        })
        .await
        .unwrap()
        .router_service(mock.boxed());

        let non_preflighted_request = RouterRequest::fake_builder().build().unwrap();

        let res = service_stack
            .oneshot(non_preflighted_request)
            .await
            .unwrap();

        assert_data(res, expected_response_data);
    }

    fn assert_data(res: RouterResponse, expected_response_data: serde_json_bytes::Value) {
        match res.response.into_body() {
            ResponseBody::GraphQL(res) => {
                assert_eq!(res.data.unwrap(), expected_response_data);
            }
            other => panic!("expected graphql response, found {:?}", other),
        }
    }

    fn assert_error(res: RouterResponse) {
        match res.response.into_body() {
            ResponseBody::GraphQL(res) => {
                assert_eq!(
                    1,
                    res.errors.len(),
                    "expected one(1) error in the RouterResponse, found {}\n{:?}",
                    res.errors.len(),
                    res.errors
                );
                assert_eq!(res.errors[0].message, "This operation has been blocked as a potential Cross-Site Request Forgery (CSRF). \
                Please either specify a 'content-type' header \
                (with a mime-type that is not one of application/x-www-form-urlencoded, multipart/form-data, text/plain) \
                or provide one of the following headers: x-apollo-operation-name, apollo-require-preflight");
            }
            other => panic!("expected graphql response, found {:?}", other),
        }
    }
}
