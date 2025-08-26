//! Cross Site Request Forgery (CSRF) plugin.
use std::ops::ControlFlow;
use std::sync::Arc;

use http::HeaderMap;
use http::StatusCode;
use http::header;
use schemars::JsonSchema;
use serde::Deserialize;
use tower::BoxError;
use tower::ServiceBuilder;
use tower::ServiceExt;

use crate::layers::ServiceBuilderExt;
use crate::plugin::Plugin;
use crate::plugin::PluginInit;
use crate::register_plugin;
use crate::services::router;

/// CSRF protection configuration.
///
/// See <https://owasp.org/www-community/attacks/csrf> for an explanation on CSRF attacks.
#[derive(Deserialize, Debug, Clone, JsonSchema)]
#[serde(deny_unknown_fields)]
#[serde(default)]
pub(crate) struct CSRFConfig {
    /// The CSRF plugin is enabled by default.
    ///
    /// Setting `unsafe_disabled: true` *disables* CSRF protection.
    // TODO rename this to enabled. This is in line with the other plugins and will be less confusing.
    unsafe_disabled: bool,
    /// Override the headers to check for by setting
    /// custom_headers
    /// Note that if you set required_headers here,
    /// you may also want to have a look at your `CORS` configuration,
    /// and make sure you either:
    /// - did not set any `allow_headers` list (so it defaults to `mirror_request`)
    /// - added your required headers to the allow_headers list, as shown in the
    ///   `examples/cors-and-csrf/custom-headers.router.yaml` files.
    required_headers: Arc<Vec<String>>,
}

fn apollo_custom_preflight_headers() -> Arc<Vec<String>> {
    Arc::new(vec![
        "x-apollo-operation-name".to_string(),
        "apollo-require-preflight".to_string(),
    ])
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

    fn router_service(&self, service: router::BoxService) -> router::BoxService {
        if !self.config.unsafe_disabled {
            let required_headers = self.config.required_headers.clone();
            ServiceBuilder::new()
                .checkpoint(move |req: router::Request| {
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
                        let res = router::Response::infallible_builder()
                            .error(error)
                            .status_code(StatusCode::BAD_REQUEST)
                            .context(req.context)
                            .build();
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
fn is_preflighted(req: &router::Request, required_headers: &[String]) -> bool {
    let headers = req.router_request.headers();
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
    use http_body_util::BodyExt;
    use mime::APPLICATION_JSON;

    use super::*;
    use crate::graphql;
    use crate::plugins::test::PluginTestHarness;

    #[tokio::test]
    async fn it_lets_preflighted_request_pass_through() {
        let with_preflight_content_type = router::Request::fake_builder()
            .header(CONTENT_TYPE, APPLICATION_JSON.essence_str())
            .build()
            .unwrap();
        assert_accepted(
            include_str!("fixtures/default.router.yaml"),
            with_preflight_content_type,
        )
        .await;

        let with_preflight_header = router::Request::fake_builder()
            .header("apollo-require-preflight", "this-is-a-test")
            .build()
            .unwrap();
        assert_accepted(
            include_str!("fixtures/default.router.yaml"),
            with_preflight_header,
        )
        .await;
    }

    #[tokio::test]
    async fn it_rejects_preflighted_multipart_form_data() {
        let with_preflight_content_type = router::Request::fake_builder()
            .header(CONTENT_TYPE, "multipart/form-data; boundary=842705fe5c26bcc3-e1302903b7efd762-d3aeccc8154e83c9-2ac7e6d91c6a7fdc")
            .build()
            .unwrap();
        assert_rejected(
            include_str!("fixtures/default.router.yaml"),
            with_preflight_content_type,
        )
        .await;
    }

    #[tokio::test]
    async fn it_rejects_non_preflighted_headers_request() {
        let mut non_preflighted_request = router::Request::fake_builder().build().unwrap();
        // fake_builder defaults to `Content-Type: application/json`,
        // specifically to avoid the case we’re testing here.
        non_preflighted_request
            .router_request
            .headers_mut()
            .remove("content-type");
        assert_rejected(
            include_str!("fixtures/default.router.yaml"),
            non_preflighted_request,
        )
        .await
    }

    #[tokio::test]
    async fn it_rejects_non_preflighted_content_type_request() {
        let non_preflighted_request = router::Request::fake_builder()
            .header(CONTENT_TYPE, "text/plain")
            .build()
            .unwrap();
        assert_rejected(
            include_str!("fixtures/default.router.yaml"),
            non_preflighted_request,
        )
        .await;

        let non_preflighted_request = router::Request::fake_builder()
            .header(CONTENT_TYPE, "text/plain; charset=utf8")
            .build()
            .unwrap();
        assert_rejected(
            include_str!("fixtures/default.router.yaml"),
            non_preflighted_request,
        )
        .await;
    }

    #[tokio::test]
    async fn it_accepts_non_preflighted_headers_request_when_plugin_is_disabled() {
        let non_preflighted_request = router::Request::fake_builder().build().unwrap();
        assert_accepted(
            include_str!("fixtures/unsafe_disabled.router.yaml"),
            non_preflighted_request,
        )
        .await
    }

    #[tokio::test]
    async fn it_rejects_non_preflighted_headers_request_when_required_headers_are_not_present() {
        let non_preflighted_request = router::Request::fake_builder().build().unwrap();
        assert_rejected(
            include_str!("fixtures/required_headers.router.yaml"),
            non_preflighted_request,
        )
        .await
    }

    // Check that when the headers are present, the request is accepted
    #[tokio::test]
    async fn it_accepts_non_preflighted_headers_request_when_required_headers_are_present() {
        let non_preflighted_request = router::Request::fake_builder()
            .header("X-MY-CSRF-Token", "this-is-a-test")
            .build()
            .unwrap();
        assert_accepted(
            include_str!("fixtures/required_headers.router.yaml"),
            non_preflighted_request,
        )
        .await
    }

    async fn assert_accepted(config: &'static str, request: router::Request) {
        let plugin = PluginTestHarness::<Csrf>::builder()
            .config(config)
            .build()
            .await
            .expect("test harness");
        let router_service =
            plugin.router_service(|_r| async { router::Response::fake_builder().build() });
        let mut resp = router_service
            .call(request)
            .await
            .expect("expected response");

        let body = resp
            .response
            .body_mut()
            .collect()
            .await
            .expect("expected body");

        let response: graphql::Response = serde_json::from_slice(&body.to_bytes()).unwrap();
        assert_eq!(response.errors.len(), 0);
    }

    async fn assert_rejected(config: &'static str, request: router::Request) {
        let plugin = PluginTestHarness::<Csrf>::builder()
            .config(config)
            .build()
            .await
            .expect("test harness");
        let router_service =
            plugin.router_service(|_r| async { router::Response::fake_builder().build() });
        let mut resp = router_service
            .call(request)
            .await
            .expect("expected response");

        let body = resp
            .response
            .body_mut()
            .collect()
            .await
            .expect("expected body");

        let response: graphql::Response = serde_json::from_slice(&body.to_bytes()).unwrap();
        assert_eq!(response.errors.len(), 1);
        assert_eq!(
            response.errors[0]
                .extensions
                .get("code")
                .expect("error code")
                .as_str(),
            Some("CSRF_ERROR")
        );
    }
}
