//! Selectors related to Connectors.

use derivative::Derivative;
use opentelemetry_api::Value;
use schemars::JsonSchema;
use serde::Deserialize;
use tower::BoxError;

use crate::plugins::telemetry::config::AttributeValue;
use crate::plugins::telemetry::config_new::instruments::InstrumentValue;
use crate::plugins::telemetry::config_new::instruments::Standard;
use crate::plugins::telemetry::config_new::selectors::ErrorRepr;
use crate::plugins::telemetry::config_new::selectors::ResponseStatus;
use crate::plugins::telemetry::config_new::Selector;
use crate::plugins::telemetry::config_new::Stage;
use crate::services::connector_service::ConnectorInfo;
use crate::services::connector_service::CONNECTOR_INFO_CONTEXT_KEY;
use crate::services::http::HttpRequest;
use crate::services::http::HttpResponse;
use crate::Context;

#[derive(Deserialize, JsonSchema, Clone, Debug, PartialEq)]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
pub(crate) enum ConnectorSource {
    /// The name of the connector source.
    Name,
}

#[derive(Deserialize, JsonSchema, Clone, Debug)]
#[serde(deny_unknown_fields, rename_all = "snake_case", untagged)]
pub(crate) enum ConnectorHttpValue {
    Standard(Standard),
    Custom(ConnectorHttpSelector),
}

impl From<&ConnectorHttpValue> for InstrumentValue<ConnectorHttpSelector> {
    fn from(value: &ConnectorHttpValue) -> Self {
        match value {
            ConnectorHttpValue::Standard(s) => InstrumentValue::Standard(s.clone()),
            ConnectorHttpValue::Custom(selector) => InstrumentValue::Custom(selector.clone()),
        }
    }
}

#[derive(Deserialize, JsonSchema, Clone, Derivative)]
#[serde(deny_unknown_fields, rename_all = "snake_case", untagged)]
#[derivative(Debug, PartialEq)]
pub(crate) enum ConnectorHttpSelector {
    SubgraphName {
        /// The subgraph name
        subgraph_name: bool,
    },
    ConnectorSource {
        /// The connector source.
        connector_source: ConnectorSource,
    },
    ConnectorRequestHeader {
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
}

impl Selector for ConnectorHttpSelector {
    type Request = HttpRequest;
    type Response = HttpResponse;
    type EventResponse = ();

