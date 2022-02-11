use crate::layer::ConfigurableLayer;
use crate::{register_layer, SubgraphRequest};
use http::header::HeaderName;
use http::HeaderValue;
use schemars::gen::SchemaGenerator;
use schemars::schema::Schema;
use schemars::{schema_for, JsonSchema};
use serde::Deserialize;
use std::str::FromStr;
use std::task::Poll;
use tower::{BoxError, Layer, Service};

#[derive(Clone, Deserialize)]
#[serde(try_from = "OperationDef")]
pub enum Operation {
    PropagateAll,
    Propagate {
        name: HeaderName,
        default_value: Option<HeaderValue>,
    },
    Insert {
        name: HeaderName,
        value: HeaderValue,
    },
    Remove(HeaderName),
}

// Proxy the schema to the mirror type.
impl JsonSchema for Operation {
    fn schema_name() -> String {
        "Operation".to_string()
    }

    fn json_schema(_gen: &mut SchemaGenerator) -> Schema {
        schema_for!(OperationDef).schema.into()
    }
}

// Mirror for deserializing operation.
// HeaderName and HeaderValue do no implement Deserialize. So this type is used instead in
// combination with `try_from`
// This type also enables us to support `JsonSchema` derive.
#[derive(Clone, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum OperationDef {
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

impl TryFrom<OperationDef> for Operation {
    type Error = BoxError;

    fn try_from(value: OperationDef) -> Result<Self, Self::Error> {
        match value {
            OperationDef::PropagateAll => Ok(Operation::PropagateAll),
            OperationDef::Propagate {
                name,
                default_value,
            } => Ok(Operation::Propagate {
                name: HeaderName::from_str(name.as_str())?,
                default_value: match default_value {
                    Some(value) => Some(HeaderValue::from_str(value.as_str())?),
                    None => None,
                },
            }),
            OperationDef::Insert { name, value } => Ok(Operation::Insert {
                name: HeaderName::from_str(name.as_str())?,
                value: HeaderValue::from_str(value.as_str())?,
            }),
            OperationDef::Remove(name) => {
                Ok(Operation::Remove(HeaderName::from_str(name.as_str())?))
            }
        }
    }
}

#[derive(Deserialize, JsonSchema)]
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

#[derive(Default, JsonSchema)]
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
                        for (name, value) in request.context.request.headers() {
                            subgraph_request_headers.insert(name, value.clone());
                        }
                    }
                    Operation::Propagate {
                        name,
                        default_value: None,
                    } => {
                        if let Some(header) = request.context.request.headers().get(name) {
                            subgraph_request_headers.insert(name, header.clone());
                        }
                    }
                    Operation::Propagate {
                        name,
                        default_value: Some(default_value),
                    } => {
                        if let Some(header) = request.context.request.headers().get(name) {
                            subgraph_request_headers.insert(name, header.clone());
                        } else {
                            subgraph_request_headers.insert(name, default_value.clone());
                        }
                    }
                    Operation::Insert { name, value } => {
                        subgraph_request_headers.insert(name, value.clone());
                    }
                    Operation::Remove(name) => {
                        subgraph_request_headers.remove(name);
                    }
                }
            }
        }

        self.service.call(request)
    }
}

#[cfg(test)]
mod test {
    use crate::header_manipulation::Operation;
    use serde_json::json;

    #[test]
    fn test_invalid_header() {
        assert_eq!(
            serde_json::from_value::<Operation>(json! (
                {"propagate": {"name": "f\n"}}
            ))
            .err()
            .unwrap()
            .to_string(),
            "invalid HTTP header name"
        );
    }

    #[test]
    fn test_valid_header() {
        assert!(serde_json::from_value::<Operation>(json! (
            {"propagate": {"name": "f"}}
        ))
        .is_ok());
    }
}
