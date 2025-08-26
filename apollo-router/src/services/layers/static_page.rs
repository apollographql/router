//! Provides the home page and sandbox page implementations.

use std::ops::ControlFlow;

use bytes::Bytes;
use http::HeaderMap;
use http::HeaderValue;
use http::Method;
use http::header::CONTENT_TYPE;
use mediatype::MediaType;
use mediatype::MediaTypeList;
use mediatype::names::HTML;
use mediatype::names::TEXT;
use tower::BoxError;
use tower::Layer;
use tower::Service;

use crate::Configuration;
use crate::configuration::Homepage;
use crate::layers::sync_checkpoint::CheckpointService;
use crate::services::router;

/// A layer that serves a static page for all requests that accept a `text/html` response
/// (typically a user navigating to a page in the browser).
#[derive(Clone)]
pub(crate) struct StaticPageLayer {
    static_page: Option<Bytes>,
}

impl StaticPageLayer {
    /// Create a static page based on configuration: either an Apollo Sandbox, or a simple home
    /// page.
    pub(crate) fn new(configuration: &Configuration) -> Self {
        let static_page = if configuration.sandbox.enabled {
            Some(Bytes::from(sandbox_page_content()))
        } else if configuration.homepage.enabled {
            Some(Bytes::from(home_page_content(&configuration.homepage)))
        } else {
            None
        };

        Self { static_page }
    }
}

impl<S> Layer<S> for StaticPageLayer
where
    S: Service<router::Request, Response = router::Response, Error = BoxError> + Send + 'static,
    <S as Service<router::Request>>::Future: Send + 'static,
{
    type Service = CheckpointService<S, router::Request>;

    fn layer(&self, service: S) -> Self::Service {
        if let Some(static_page) = &self.static_page {
            let page = static_page.clone();

            CheckpointService::new(
                move |req| {
                    let res = if req.router_request.method() == Method::GET
                        && accepts_html(req.router_request.headers())
                    {
                        ControlFlow::Break(
                            router::Response::http_response_builder()
                                .response(
                                    http::Response::builder()
                                        .header(
                                            CONTENT_TYPE,
                                            HeaderValue::from_static(
                                                mime::TEXT_HTML_UTF_8.as_ref(),
                                            ),
                                        )
                                        .body(router::body::from_bytes(page.clone()))
                                        .unwrap(),
                                )
                                .context(req.context)
                                .build()
                                .unwrap(),
                        )
                    } else {
                        ControlFlow::Continue(req)
                    };

                    Ok(res)
                },
                service,
            )
        } else {
            CheckpointService::new(move |req| Ok(ControlFlow::Continue(req)), service)
        }
    }
}

/// Returns true if the given header map contains an `Accept` header which contains the "text/html"
/// MIME type.
///
/// `Accept` priorities or preferences are not considered.
fn accepts_html(headers: &HeaderMap) -> bool {
    let text_html = MediaType::new(TEXT, HTML);

    headers.get_all(&http::header::ACCEPT).iter().any(|value| {
        value
            .to_str()
            .map(|accept_str| {
                let mut list = MediaTypeList::new(accept_str);

                list.any(|mime| mime.as_ref() == Ok(&text_html))
            })
            .unwrap_or(false)
    })
}

pub(crate) fn sandbox_page_content() -> Vec<u8> {
    const TEMPLATE: &str = include_str!("../../../templates/sandbox_index.html");
    TEMPLATE
        .replace("{{APOLLO_ROUTER_VERSION}}", std::env!("CARGO_PKG_VERSION"))
        .into_bytes()
}

pub(crate) fn home_page_content(homepage_config: &Homepage) -> Vec<u8> {
    const TEMPLATE: &str = include_str!("../../../templates/homepage_index.html");
    let graph_ref = serde_json::to_string(&homepage_config.graph_ref).expect("cannot fail");
    TEMPLATE.replace("{{GRAPH_REF}}", &graph_ref).into_bytes()
}
