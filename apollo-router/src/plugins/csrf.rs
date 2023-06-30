//! Cross Site Request Forgery (CSRF) plugin.
use std::ops::ControlFlow;

use http::header;
use http::HeaderMap;
use http::StatusCode;
use schemars::JsonSchema;
use serde::Deserialize;
use tower::BoxError;
use tower::ServiceBuilder;
use tower::ServiceExt;

use crate::layers::ServiceBuilderExt;
use crate::plugin::Plugin;
use crate::plugin::PluginInit;
use crate::register_plugin;
use crate::services::supergraph;
use crate::services::SupergraphResponse;

/// CSRF Configuration.
#[derive(Deserialize, Debug, Clone, JsonSchema)]
#[serde(deny_unknown_fields)]
#[serde(default)]
pub(crate) struct CSRFConfig {
    /// The CSRF plugin is enabled by default;
    /// set unsafe_disabled = true to disable the plugin behavior
    /// Note that setting this to true is deemed unsafe.
    /// See <https://developer.mozilla.org/en-US/docs/Glossary/CSRF>.
    unsafe_disabled: bool,
    /// Override the headers to check for by setting
    /// custom_headers
    /// Note that if you set required_headers here,
    /// you may also want to have a look at your `CORS` configuration,
    /// and make sure you either:
    /// - did not set any `allow_headers` list (so it defaults to `mirror_request`)
    /// - added your required headers to the allow_headers list, as shown in the
    /// `examples/cors-and-csrf/custom-headers.router.yaml` files.
    required_headers: Vec<String>,
}

fn apollo_custom_preflight_headers() -> Vec<String> {
    vec![
        "x-apollo-operation-name".to_string(),
        "apollo-require-preflight".to_string(),
    ]
}

impl Default for CSRFConfig {
    fn default() -> Self {
        Self {
            unsafe_disabled: false,
            required_headers: apollo_custom_preflight_headers(),
        }
    }
}

static NON_PREFLIGHTED_CONTENT_TYPES: &[&str] = &[
    "application/x-www-form-urlencoded",
    "multipart/form-data",
    "text/plain",
];

/// The Csrf plugin makes sure any request received would have been preflighted if it was sent by a browser.
///
/// Quoting the [great apollo server comment](
/// https://github.com/apollographql/apollo-server/blob/12bf5fc8ef305caa6a8848e37f862d32dae5957f/packages/server/src/preventCsrf.ts#L26):
///
/// We don't want random websites to be able to execute actual GraphQL operations
/// from a user's browser unless our CORS policy supports it. It's not good
/// enough just to ensure that the browser can't read the response from the
/// operation; we also want to prevent CSRF, where the attacker can cause side
/// effects with an operation or can measure the timing of a read operation. Our
/// goal is to ensure that we don't run the context function or execute the
/// GraphQL operation until the browser has evaluated the CORS policy, which
/// means we want all operations to be pre-flighted. We can do that by only
/// processing operations that have at least one header set that appears to be
/// manually set by the JS code rather than by the browser automatically.
///
/// POST requests generally have a content-type `application/json`, which is
/// sufficient to trigger preflighting. So we take extra care with requests that
/// specify no content-type or that specify one of the three non-preflighted
/// content types. For those operations, we require (if this feature is enabled)
/// one of a set of specific headers to be set. By ensuring that every operation
/// either has a custom content-type or sets one of these headers, we know we
/// won't execute operations at the request of origins who our CORS policy will
/// block.
#[derive(Debug, Clone)]
pub(crate) struct Csrf {
    config: CSRFConfig,
}

#[async_trait::async_trait]
impl Plugin for Csrf {
    type Config = CSRFConfig;

    async fn new(init: PluginInit<Self::Config>) -> Result<Self, BoxError> {
        Ok(Csrf {
            config: init.config,
        })
    }

