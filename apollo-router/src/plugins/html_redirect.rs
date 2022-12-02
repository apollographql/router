use std::ops::ControlFlow;

use axum::response::Html;
use axum::response::IntoResponse;
use futures::prelude::*;
use http::HeaderMap;
use http_body::Body as _;
use hyper::Body;
use mediatype::names::HTML;
use mediatype::names::TEXT;
use mediatype::MediaType;
use mediatype::MediaTypeList;
use tower::BoxError;
use tower::ServiceBuilder;
use tower::ServiceExt;

use crate::configuration::Homepage;
use crate::configuration::Sandbox;
use crate::layers::ServiceBuilderExt;
use crate::plugin::Plugin;
use crate::plugin::PluginInit;
use crate::register_plugin;
use crate::services::router;

#[derive(Debug, Clone)]
struct RedirectHTML {}

// This plugin should be the last one to wrap the router services,
// since we want to redirect before
#[async_trait::async_trait]
impl Plugin for RedirectHTML {
    type Config = ();

    async fn new(_: PluginInit<Self::Config>) -> Result<Self, BoxError> {
        Ok(Self {})
    }

    fn router_service(&self, service: router::BoxService) -> router::BoxService {
        let homepage = Homepage::display_page();
        let sandbox = Sandbox::display_page();
        let homepage_enabled = true;
        let sandbox_enabled = false;

        // todo: get whether homepage or sandbox is enabled
        let maybe_response = if true {
            Some(Html(homepage).into_response())
        } else if false {
            Some(Html(sandbox).into_response())
        } else {
            None
        };

        let router_response: Option<router::Response> = maybe_response.map(|response| {
            response
                .map(|body| {
                    let mut body = Box::pin(body);
                    Body::wrap_stream(stream::poll_fn(move |ctx| body.as_mut().poll_data(ctx)))
                })
                .into()
        });

        ServiceBuilder::new()
            .checkpoint(move |req: router::Request| {
                if router_response.is_some() && prefers_html(req.router_request.headers()) {
                    Ok(ControlFlow::Break(router_response.expect("checked above")))
                } else {
                    Ok(ControlFlow::Continue(req))
                }
            })
            .service(service)
            .boxed()
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

register_plugin!("apollo", "redirect-html", RedirectHTML);
