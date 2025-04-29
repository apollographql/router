use derivative::Derivative;
use opentelemetry::Array;
use opentelemetry::StringValue;
use opentelemetry::Value;
use schemars::JsonSchema;
use serde::Deserialize;
use tower::BoxError;

use crate::Context;
use crate::plugins::connectors::handle_responses::MappedResponse;
use crate::plugins::telemetry::config::AttributeValue;
use crate::plugins::telemetry::config_new::Selector;
use crate::plugins::telemetry::config_new::Stage;
use crate::plugins::telemetry::config_new::connector::ConnectorRequest;
use crate::plugins::telemetry::config_new::connector::ConnectorResponse;
use crate::plugins::telemetry::config_new::instruments::InstrumentValue;
use crate::plugins::telemetry::config_new::instruments::Standard;
use crate::plugins::telemetry::config_new::selectors::ErrorRepr;
use crate::plugins::telemetry::config_new::selectors::ResponseStatus;
use crate::services::connector::request_service::TransportRequest;
use crate::services::connector::request_service::TransportResponse;

#[derive(Deserialize, JsonSchema, Clone, Debug, PartialEq)]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
pub(crate) enum ConnectorSource {
    /// The name of the connector source.
    Name,
}

#[derive(Deserialize, JsonSchema, Clone, Debug)]
#[serde(deny_unknown_fields, rename_all = "snake_case", untagged)]
pub(crate) enum ConnectorValue {
    Standard(Standard),
    Custom(ConnectorSelector),
}

impl From<&ConnectorValue> for InstrumentValue<ConnectorSelector> {
    fn from(value: &ConnectorValue) -> Self {
        match value {
            ConnectorValue::Standard(s) => InstrumentValue::Standard(s.clone()),
            ConnectorValue::Custom(selector) => InstrumentValue::Custom(selector.clone()),
        }
    }
}

#[derive(Deserialize, JsonSchema, Clone, Debug, PartialEq)]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
pub(crate) enum MappingProblems {
    Problems,
    Count,
}

#[derive(Deserialize, JsonSchema, Clone, Derivative)]
#[serde(deny_unknown_fields, rename_all = "snake_case", untagged)]
#[derivative(Debug, PartialEq)]
pub(crate) enum ConnectorSelector {
    SubgraphName {
        /// The subgraph name
        subgraph_name: bool,
    },
    ConnectorSource {
        /// The connector source.
        connector_source: ConnectorSource,
    },
    HttpRequestHeader {
        /// The name of a connector HTTP request header.
        connector_http_request_header: String,
        #[serde(skip)]
        #[allow(dead_code)]
        /// Optional redaction pattern.
        redact: Option<String>,
        /// Optional default value.
        default: Option<String>,
    },
    ConnectorResponseHeader {
        /// The name of a connector HTTP response header.
        connector_http_response_header: String,
        #[serde(skip)]
        #[allow(dead_code)]
        /// Optional redaction pattern.
        redact: Option<String>,
        /// Optional default value.
        default: Option<String>,
    },
    ConnectorResponseStatus {
        /// The connector HTTP response status code.
        connector_http_response_status: ResponseStatus,
    },
    ConnectorHttpMethod {
        /// The connector HTTP method.
        connector_http_method: bool,
    },
    ConnectorUrlTemplate {
        /// The connector URL template.
        connector_url_template: bool,
    },
    StaticField {
        /// A static value
        r#static: AttributeValue,
    },
    Error {
        /// Critical error if it happens
        error: ErrorRepr,
    },
    RequestMappingProblems {
        /// Request mapping problems, if any
        connector_request_mapping_problems: MappingProblems,
    },
    ResponseMappingProblems {
        /// Response mapping problems, if any
        connector_response_mapping_problems: MappingProblems,
    },
}

impl Selector for ConnectorSelector {
    type Request = ConnectorRequest;
    type Response = ConnectorResponse;
    type EventResponse = ();