    fn supergraph_service(&self, service: supergraph::BoxService) -> supergraph::BoxService {
        if !self.config.unsafe_disabled {
            let required_headers = self.config.required_headers.clone();
            ServiceBuilder::new()
                .checkpoint(move |req: supergraph::Request| {
                    if is_preflighted(&req, required_headers.as_slice()) {
                        tracing::trace!("request is preflighted");
                        Ok(ControlFlow::Continue(req))
                    } else {
                        tracing::trace!("request is not preflighted");
                        let error = crate::error::Error::builder().message(
                            format!(
                                "This operation has been blocked as a potential Cross-Site Request Forgery (CSRF). \
                                Please either specify a 'content-type' header (with a mime-type that is not one of {}) \
                                or provide one of the following headers: {}", 
                                NON_PREFLIGHTED_CONTENT_TYPES.join(", "),
                                required_headers.join(", ")
                            ))
                            .extension_code("CSRF_ERROR")
                            .build();
                        let res = SupergraphResponse::builder()
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

// A `preflighted` request is the opposite of a `simple` request.
//
// A simple request is a request that satisfies the three predicates below:
// - Has method `GET` `POST` or `HEAD` (which turns out to be the three methods our web server allows)
// - If content-type is set, it must be with a mime type that is application/x-www-form-urlencoded OR multipart/form-data OR text/plain
// - The only headers added by javascript code are part of the cors safelisted request headers (Accept,Accept-Language,Content-Language,Content-Type, and simple Range
//
// Given the first step is covered in our web browser, we'll take care of the two other steps below:
fn is_preflighted(req: &supergraph::Request, required_headers: &[String]) -> bool {
    let headers = req.supergraph_request.headers();
    content_type_requires_preflight(headers)
        || recommended_header_is_provided(headers, required_headers)
}

// Part two of the algorithm above:
// If content-type is set, it must be with a mime type that is application/x-www-form-urlencoded OR multipart/form-data OR text/plain
// The details of the algorithm are covered in the fetch specification https://fetch.spec.whatwg.org/#cors-safelisted-request-header
//
// content_type_requires_preflight will thus return true if
// the header value is !(`application/x-www-form-urlencoded` || `multipart/form-data` || `text/plain`)
fn content_type_requires_preflight(headers: &HeaderMap) -> bool {
    let joined_content_type_header_value = if let Ok(combined_headers) = headers
        .get_all(header::CONTENT_TYPE)
        .iter()
        .map(|header_value| {
            // The mime type parser we're using is a bit askew,
            // so we're going to perform a bit of trimming, and character replacement
            // before we combine the header values
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

    if let Ok(mime_type) = joined_content_type_header_value.parse::<mime::Mime>() {
        !NON_PREFLIGHTED_CONTENT_TYPES.contains(&mime_type.essence_str())
    } else {
        // If we get here, this means that we couldn't parse the content-type value into
        // a valid mime type... which would be safe enough for us to assume preflight was triggered if the `mime`
        // crate followed the fetch specification, but it unfortunately doesn't (see comment above).
        //
        // Better safe than sorry, we will claim we don't have solid enough reasons
        // to believe the request will have triggered preflight
        false
    }
}

// Part three of the algorithm described above:
// The only headers added by javascript code are part of the cors safelisted request headers (Accept,Accept-Language,Content-Language,Content-Type, and simple Range
//
// It would be pretty hard for us to keep track of the headers browser send themselves,
// and the ones that were explicitely added by a javascript client (and have thus triggered preflight).
// so we will do the oposite:
// We hereby challenge any client to provide one of the required_headers.
// Browsers definitely will not add any "x-apollo-operation-name" or "apollo-require-preflight" to every request anytime soon,
// which means if the header is present, javascript has added it, and the browser will have triggered preflight.
fn recommended_header_is_provided(headers: &HeaderMap, required_headers: &[String]) -> bool {
    required_headers
        .iter()
        .any(|header| headers.get(header).is_some())
}

register_plugin!("apollo", "csrf", Csrf);

#[cfg(test)]
mod csrf_tests {
    use crate::plugin::PluginInit;
    #[tokio::test]
    async fn plugin_registered() {
        crate::plugin::plugins()
            .find(|factory| factory.name == "apollo.csrf")
            .expect("Plugin not found")
            .create_instance_without_schema(&serde_json::json!({ "unsafe_disabled": true }))
            .await
            .unwrap();

        crate::plugin::plugins()
            .find(|factory| factory.name == "apollo.csrf")
            .expect("Plugin not found")
            .create_instance_without_schema(&serde_json::json!({}))
            .await
            .unwrap();
    }

    use http::header::CONTENT_TYPE;
    use mime::APPLICATION_JSON;
    use serde_json_bytes::json;
    use tower::ServiceExt;

    use super::*;
    use crate::plugin::test::MockSupergraphService;

    #[tokio::test]
    async fn it_lets_preflighted_request_pass_through() {
        let config = CSRFConfig::default();
        let with_preflight_content_type = supergraph::Request::fake_builder()
            .header(CONTENT_TYPE, APPLICATION_JSON.essence_str())
            .build()
            .unwrap();
        assert_accepted(config.clone(), with_preflight_content_type).await;

        let with_preflight_header = supergraph::Request::fake_builder()
            .header("apollo-require-preflight", "this-is-a-test")
            .build()
            .unwrap();
        assert_accepted(config, with_preflight_header).await;
    }

    #[tokio::test]
    async fn it_rejects_non_preflighted_headers_request() {
        let config = CSRFConfig::default();
        let mut non_preflighted_request = supergraph::Request::fake_builder().build().unwrap();
        // fake_builder defaults to `Content-Type: application/json`,
        // specifically to avoid the case we’re testing here.
        non_preflighted_request
            .supergraph_request
            .headers_mut()
            .remove("content-type");
        assert_rejected(config, non_preflighted_request).await
    }

    #[tokio::test]
    async fn it_rejects_non_preflighted_content_type_request() {
        let config = CSRFConfig::default();
        let non_preflighted_request = supergraph::Request::fake_builder()
            .header(CONTENT_TYPE, "text/plain")
            .build()
            .unwrap();
        assert_rejected(config.clone(), non_preflighted_request).await;

        let non_preflighted_request = supergraph::Request::fake_builder()
            .header(CONTENT_TYPE, "text/plain; charset=utf8")
            .build()
            .unwrap();
        assert_rejected(config, non_preflighted_request).await;
    }

    #[tokio::test]
    async fn it_accepts_non_preflighted_headers_request_when_plugin_is_disabled() {
        let config = CSRFConfig {
            unsafe_disabled: true,
            ..Default::default()
        };
        let non_preflighted_request = supergraph::Request::fake_builder().build().unwrap();
        assert_accepted(config, non_preflighted_request).await
    }

    async fn assert_accepted(config: CSRFConfig, request: supergraph::Request) {
        let mut mock_service = MockSupergraphService::new();
        mock_service.expect_call().times(1).returning(move |_| {
            Ok(SupergraphResponse::fake_builder()
                .data(json!({ "test": 1234_u32 }))
                .build()
                .unwrap())
        });

        let service_stack = Csrf::new(PluginInit::fake_new(config, Default::default()))
            .await
            .unwrap()
            .supergraph_service(mock_service.boxed());
        let res = service_stack
            .oneshot(request)
            .await
            .unwrap()
            .next_response()
            .await
            .unwrap();

        assert_eq!(res.errors, []);
        assert_eq!(res.data.unwrap(), json!({ "test": 1234_u32 }));
    }

    async fn assert_rejected(config: CSRFConfig, request: supergraph::Request) {
        let service_stack = Csrf::new(PluginInit::fake_new(config, Default::default()))
            .await
            .unwrap()
            .supergraph_service(MockSupergraphService::new().boxed());
        let res = service_stack
            .oneshot(request)
            .await
            .unwrap()
            .next_response()
            .await
            .unwrap();

        assert_eq!(
            1,
            res.errors.len(),
            "expected one(1) error in the SupergraphResponse, found {}\n{:?}",
            res.errors.len(),
            res.errors
        );
        assert_eq!(res.errors[0].message, "This operation has been blocked as a potential Cross-Site Request Forgery (CSRF). \
                Please either specify a 'content-type' header \
                (with a mime-type that is not one of application/x-www-form-urlencoded, multipart/form-data, text/plain) \
                or provide one of the following headers: x-apollo-operation-name, apollo-require-preflight");
    }
}
