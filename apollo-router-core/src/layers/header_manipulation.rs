use crate::layer::ConfigurableLayer;
use crate::{register_layer, SubgraphRequest};
use http::header::HeaderName;
use http::HeaderValue;
use serde::Deserialize;
use std::str::FromStr;
use std::task::Poll;
use tower::{BoxError, Layer, Service};
#[derive(Clone, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Operation {
    PropagateAll,
    Propagate {
        name: String,
        default_value: Option<String>,
    },
    Insert {
        name: String,
        value: String,
    },
    Remove(String),
}

#[derive(Deserialize)]
#[serde(transparent)]
pub struct Config {
    operations: Vec<Operation>,
}

register_layer!("headers", HeaderManipulationLayer);
impl ConfigurableLayer for HeaderManipulationLayer {
    type Config = Config;

    fn configure(&mut self, configuration: Self::Config) -> Result<(), BoxError> {
        self.operations = configuration.operations;
        Ok(())
    }
}

#[derive(Default)]
pub struct HeaderManipulationLayer {
    operations: Vec<Operation>,
}

impl HeaderManipulationLayer {
    pub fn new(operations: Vec<Operation>) -> Self {
        Self { operations }
    }
}

impl From<Operation> for HeaderManipulationLayer {
    fn from(operation: Operation) -> Self {
        HeaderManipulationLayer::new(vec![operation])
    }
}

impl<S> Layer<S> for HeaderManipulationLayer {
    type Service = HeaderManipulationService<S>;

    fn layer(&self, service: S) -> Self::Service {
        HeaderManipulationService {
            service,
            operations: self.operations.to_owned(),
        }
    }
}

pub struct HeaderManipulationService<S> {
    service: S,
    operations: Vec<Operation>,
}

impl<S> Service<SubgraphRequest> for HeaderManipulationService<S>
where
    S: Service<SubgraphRequest>,
{
    type Response = S::Response;
    type Error = S::Error;
    type Future = S::Future;

    fn poll_ready(&mut self, cx: &mut std::task::Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.service.poll_ready(cx)
    }

    fn call(&mut self, mut request: SubgraphRequest) -> Self::Future {
        {
            //Add the header to the request and pass it on to the service.
            let subgraph_request_headers = request.http_request.headers_mut();

            for operation in &self.operations {
                match operation {
                    Operation::PropagateAll => {
                        for (header_name, header_value) in request.context.request.headers() {
                            subgraph_request_headers.insert(header_name, header_value.clone());
                        }
                    }
                    Operation::Propagate {
                        name,
                        default_value: None,
                    } => {
                        if let Some(header) = request.context.request.headers().get(name) {
                            subgraph_request_headers.insert(
                                HeaderName::from_str(name.as_str()).unwrap(),
                                header.clone(),
                            );
                        }
                    }
                    Operation::Propagate {
                        name,
                        default_value: Some(default_value),
                    } => {
                        let name = HeaderName::from_str(name.as_str()).unwrap();
                        if let Some(header) = request.context.request.headers().get(&name) {
                            subgraph_request_headers.insert(name, header.clone());
                        } else {
                            subgraph_request_headers.insert(
                                name,
                                HeaderValue::from_str(default_value.as_str()).unwrap(),
                            );
                        }
                    }
                    Operation::Insert { name, value } => {
                        let name = HeaderName::from_str(name.as_str()).unwrap();
                        subgraph_request_headers
                            .insert(name, HeaderValue::from_str(value.as_str()).unwrap());
                    }
                    Operation::Remove(header_name) => {
                        subgraph_request_headers.remove(header_name);
                    }
                }
            }
        }

        self.service.call(request)
    }
}