    fn on_request(&self, request: &Self::Request) -> Option<Value> {
        let connector_info = request
            .context
            .get::<&str, ConnectorInfo>(CONNECTOR_INFO_CONTEXT_KEY);
        match self {
            ConnectorHttpSelector::SubgraphName { subgraph_name } if *subgraph_name => {
                connector_info
                    .ok()
                    .flatten()
                    .map(|info| info.subgraph_name.clone())
                    .map(opentelemetry::Value::from)
            }
            ConnectorHttpSelector::ConnectorSource { .. } => connector_info
                .ok()
                .flatten()
                .and_then(|info| info.source_name.clone())
                .map(opentelemetry::Value::from),
            ConnectorHttpSelector::ConnectorHttpMethod {
                connector_http_method,
            } if *connector_http_method => connector_info
                .ok()
                .flatten()
                .map(|info| info.http_method.clone())
                .map(opentelemetry::Value::from),
            ConnectorHttpSelector::ConnectorUrlTemplate {
                connector_url_template,
            } if *connector_url_template => connector_info
                .ok()
                .flatten()
                .map(|info| info.url_template.clone())
                .map(opentelemetry::Value::from),
            ConnectorHttpSelector::ConnectorRequestHeader {
                connector_http_request_header: connector_request_header,
                default,
                ..
            } => request
                .http_request
                .headers()
                .get(connector_request_header)
                .and_then(|h| Some(h.to_str().ok()?.to_string()))
                .or_else(|| default.clone())
                .map(opentelemetry::Value::from),
            ConnectorHttpSelector::StaticField { r#static } => Some(r#static.clone().into()),
            _ => None,
        }
    }

    fn on_response(&self, response: &Self::Response) -> Option<Value> {
        let connector_info = response
            .context
            .get::<&str, ConnectorInfo>(CONNECTOR_INFO_CONTEXT_KEY);
        match self {
            ConnectorHttpSelector::SubgraphName { subgraph_name } if *subgraph_name => {
                connector_info
                    .ok()
                    .flatten()
                    .map(|info| info.subgraph_name.clone())
                    .map(opentelemetry::Value::from)
            }
            ConnectorHttpSelector::ConnectorSource { .. } => connector_info
                .ok()
                .flatten()
                .and_then(|info| info.source_name.clone())
                .map(opentelemetry::Value::from),
            ConnectorHttpSelector::ConnectorHttpMethod {
                connector_http_method,
            } if *connector_http_method => connector_info
                .ok()
                .flatten()
                .map(|info| info.http_method.clone())
                .map(opentelemetry::Value::from),
            ConnectorHttpSelector::ConnectorUrlTemplate {
                connector_url_template,
            } if *connector_url_template => connector_info
                .ok()
                .flatten()
                .map(|info| info.url_template.clone())
                .map(opentelemetry::Value::from),
            ConnectorHttpSelector::ConnectorResponseHeader {
                connector_http_response_header: connector_response_header,
                default,
                ..
            } => response
                .http_response
                .headers()
                .get(connector_response_header)
                .and_then(|h| Some(h.to_str().ok()?.to_string()))
                .or_else(|| default.clone())
                .map(opentelemetry::Value::from),
            ConnectorHttpSelector::ConnectorResponseStatus {
                connector_http_response_status: response_status,
            } => match response_status {
                ResponseStatus::Code => {
                    Some(Value::I64(response.http_response.status().as_u16() as i64))
                }
                ResponseStatus::Reason => response
                    .http_response
                    .status()
                    .canonical_reason()
                    .map(|reason| reason.into()),
            },
            ConnectorHttpSelector::StaticField { r#static } => Some(r#static.clone().into()),
            _ => None,
        }
    }

    fn on_error(&self, error: &BoxError, ctx: &Context) -> Option<Value> {
        let connector_info = ctx.get::<&str, ConnectorInfo>(CONNECTOR_INFO_CONTEXT_KEY);
        match self {
            ConnectorHttpSelector::SubgraphName { subgraph_name } if *subgraph_name => {
                connector_info
                    .ok()
                    .flatten()
                    .map(|info| info.subgraph_name.clone())
                    .map(opentelemetry::Value::from)
            }
            ConnectorHttpSelector::ConnectorSource { .. } => connector_info
                .ok()
                .flatten()
                .and_then(|info| info.source_name.clone())
                .map(opentelemetry::Value::from),
            ConnectorHttpSelector::ConnectorHttpMethod {
                connector_http_method,
            } if *connector_http_method => connector_info
                .ok()
                .flatten()
                .map(|info| info.http_method.clone())
                .map(opentelemetry::Value::from),
            ConnectorHttpSelector::ConnectorUrlTemplate {
                connector_url_template,
            } if *connector_url_template => connector_info
                .ok()
                .flatten()
                .map(|info| info.url_template.clone())
                .map(opentelemetry::Value::from),
            ConnectorHttpSelector::Error { .. } => Some(error.to_string().into()),
            ConnectorHttpSelector::StaticField { r#static } => Some(r#static.clone().into()),
            _ => None,
        }
    }

    fn on_drop(&self) -> Option<Value> {
        match self {
            ConnectorHttpSelector::StaticField { r#static } => Some(r#static.clone().into()),
            _ => None,
        }
    }

    fn is_active(&self, stage: Stage) -> bool {
        match stage {
            Stage::Request => matches!(
                self,
                ConnectorHttpSelector::ConnectorRequestHeader { .. }
                    | ConnectorHttpSelector::SubgraphName { .. }
                    | ConnectorHttpSelector::ConnectorSource { .. }
                    | ConnectorHttpSelector::ConnectorHttpMethod { .. }
                    | ConnectorHttpSelector::ConnectorUrlTemplate { .. }
                    | ConnectorHttpSelector::StaticField { .. }
            ),
            Stage::Response => matches!(
                self,
                ConnectorHttpSelector::ConnectorResponseHeader { .. }
                    | ConnectorHttpSelector::ConnectorResponseStatus { .. }
                    | ConnectorHttpSelector::SubgraphName { .. }
                    | ConnectorHttpSelector::ConnectorSource { .. }
                    | ConnectorHttpSelector::ConnectorHttpMethod { .. }
                    | ConnectorHttpSelector::ConnectorUrlTemplate { .. }
                    | ConnectorHttpSelector::StaticField { .. }
            ),
            Stage::ResponseEvent => false,
            Stage::ResponseField => false,
            Stage::Error => matches!(
                self,
                ConnectorHttpSelector::Error { .. }
                    | ConnectorHttpSelector::SubgraphName { .. }
                    | ConnectorHttpSelector::ConnectorSource { .. }
                    | ConnectorHttpSelector::ConnectorHttpMethod { .. }
                    | ConnectorHttpSelector::ConnectorUrlTemplate { .. }
                    | ConnectorHttpSelector::StaticField { .. }
            ),
            Stage::Drop => matches!(self, ConnectorHttpSelector::StaticField { .. }),
        }
    }
}

#[cfg(test)]
mod tests {
    use apollo_federation::sources::connect::HTTPMethod;
    use http::StatusCode;
    use rstest::fixture;
    use rstest::rstest;

    use super::ConnectorSource;
    use crate::plugins::telemetry::config_new::connectors::http::selectors::ConnectorHttpSelector;
    use crate::plugins::telemetry::config_new::selectors::ResponseStatus;
    use crate::plugins::telemetry::config_new::selectors::RouterSelector;
    use crate::plugins::telemetry::config_new::Selector;
    use crate::services::connector_service::ConnectorInfo;
    use crate::services::connector_service::CONNECTOR_INFO_CONTEXT_KEY;
    use crate::services::http::HttpRequest;
    use crate::services::http::HttpResponse;
    use crate::Context;

    const TEST_SUBGRAPH_NAME: &str = "test_subgraph_name";
    const TEST_SOURCE_NAME: &str = "test_source_name";
    const TEST_URL_TEMPLATE: &str = "/test";
    const TEST_HEADER_NAME: &str = "test_header_name";
    const TEST_HEADER_VALUE: &str = "test_header_value";
    const TEST_STATIC: &str = "test_static";

    #[fixture]
    fn connector_info() -> ConnectorInfo {
        ConnectorInfo {
            subgraph_name: TEST_SUBGRAPH_NAME.to_string(),
            source_name: Some(TEST_SOURCE_NAME.to_string()),
            http_method: HTTPMethod::Get.as_str().to_string(),
            url_template: TEST_URL_TEMPLATE.to_string(),
        }
    }

    #[fixture]
    fn context(connector_info: ConnectorInfo) -> Context {
        let context = Context::default();
        context
            .insert(CONNECTOR_INFO_CONTEXT_KEY, connector_info)
            .unwrap();
        context
    }

    #[fixture]
    fn http_request(context: Context) -> HttpRequest {
        HttpRequest {
            http_request: http::Request::builder().body("".into()).unwrap(),
            context,
        }
    }

    #[fixture]
    fn http_request_with_header(context: Context) -> HttpRequest {
        HttpRequest {
            http_request: http::Request::builder()
                .header(TEST_HEADER_NAME, TEST_HEADER_VALUE)
                .body("".into())
                .unwrap(),
            context,
        }
    }

    #[fixture]
    fn http_response(
        context: Context,
        #[default(StatusCode::OK)] status_code: StatusCode,
    ) -> HttpResponse {
        HttpResponse {
            http_response: http::Response::builder()
                .status(status_code)
                .body("".into())
                .unwrap(),
            context,
        }
    }

    #[fixture]
    fn http_response_with_header(
        context: Context,
        #[default(StatusCode::OK)] status_code: StatusCode,
    ) -> HttpResponse {
        HttpResponse {
            http_response: http::Response::builder()
                .status(status_code)
                .header(TEST_HEADER_NAME, TEST_HEADER_VALUE)
                .body("".into())
                .unwrap(),
            context,
        }
    }

    #[rstest]
    #[case(
        http_request(context(connector_info())),
        ConnectorHttpSelector::StaticField { r#static: TEST_STATIC.into() },
        Some(TEST_STATIC.into()),
    )]
    #[case(
        http_request(context(connector_info())),
        ConnectorHttpSelector::SubgraphName { subgraph_name: true },
        Some(TEST_SUBGRAPH_NAME.into()),
    )]
    #[case(
        http_request(context(connector_info())),
        ConnectorHttpSelector::ConnectorSource { connector_source: ConnectorSource::Name },
        Some(TEST_SOURCE_NAME.into()),
    )]
    #[case(
        http_request(context(connector_info())),
        ConnectorHttpSelector::ConnectorUrlTemplate { connector_url_template: true },
        Some(TEST_URL_TEMPLATE.into()),
    )]
    #[case(
        http_request(context(connector_info())),
        ConnectorHttpSelector::ConnectorRequestHeader {
            connector_http_request_header: TEST_HEADER_NAME.to_string(),
            redact: None,
            default: Some("defaulted".into()),
        },
        Some("defaulted".into()),
    )]
    #[case(
        http_request_with_header(context(connector_info())),
        ConnectorHttpSelector::ConnectorRequestHeader {
            connector_http_request_header: TEST_HEADER_NAME.to_string(),
            redact: None,
            default: None,
        },
        Some(TEST_HEADER_VALUE.into()),
    )]
    fn connector_on_request(
        #[case] http_request: HttpRequest,
        #[case] selector: ConnectorHttpSelector,
        #[case] expected: Option<opentelemetry::Value>,
    ) {
        assert_eq!(expected, selector.on_request(&http_request));
    }

    #[rstest]
    #[case(
        http_response(context(connector_info()), StatusCode::OK),
        ConnectorHttpSelector::StaticField { r#static: TEST_STATIC.into() },
        Some(TEST_STATIC.into()),
    )]
    #[case(
        http_response(context(connector_info()), StatusCode::OK),
        ConnectorHttpSelector::SubgraphName { subgraph_name: true },
        Some(TEST_SUBGRAPH_NAME.into()),
    )]
    #[case(
        http_response(context(connector_info()), StatusCode::OK),
        ConnectorHttpSelector::ConnectorSource { connector_source: ConnectorSource::Name },
        Some(TEST_SOURCE_NAME.into()),
    )]
    #[case(
        http_response(context(connector_info()), StatusCode::OK),
        ConnectorHttpSelector::ConnectorUrlTemplate { connector_url_template: true },
        Some(TEST_URL_TEMPLATE.into()),
    )]
    #[case(
        http_response(context(connector_info()), StatusCode::OK),
        ConnectorHttpSelector::ConnectorResponseHeader {
            connector_http_response_header: TEST_HEADER_NAME.to_string(),
            redact: None,
            default: Some("defaulted".into()),
        },
        Some("defaulted".into()),
    )]
    #[case(
        http_response_with_header(context(connector_info()), StatusCode::OK),
        ConnectorHttpSelector::ConnectorResponseHeader {
            connector_http_response_header: TEST_HEADER_NAME.to_string(),
            redact: None,
            default: None,
        },
        Some(TEST_HEADER_VALUE.into()),
    )]
    #[case(
        http_response(context(connector_info()), StatusCode::NOT_FOUND),
        ConnectorHttpSelector::ConnectorResponseStatus {
            connector_http_response_status: ResponseStatus::Code,
        },
        Some(opentelemetry::Value::I64(404)),
    )]
    #[case(
        http_response(context(connector_info()), StatusCode::NOT_FOUND),
        ConnectorHttpSelector::ConnectorResponseStatus {
            connector_http_response_status: ResponseStatus::Reason,
        },
        Some("Not Found".into()),
    )]
    #[case(
        http_response(context(connector_info()), StatusCode::OK),
        ConnectorHttpSelector::ConnectorHttpMethod { connector_http_method: true },
        Some(HTTPMethod::Get.as_str().into()),
    )]
    fn connector_on_response(
        #[case] http_response: HttpResponse,
        #[case] selector: ConnectorHttpSelector,
        #[case] expected: Option<opentelemetry::Value>,
    ) {
        assert_eq!(expected, selector.on_response(&http_response));
    }

    #[rstest]
    #[case(
        RouterSelector::StaticField { r#static: TEST_STATIC.into() },
        Some(TEST_STATIC.into()),
    )]
    fn connector_on_drop(
        #[case] selector: RouterSelector,
        #[case] expected: Option<opentelemetry::Value>,
    ) {
        assert_eq!(expected, selector.on_drop());
    }
}
