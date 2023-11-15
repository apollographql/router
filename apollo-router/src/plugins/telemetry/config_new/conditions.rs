use opentelemetry_api::Value;
use schemars::JsonSchema;
use serde::Deserialize;

use crate::plugins::telemetry::config::AttributeValue;
use crate::plugins::telemetry::config_new::Selector;

#[allow(dead_code)]
#[derive(Deserialize, JsonSchema, Clone, Debug)]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
pub(crate) enum Condition<T> {
    /// A condition to check a selection against a value.
    Eq([SelectorOrValue<T>; 2]),
    /// All sub-conditions must be true.
    All(Vec<Condition<T>>),
    /// At least one sub-conditions must be true.
    Any(Vec<Condition<T>>),
    /// The sub-condition must not be true
    Not(Box<Condition<T>>),
}

impl Condition<()> {
    pub(crate) fn empty<T>() -> Condition<T> {
        Condition::Any(vec![])
    }
}

#[allow(dead_code)]
#[derive(Deserialize, JsonSchema, Clone, Debug)]
#[serde(deny_unknown_fields, rename_all = "snake_case", untagged)]
pub(crate) enum SelectorOrValue<T> {
    /// A constant value.
    Value(AttributeValue),
    /// Selector to extract a value from the pipeline.
    Selector(T),
}

#[allow(dead_code)]
impl<T> Condition<T>
where
    T: Selector,
{
    fn evaluate(&self, request: &T::Request, response: &T::Response) -> bool {
        match self {
            Condition::Eq(eq) => {
                // We don't know if the selection was for the request or result, so we try both.
                let left = eq[0]
                    .on_request(request)
                    .or_else(|| eq[0].on_response(response));
                let right = eq[1]
                    .on_request(request)
                    .or_else(|| eq[1].on_response(response));
                left == right
            }
            Condition::All(all) => all.iter().all(|c| c.evaluate(request, response)),
            Condition::Any(any) => any.iter().all(|c| c.evaluate(request, response)),
            Condition::Not(not) => !not.evaluate(request, response),
        }
    }
}

impl<T> Selector for SelectorOrValue<T>
where
    T: Selector,
{
    type Request = T::Request;
    type Response = T::Response;

    fn on_request(&self, request: &T::Request) -> Option<Value> {
        match self {
            SelectorOrValue::Value(value) => Some(value.clone().into()),
            SelectorOrValue::Selector(selector) => selector.on_request(request),
        }
    }

    fn on_response(&self, response: &T::Response) -> Option<Value> {
        match self {
            SelectorOrValue::Value(value) => Some(value.clone().into()),
            SelectorOrValue::Selector(selector) => selector.on_response(response),
        }
    }
}

#[cfg(test)]
mod test {
    #[test]
    fn test_conditions() {}
}
