use schemars::JsonSchema;
use serde::Deserialize;
use tower::BoxError;

use crate::graphql::Request;
use crate::graphql::Response;
use crate::plugins::telemetry::config_new::Selector;

#[derive(Deserialize, JsonSchema, Clone, Debug)]
#[cfg_attr(test, derive(PartialEq))]
#[serde(deny_unknown_fields, untagged)]
pub(crate) enum GraphQLSelector {
    FieldLength,
}

impl Selector for GraphQLSelector {
    type Request = Request;
    type Response = Response;
    type EventResponse = ();

    fn on_request(&self, request: &Self::Request) -> Option<opentelemetry::Value> {
        None
    }

    fn on_response(&self, response: &Self::Response) -> Option<opentelemetry::Value> {
        todo!()
    }

    fn on_error(&self, error: &BoxError) -> Option<opentelemetry::Value> {
        None
    }
}
