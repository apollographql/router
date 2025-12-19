// mediator from selector to conditional if selector doesn't have all the required behavior

use std::fmt::Debug;

use apollo_compiler::ast::NamedType;
use apollo_compiler::executable::Field;
use opentelemetry::Array;
use opentelemetry::Value;
use schemars::JsonSchema;
use serde::Deserialize;
use tower::BoxError;

use crate::Context;
use crate::plugins::telemetry::config_new::Selector;
use crate::plugins::telemetry::config_new::Stage;

#[derive(Debug, Deserialize, JsonSchema, Clone, PartialEq)]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
pub(crate) enum Operator<T> {
    ArrayLength(T),
}

impl<T> Operator<T> {
    fn operate(&self, value: Value) -> Option<Value> {
        match self {
            Self::ArrayLength(_) => match value {
                Value::Bool(_) | Value::I64(_) | Value::F64(_) | Value::String(_) => None,
                Value::Array(arr) => match arr {
                    Array::Bool(arr) => Some((arr.len() as i64).into()),
                    Array::I64(arr) => Some((arr.len() as i64).into()),
                    Array::F64(arr) => Some((arr.len() as i64).into()),
                    Array::String(arr) => Some((arr.len() as i64).into()),
                },
            },
        }
    }
}

impl<T: Selector> Selector for Operator<T> {
    type Request = T::Request;
    type Response = T::Response;
    type EventResponse = T::EventResponse;

    fn on_request(&self, request: &Self::Request) -> Option<Value> {
        let value = match self {
            Self::ArrayLength(selector) => selector.on_request(request),
        }?;
        self.operate(value)
    }

    fn on_response(&self, response: &Self::Response) -> Option<Value> {
        let value = match self {
            Self::ArrayLength(selector) => selector.on_response(response),
        }?;
        self.operate(value)
    }

    fn on_response_event(&self, response: &Self::EventResponse, ctx: &Context) -> Option<Value> {
        let value = match self {
            Self::ArrayLength(selector) => selector.on_response_event(response, ctx),
        }?;
        self.operate(value)
    }

    fn on_error(&self, error: &BoxError, ctx: &Context) -> Option<Value> {
        let value = match self {
            Self::ArrayLength(selector) => selector.on_error(error, ctx),
        }?;
        self.operate(value)
    }

    fn on_response_field(
        &self,
        ty: &NamedType,
        field: &Field,
        value: &serde_json_bytes::Value,
        ctx: &Context,
    ) -> Option<Value> {
        let value = match self {
            Self::ArrayLength(selector) => selector.on_response_field(ty, field, value, ctx),
        }?;
        self.operate(value)
    }

    fn is_active(&self, stage: Stage) -> bool {
        match self {
            Self::ArrayLength(selector) => selector.is_active(stage),
        }
    }
}
