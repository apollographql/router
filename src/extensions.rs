

use std::sync::Arc;
use async_trait::async_trait;
// Extensions will use chaining rather than than callbacks for start and end.
// Callbacks have not been found to be a good fit to thread local based stuff such as OTel.

use crate::{Configuration, DownstreamRequestChain, ExtensionManager, QueryPlan, Request, Response, Schema};

// Why have this at all when users can compose via RouterFactory?
//
struct Chain {}

impl Chain {
    pub async fn validate_response(&self, _response: Response) {
        todo!()
    }
    pub async fn make_request(&self, _upstream_request: Request, _downstream_request: Request) -> Response {
        todo!()
    }
    pub async fn plan_query(&self, _request: Request) -> QueryPlan {
        todo!()
    }
    pub async fn schema_read(&self) -> Schema {
        todo!()
    }

    pub async fn visit_query(&self) {}
}


#[async_trait]
trait Extension: Send + Sync {
    async fn configure(&self, _configuration: Arc<dyn Configuration>) {}
    async fn schema_read(&self, chain: Chain) -> Schema {
        chain.schema_read().await
    }
    async fn plan_query(&self, chain: Chain, upstream_request: Request) -> QueryPlan {
        chain.plan_query(upstream_request).await
    }
    async fn visit_query(&self, chain: Chain) -> () {
        chain.visit_query().await
    }
    async fn make_downstream_request(&self, chain: DownstreamRequestChain, _upstream_request: &Request, downstream_request: Request) -> Response {

        chain(downstream_request).await

    }
    async fn validate_response(&self, chain: Chain, response: Response) {
        chain.validate_response(response).await
    }
}

struct HeadersExtension {}

#[async_trait]
impl Extension for HeadersExtension {
    async fn make_downstream_request(&self, chain: DownstreamRequestChain, upstream_request: &Request, mut downstream_request: Request) -> Response {
        downstream_request.set_header("A", upstream_request.get_header("A"));
        chain(downstream_request).await
    }
}

struct OtelExtension {}
//
// #[async_trait]
// impl Extension for OtelExtension {
//     async fn configure(&self, configuration: Arc<dyn Configuration>) {
//         //Load config
//     }
//
//     async fn make_downstream_request(&self, chain: DownstreamRequestChain, upstream_request: Request, mut downstream_request:Request) -> Response {
//         //chain(downstream_request).await
//     }
// }
//
// struct NewRelicExtension {
//
// }
//
//
// #[async_trait]
// impl Extension for NewRelicExtension {
//
//     async fn configure(&self, configuration: Arc<dyn Configuration>) {
//         //Load config
//     }
//
//     async fn make_downstream_request(&self, chain: DownstreamRequestChain, upstream_request: Request, mut downstream_request:Request) -> Response {
//         chain(downstream_request).await
//     }
// }


pub struct DefaultExtensionManager {
    extensions: Vec<Box<dyn Extension>>,
}

impl DefaultExtensionManager {
    pub fn new(_config: Arc<dyn Configuration>) -> Self {
        println!("Creating DefaultExtensions");
        Self {
            extensions: Vec::default()
        }
    }
}


#[async_trait]
impl ExtensionManager for DefaultExtensionManager {
    async fn do_make_downstream_request(&self, upstream_request: Request, downstream_request: Request, delegate: DownstreamRequestChain) -> Response {
        let chain = self.extensions.iter().fold(delegate, |next, &extension| {
            let next: DownstreamRequestChain = Box::pin(|downstream_request| {
                let f = extension.make_downstream_request(next, &upstream_request, downstream_request);
                Box::pin(f)
            });
            next
        });
        chain(downstream_request).await
    }
}