    fn on_request(&self, request: &Self::Request) -> Option<Value> {
        match self {
            ConnectorSelector::SubgraphName { subgraph_name } if *subgraph_name => Some(
                opentelemetry::Value::from(request.connector.id.subgraph_name.clone()),
            ),
            ConnectorSelector::ConnectorSource { .. } => request
                .connector
                .id
                .source_name
                .as_ref()
                .cloned()
                .map(opentelemetry::Value::from),
            ConnectorSelector::ConnectorHttpMethod {
                connector_http_method,
            } if *connector_http_method => Some(opentelemetry::Value::from(
                request.connector.transport.method.as_str().to_string(),
            )),
            ConnectorSelector::ConnectorUrlTemplate {
                connector_url_template,
            } if *connector_url_template => Some(opentelemetry::Value::from(
                request.connector.transport.connect_template.to_string(),
            )),
            ConnectorSelector::HttpRequestHeader {
                connector_http_request_header: connector_request_header,
                default,
                ..
            } => {
                let TransportRequest::Http(ref http_request) = request.transport_request;
                http_request
                    .inner
                    .headers()
                    .get(connector_request_header)
                    .and_then(|h| Some(h.to_str().ok()?.to_string()))
                    .or_else(|| default.clone())
                    .map(opentelemetry::Value::from)
            }
            ConnectorSelector::RequestMappingProblems {
                connector_request_mapping_problems: mapping_problems,
            } => match mapping_problems {
                MappingProblems::Problems => Some(Value::Array(Array::String(
                    request
                        .mapping_problems
                        .iter()
                        .filter_map(|problem| {
                            serde_json::to_string(problem).ok().map(StringValue::from)
                        })
                        .collect(),
                ))),
                MappingProblems::Count => Some(Value::I64(
                    request
                        .mapping_problems
                        .iter()
                        .map(|problem| problem.count as i64)
                        .sum(),
                )),
            },
            ConnectorSelector::StaticField { r#static } => Some(r#static.clone().into()),
            _ => None,
        }
    }

    fn on_response(&self, response: &Self::Response) -> Option<Value> {
        match self {
            ConnectorSelector::ConnectorResponseHeader {
                connector_http_response_header: connector_response_header,
                default,
                ..
            } => {
                if let Ok(TransportResponse::Http(ref http_response)) = response.transport_result {
                    http_response
                        .inner
                        .headers
                        .get(connector_response_header)
                        .and_then(|h| Some(h.to_str().ok()?.to_string()))
                        .or_else(|| default.clone())
                        .map(opentelemetry::Value::from)
                } else {
                    None
                }
            }
            ConnectorSelector::ConnectorResponseStatus {
                connector_http_response_status: response_status,
            } => {
                if let Ok(TransportResponse::Http(ref http_response)) = response.transport_result {
                    let status = http_response.inner.status;
                    match response_status {
                        ResponseStatus::Code => Some(Value::I64(status.as_u16() as i64)),
                        ResponseStatus::Reason => {
                            status.canonical_reason().map(|reason| reason.into())
                        }
                    }
                } else {
                    None
                }
            }
            ConnectorSelector::ResponseMappingProblems {
                connector_response_mapping_problems: mapping_problems,
            } => {
                if let MappedResponse::Data { ref problems, .. } = response.mapped_response {
                    match mapping_problems {
                        MappingProblems::Problems => Some(Value::Array(Array::String(
                            problems
                                .iter()
                                .filter_map(|problem| {
                                    serde_json::to_string(problem).ok().map(StringValue::from)
                                })
                                .collect(),
                        ))),
                        MappingProblems::Count => Some(Value::I64(
                            problems.iter().map(|problem| problem.count as i64).sum(),
                        )),
                    }
                } else {
                    None
                }
            }
            _ => None,
        }
    }

    fn on_error(&self, error: &BoxError, _: &Context) -> Option<Value> {
        match self {
            ConnectorSelector::Error { .. } => Some(error.to_string().into()),
            ConnectorSelector::StaticField { r#static } => Some(r#static.clone().into()),
            _ => None,
        }
    }

    fn on_drop(&self) -> Option<Value> {
        match self {
            ConnectorSelector::StaticField { r#static } => Some(r#static.clone().into()),
            _ => None,
        }
    }

    fn is_active(&self, stage: Stage) -> bool {
        match stage {
            Stage::Request => matches!(
                self,
                ConnectorSelector::HttpRequestHeader { .. }
                    | ConnectorSelector::SubgraphName { .. }
                    | ConnectorSelector::ConnectorSource { .. }
                    | ConnectorSelector::ConnectorHttpMethod { .. }
                    | ConnectorSelector::ConnectorUrlTemplate { .. }
                    | ConnectorSelector::StaticField { .. }
                    | ConnectorSelector::RequestMappingProblems { .. }
            ),
            Stage::Response => matches!(
                self,
                ConnectorSelector::ConnectorResponseHeader { .. }
                    | ConnectorSelector::ConnectorResponseStatus { .. }
                    | ConnectorSelector::SubgraphName { .. }
                    | ConnectorSelector::ConnectorSource { .. }
                    | ConnectorSelector::ConnectorHttpMethod { .. }
                    | ConnectorSelector::ConnectorUrlTemplate { .. }
                    | ConnectorSelector::StaticField { .. }
                    | ConnectorSelector::ResponseMappingProblems { .. }
            ),
            Stage::ResponseEvent => false,
            Stage::ResponseField => false,
            Stage::Error => matches!(
                self,
                ConnectorSelector::Error { .. }
                    | ConnectorSelector::SubgraphName { .. }
                    | ConnectorSelector::ConnectorSource { .. }
                    | ConnectorSelector::ConnectorHttpMethod { .. }
                    | ConnectorSelector::ConnectorUrlTemplate { .. }
                    | ConnectorSelector::StaticField { .. }
            ),
            Stage::Drop => matches!(self, ConnectorSelector::StaticField { .. }),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;
    use std::sync::Arc;

    use apollo_compiler::name;
    use apollo_federation::sources::connect::ConnectId;
    use apollo_federation::sources::connect::ConnectSpec;
    use apollo_federation::sources::connect::Connector;
    use apollo_federation::sources::connect::HttpJsonTransport;
    use apollo_federation::sources::connect::JSONSelection;
    use apollo_federation::sources::connect::StringTemplate;
    use http::HeaderValue;
    use http::StatusCode;
    use opentelemetry::Array;
    use opentelemetry::StringValue;
    use opentelemetry::Value;

    use super::ConnectorSelector;
    use super::ConnectorSource;
    use super::MappingProblems;
    use crate::Context;
    use crate::plugins::connectors::handle_responses::MappedResponse;
    use crate::plugins::connectors::make_requests::ResponseKey;
    use crate::plugins::connectors::mapping::Problem;
    use crate::plugins::telemetry::config_new::Selector;
    use crate::plugins::telemetry::config_new::selectors::ResponseStatus;
    use crate::services::connector::request_service::Request;
    use crate::services::connector::request_service::Response;
    use crate::services::connector::request_service::TransportRequest;
    use crate::services::connector::request_service::TransportResponse;
    use crate::services::connector::request_service::transport;
    use crate::services::router::body;

    const TEST_SUBGRAPH_NAME: &str = "test_subgraph_name";
    const TEST_SOURCE_NAME: &str = "test_source_name";
    const TEST_URL_TEMPLATE: &str = "/test";
    const TEST_HEADER_NAME: &str = "test_header_name";
    const TEST_HEADER_VALUE: &str = "test_header_value";
    const TEST_STATIC: &str = "test_static";

    fn context() -> Context {
        Context::default()
    }

    fn connector() -> Connector {
        Connector {
            id: ConnectId::new(
                TEST_SUBGRAPH_NAME.into(),
                Some(TEST_SOURCE_NAME.into()),
                name!(Query),
                name!(users),
                0,
                "label",
            ),
            transport: HttpJsonTransport {
                source_url: None,
                connect_template: StringTemplate::from_str(TEST_URL_TEMPLATE).unwrap(),
                ..Default::default()
            },
            selection: JSONSelection::empty(),
            config: None,
            max_requests: None,
            entity_resolver: None,
            spec: ConnectSpec::V0_1,
            request_variables: Default::default(),
            response_variables: Default::default(),
            batch_settings: None,
            request_headers: Default::default(),
            response_headers: Default::default(),
        }
    }

    fn response_key() -> ResponseKey {
        ResponseKey::RootField {
            name: "hello".to_string(),
            inputs: Default::default(),
            selection: Arc::new(JSONSelection::parse("$.data").unwrap()),
        }
    }

    fn http_request() -> http::Request<String> {
        http::Request::builder().body("".into()).unwrap()
    }

    fn http_request_with_header() -> http::Request<String> {
        let mut http_request = http::Request::builder().body("".into()).unwrap();
        http_request.headers_mut().insert(
            TEST_HEADER_NAME,
            HeaderValue::from_static(TEST_HEADER_VALUE),
        );
        http_request
    }

    fn connector_request(http_request: http::Request<String>) -> Request {
        connector_request_with_mapping_problems(http_request, vec![])
    }

    fn connector_request_with_mapping_problems(
        http_request: http::Request<String>,
        mapping_problems: Vec<Problem>,
    ) -> Request {
        Request {
            context: context(),
            connector: Arc::new(connector()),
            service_name: Default::default(),
            transport_request: TransportRequest::Http(transport::http::HttpRequest {
                inner: http_request,
                debug: None,
            }),
            key: response_key(),
            mapping_problems,
            supergraph_request: Default::default(),
        }
    }

    fn connector_response(status_code: StatusCode) -> Response {
        connector_response_with_mapping_problems(status_code, vec![])
    }

    fn connector_response_with_mapping_problems(
        status_code: StatusCode,
        mapping_problems: Vec<Problem>,
    ) -> Response {
        Response {
            context: context(),
            connector: connector().into(),
            transport_result: Ok(TransportResponse::Http(transport::http::HttpResponse {
                inner: http::Response::builder()
                    .status(status_code)
                    .body(body::empty())
                    .expect("expecting valid response")
                    .into_parts()
                    .0,
            })),
            mapped_response: MappedResponse::Data {
                data: serde_json::json!({})
                    .try_into()
                    .expect("expecting valid JSON"),
                key: response_key(),
                problems: mapping_problems,
            },
        }
    }

    fn connector_response_with_header() -> Response {
        Response {
            context: context(),
            connector: connector().into(),
            transport_result: Ok(TransportResponse::Http(transport::http::HttpResponse {
                inner: http::Response::builder()
                    .status(200)
                    .header(TEST_HEADER_NAME, TEST_HEADER_VALUE)
                    .body(body::empty())
                    .expect("expecting valid response")
                    .into_parts()
                    .0,
            })),
            mapped_response: MappedResponse::Data {
                data: serde_json::json!({})
                    .try_into()
                    .expect("expecting valid JSON"),
                key: response_key(),
                problems: vec![],
            },
        }
    }

    fn mapping_problems() -> Vec<Problem> {
        vec![
            Problem {
                count: 1,
                message: "error message".to_string(),
                path: "@.id".to_string(),
            },
            Problem {
                count: 2,
                message: "warn message".to_string(),
                path: "@.id".to_string(),
            },
            Problem {
                count: 3,
                message: "info message".to_string(),
                path: "@.id".to_string(),
            },
        ]
    }

    fn mapping_problem_array() -> Value {
        Value::Array(Array::String(vec![
            StringValue::from(String::from(
                "{\"message\":\"error message\",\"path\":\"@.id\",\"count\":1}",
            )),
            StringValue::from(String::from(
                "{\"message\":\"warn message\",\"path\":\"@.id\",\"count\":2}",
            )),
            StringValue::from(String::from(
                "{\"message\":\"info message\",\"path\":\"@.id\",\"count\":3}",
            )),
        ]))
    }

    #[test]
    fn connector_on_request_static_field() {
        let selector = ConnectorSelector::StaticField {
            r#static: TEST_STATIC.into(),
        };
        assert_eq!(
            Some(TEST_STATIC.into()),
            selector.on_request(&connector_request(http_request()))
        );
    }

    #[test]
    fn connector_on_request_subgraph_name() {
        let selector = ConnectorSelector::SubgraphName {
            subgraph_name: true,
        };
        assert_eq!(
            Some(TEST_SUBGRAPH_NAME.into()),
            selector.on_request(&connector_request(http_request()))
        );
    }

    #[test]
    fn connector_on_request_connector_source() {
        let selector = ConnectorSelector::ConnectorSource {
            connector_source: ConnectorSource::Name,
        };
        assert_eq!(
            Some(TEST_SOURCE_NAME.into()),
            selector.on_request(&connector_request(http_request()))
        );
    }

    #[test]
    fn connector_on_request_url_template() {
        let selector = ConnectorSelector::ConnectorUrlTemplate {
            connector_url_template: true,
        };
        assert_eq!(
            Some(TEST_URL_TEMPLATE.into()),
            selector.on_request(&connector_request(http_request()))
        );
    }

    #[test]
    fn connector_on_request_header_defaulted() {
        let selector = ConnectorSelector::HttpRequestHeader {
            connector_http_request_header: TEST_HEADER_NAME.to_string(),
            redact: None,
            default: Some("defaulted".into()),
        };
        assert_eq!(
            Some("defaulted".into()),
            selector.on_request(&connector_request(http_request()))
        );
    }

    #[test]
    fn connector_on_request_header_with_value() {
        let selector = ConnectorSelector::HttpRequestHeader {
            connector_http_request_header: TEST_HEADER_NAME.to_string(),
            redact: None,
            default: None,
        };
        assert_eq!(
            Some(TEST_HEADER_VALUE.into()),
            selector.on_request(&connector_request(http_request_with_header()))
        );
    }

    #[test]
    fn connector_on_response_header_defaulted() {
        let selector = ConnectorSelector::ConnectorResponseHeader {
            connector_http_response_header: TEST_HEADER_NAME.to_string(),
            redact: None,
            default: Some("defaulted".into()),
        };
        assert_eq!(
            Some("defaulted".into()),
            selector.on_response(&connector_response(StatusCode::OK))
        );
    }

    #[test]
    fn connector_on_response_header_with_value() {
        let selector = ConnectorSelector::ConnectorResponseHeader {
            connector_http_response_header: TEST_HEADER_NAME.to_string(),
            redact: None,
            default: None,
        };
        assert_eq!(
            Some(TEST_HEADER_VALUE.into()),
            selector.on_response(&connector_response_with_header())
        );
    }

    #[test]
    fn connector_on_response_status_code() {
        let selector = ConnectorSelector::ConnectorResponseStatus {
            connector_http_response_status: ResponseStatus::Code,
        };
        assert_eq!(
            Some(200.into()),
            selector.on_response(&connector_response(StatusCode::OK))
        );
    }

    #[test]
    fn connector_on_response_status_reason_ok() {
        let selector = ConnectorSelector::ConnectorResponseStatus {
            connector_http_response_status: ResponseStatus::Reason,
        };
        assert_eq!(
            Some("OK".into()),
            selector.on_response(&connector_response(StatusCode::OK))
        );
    }

    #[test]
    fn connector_on_response_status_code_not_found() {
        let selector = ConnectorSelector::ConnectorResponseStatus {
            connector_http_response_status: ResponseStatus::Reason,
        };
        assert_eq!(
            Some("Not Found".into()),
            selector.on_response(&connector_response(StatusCode::NOT_FOUND))
        );
    }

    #[test]
    fn connector_on_request_mapping_problems_none() {
        let selector = ConnectorSelector::RequestMappingProblems {
            connector_request_mapping_problems: MappingProblems::Problems,
        };
        assert_eq!(
            Some(Value::Array(Array::String(vec![]))),
            selector.on_request(&connector_request(http_request()))
        );
    }

    #[test]
    fn connector_on_request_mapping_problems_count_zero() {
        let selector = ConnectorSelector::RequestMappingProblems {
            connector_request_mapping_problems: MappingProblems::Count,
        };
        assert_eq!(
            Some(0.into()),
            selector.on_request(&connector_request(http_request()))
        );
    }

    #[test]
    fn connector_on_request_mapping_problems() {
        let selector = ConnectorSelector::RequestMappingProblems {
            connector_request_mapping_problems: MappingProblems::Problems,
        };
        assert_eq!(
            Some(mapping_problem_array()),
            selector.on_request(&connector_request_with_mapping_problems(
                http_request(),
                mapping_problems()
            ))
        );
    }

    #[test]
    fn connector_on_request_mapping_problems_count() {
        let selector = ConnectorSelector::RequestMappingProblems {
            connector_request_mapping_problems: MappingProblems::Count,
        };
        assert_eq!(
            Some(6.into()),
            selector.on_request(&connector_request_with_mapping_problems(
                http_request(),
                mapping_problems()
            ))
        );
    }

    #[test]
    fn connector_on_response_mapping_problems_none() {
        let selector = ConnectorSelector::ResponseMappingProblems {
            connector_response_mapping_problems: MappingProblems::Problems,
        };
        assert_eq!(
            Some(Value::Array(Array::String(vec![]))),
            selector.on_response(&connector_response(StatusCode::OK))
        );
    }

    #[test]
    fn connector_on_response_mapping_problems_count_zero() {
        let selector = ConnectorSelector::ResponseMappingProblems {
            connector_response_mapping_problems: MappingProblems::Count,
        };
        assert_eq!(
            Some(0.into()),
            selector.on_response(&connector_response(StatusCode::OK))
        );
    }

    #[test]
    fn connector_on_response_mapping_problems() {
        let selector = ConnectorSelector::ResponseMappingProblems {
            connector_response_mapping_problems: MappingProblems::Problems,
        };
        assert_eq!(
            Some(mapping_problem_array()),
            selector.on_response(&connector_response_with_mapping_problems(
                StatusCode::OK,
                mapping_problems()
            ))
        );
    }

    #[test]
    fn connector_on_response_mapping_problems_count() {
        let selector = ConnectorSelector::ResponseMappingProblems {
            connector_response_mapping_problems: MappingProblems::Count,
        };
        assert_eq!(
            Some(6.into()),
            selector.on_response(&connector_response_with_mapping_problems(
                StatusCode::OK,
                mapping_problems()
            ))
        );
    }

    #[test]
    fn connector_on_drop_static_field() {
        let selector = ConnectorSelector::StaticField {
            r#static: TEST_STATIC.into(),
        };
        assert_eq!(Some(TEST_STATIC.into()), selector.on_drop());
    }
}
