use crate::{register_plugin, Plugin, RouterRequest, RouterResponse, ServiceBuilderExt};
use http::header::{self, HeaderName};
use http::{HeaderMap, StatusCode};
use schemars::JsonSchema;
use serde::Deserialize;
use std::ops::ControlFlow;
use tower::util::BoxService;
use tower::{BoxError, ServiceBuilder, ServiceExt};

#[derive(Deserialize, Debug, Clone, JsonSchema)]
#[serde(deny_unknown_fields)]
struct CSRFConfig {
    #[serde(default)]
    disabled: bool,
}

static NON_PREFLIGHTED_HEADER_NAMES: &[HeaderName] = &[
    header::ACCEPT,
    header::ACCEPT_LANGUAGE,
    header::CONTENT_LANGUAGE,
    header::CONTENT_TYPE,
    header::RANGE,
];

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
            ServiceBuilder::new()
                .checkpoint(move |req: RouterRequest| {
                    if should_accept(&req) {
                        Ok(ControlFlow::Continue(req))
                    } else {
                        let error = crate::Error {
                            message: format!("This operation has been blocked as a potential Cross-Site Request Forgery (CSRF). \
                            Please either specify a 'content-type' header (with a mime-type that is not one of {}) \
                            or provide a header such that the request is preflighted: \
                            https://developer.mozilla.org/en-US/docs/Web/HTTP/CORS#simple_requests", 
                            NON_PREFLIGHTED_CONTENT_TYPES.join(",")),
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

fn should_accept(req: &RouterRequest) -> bool {
    let headers = req.originating_request.headers();
    headers_require_preflight(headers) || content_type_requires_preflight(headers)
}

fn headers_require_preflight(headers: &HeaderMap) -> bool {
    headers
        .keys()
        .any(|header_name| !NON_PREFLIGHTED_HEADER_NAMES.contains(header_name))
}

fn content_type_requires_preflight(headers: &HeaderMap) -> bool {
    headers
        .get(header::CONTENT_TYPE)
        .map(|content_type| {
            if let Ok(as_str) = content_type.to_str() {
                if let Ok(mime_type) = as_str.parse::<mime::Mime>() {
                    return !NON_PREFLIGHTED_CONTENT_TYPES.contains(&mime_type.essence_str());
                }
            }
            // If we get here, this means that either turning the content-type header value
            // into a string failed (ie it's not valid UTF-8), or we couldn't parse it into
            // a valid mime type... which is actually *ok* because that would lead to a preflight.
            // (That said, it would also be reasonable to reject such requests with provided
            // yet unparsable Content-Type here.)
            true
        })
        .unwrap_or(false)
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

        let mut service_stack = Csrf::new(CSRFConfig { disabled: false })
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

        match res.response.into_body() {
            ResponseBody::GraphQL(res) => {
                assert_eq!(res.data.unwrap(), expected_response_data);
            }
            other => panic!("expected graphql response, found {:?}", other),
        }

        let with_preflight_header = RouterRequest::fake_builder()
            .headers(
                [("x-this-is-a-custom-header".into(), "this-is-a-test".into())]
                    .into_iter()
                    .collect(),
            )
            .build()
            .unwrap();

        let res = service_stack.oneshot(with_preflight_header).await.unwrap();

        match res.response.into_body() {
            ResponseBody::GraphQL(res) => {
                assert_eq!(res.data.unwrap(), expected_response_data);
            }
            other => panic!("expected graphql response, found {:?}", other),
        }
    }

    #[tokio::test]
    async fn it_rejects_non_preflighted_headers_request() {
        let mock = MockRouterService::new().build();

        let service_stack = Csrf::new(CSRFConfig { disabled: false })
            .await
            .unwrap()
            .router_service(mock.boxed());

        let non_preflighted_request = RouterRequest::fake_builder().build().unwrap();

        let res = service_stack
            .oneshot(non_preflighted_request)
            .await
            .unwrap();

        match res.response.into_body() {
            ResponseBody::GraphQL(res) => {
                assert_eq!(res.errors[0].message, "This operation has been blocked as a potential Cross-Site Request Forgery (CSRF). \
                Please either specify a 'content-type' header (with a mime-type that is not one of application/x-www-form-urlencoded,multipart/form-data,text/plain) \
                or provide a header such that the request is preflighted: https://developer.mozilla.org/en-US/docs/Web/HTTP/CORS#simple_requests");
            }
            other => panic!("expected graphql response, found {:?}", other),
        }
    }

    #[tokio::test]
    async fn it_rejects_non_preflighted_content_type_request() {
        let mock = MockRouterService::new().build();

        let service_stack = Csrf::new(CSRFConfig { disabled: false })
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

        match res.response.into_body() {
            ResponseBody::GraphQL(res) => {
                assert_eq!(res.errors[0].message, "This operation has been blocked as a potential Cross-Site Request Forgery (CSRF). \
                Please either specify a 'content-type' header (with a mime-type that is not one of application/x-www-form-urlencoded,multipart/form-data,text/plain) \
                or provide a header such that the request is preflighted: https://developer.mozilla.org/en-US/docs/Web/HTTP/CORS#simple_requests");
            }
            other => panic!("expected graphql response, found {:?}", other),
        }
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

        let service_stack = Csrf::new(CSRFConfig { disabled: true })
            .await
            .unwrap()
            .router_service(mock.boxed());

        let non_preflighted_request = RouterRequest::fake_builder().build().unwrap();

        let res = service_stack
            .oneshot(non_preflighted_request)
            .await
            .unwrap();

        match res.response.into_body() {
            ResponseBody::GraphQL(res) => {
                assert_eq!(res.data.unwrap(), expected_response_data);
            }
            other => panic!("expected graphql response, found {:?}", other),
        }
    }
}
