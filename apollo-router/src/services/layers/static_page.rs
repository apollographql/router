//!  (A)utomatic (P)ersisted (Q)ueries cache.
//!
//!  For more information on APQ see:
//!  <https://www.apollographql.com/docs/apollo-server/performance/apq/>

use std::ops::ControlFlow;

use askama::Template;
use bytes::Bytes;
use http::header::CONTENT_TYPE;
use http::HeaderMap;
use http::HeaderValue;
use http::Method;
use hyper::Body;
use mediatype::names::HTML;
use mediatype::names::TEXT;
use mediatype::MediaType;
use mediatype::MediaTypeList;
use tower::BoxError;
use tower::Layer;
use tower::Service;

use crate::configuration::Homepage;
use crate::layers::sync_checkpoint::CheckpointService;
use crate::services::router;
use crate::Configuration;

/// [`Layer`] That serves Static pages such as Homepage and Sandbox.
#[derive(Clone)]
pub(crate) struct StaticPageLayer {
    static_page: Option<Bytes>,
}

impl StaticPageLayer {
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
                        && prefers_html(req.router_request.headers())
                    {
                        let response = http::Response::builder()
                            .header(
                                CONTENT_TYPE,
                                HeaderValue::from_static(mime::TEXT_HTML_UTF_8.as_ref()),
                            )
                            .body(Body::from(page.clone()))
                            .unwrap();
                        ControlFlow::Break(router::Response {
                            response,
                            context: req.context,
                        })
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

fn prefers_html(headers: &HeaderMap) -> bool {
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

#[derive(Template)]
#[template(path = "sandbox_index.html")]
struct SandboxTemplate {
    apollo_router_version: &'static str,
}

pub(crate) fn sandbox_page_content() -> Vec<u8> {
    let template = SandboxTemplate {
        apollo_router_version: std::env!("CARGO_PKG_VERSION"),
    };
    let mut buffer = Vec::new();
    template.write_into(&mut buffer).expect("cannot fail");
    buffer
}

#[derive(Template)]
#[template(path = "homepage_index.html")]
struct HomepageTemplate {
    graph_ref: String,
}

pub(crate) fn home_page_content(homepage_config: &Homepage) -> Vec<u8> {
    let template = HomepageTemplate {
        graph_ref: homepage_config
            .graph_ref
            .as_ref()
            .cloned()
            .unwrap_or_default(),
    };
    let mut buffer = Vec::new();
    template.write_into(&mut buffer).expect("cannot fail");
    buffer
}
