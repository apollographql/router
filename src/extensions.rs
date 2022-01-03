use std::sync::Arc;
use std::thread::sleep;
use std::time::Duration;
use anyhow::anyhow;
use async_trait::async_trait;
use anyhow::Result;
use opentelemetry::Context;
use opentelemetry::trace::FutureExt;

use crate::{Chain, Configuration, Extension, QueryPlan, Request, Response, Schema};


pub struct HeadersExtension;

#[async_trait]
impl Extension for HeadersExtension {
    async fn make_downstream_request(&self, chain: Chain, upstream_request: &Request, mut downstream_request: Request) -> Result<Response> {
        //Propagate a header if it exists
        upstream_request.get_header("A").and_then(|h|downstream_request.set_header("A", h));

        let result = chain.make_downstream_request(upstream_request, downstream_request).await;

        //Our extension has special handling if there was an error downstream
        if result.is_err() {
            return Ok(Response {
                headers: Default::default(),
                body: format!("Got error {}", result.err().unwrap())
            })
        }
        result
    }
}


pub struct SecurityExtension;
#[async_trait]
impl Extension for SecurityExtension {
    async fn make_downstream_request(&self, chain: Chain, upstream_request: &Request, downstream_request: Request) -> Result<Response> {
        //Only make the request if the header is present
        if let Some(_header) = upstream_request.get_header("A") {
            return chain.make_downstream_request(upstream_request, downstream_request).await
        }
        Err(anyhow!("Missing header: A"))
    }
}

pub struct RetryExtension;
#[async_trait]
impl Extension for RetryExtension {
    async fn make_downstream_request(&self, chain: Chain, upstream_request: &Request, downstream_request: Request) -> Result<Response> {

        for _i in [0..10] {
            if let Ok(response) = chain.make_downstream_request(upstream_request, downstream_request.clone()).await {
                return Ok(response);
            }
            sleep(Duration::from_millis(1000))
        }

        Err(anyhow!("Failed"))
    }
}


pub struct OtelExtension;
#[async_trait]
impl Extension for OtelExtension {
    async fn make_downstream_request(&self, chain: Chain, upstream_request: &Request, downstream_request: Request) -> Result<Response> {
        //Add to the current context
        //This context may be picked up later
        let my_cx = Context::current_with_value(3);
        chain.make_downstream_request(upstream_request, downstream_request.clone()).with_context(my_cx).await
        //Context is dropped
    }
}

pub struct WasmExtension;
#[async_trait]
impl Extension for WasmExtension {
    async fn configure(&self, _configuration: Arc<dyn Configuration>) {
        todo!()
    }

    async fn schema_read(&self, chain: Chain) -> Result<Schema> {
        todo!()
    }

    async fn plan_query(&self, chain: Chain, upstream_request: Request) -> Result<QueryPlan> {
        todo!()
    }

    async fn make_downstream_request(&self, chain: Chain, upstream_request: &Request, downstream_request: Request) -> Result<Response> {
        todo!();
    }

    async fn validate_response(&self, chain: Chain, response: Response) -> Result<Response> {
        todo!()
    }
}





