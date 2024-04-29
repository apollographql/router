use opentelemetry::Value;
use schemars::JsonSchema;
use serde::Deserialize;

use crate::plugins::telemetry::config::AttributeValue;
use crate::plugins::telemetry::config_new::Selector;

#[derive(Deserialize, JsonSchema, Clone, Debug)]
#[cfg_attr(test, derive(PartialEq))]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
pub(crate) enum Condition<T> {
    /// A condition to check a selection against a value.
    Eq([SelectorOrValue<T>; 2]),
    /// A condition to check a selection against a selector.
    Exists(T),
    /// All sub-conditions must be true.
    All(Vec<Condition<T>>),
    /// At least one sub-conditions must be true.
    Any(Vec<Condition<T>>),
    /// The sub-condition must not be true
    Not(Box<Condition<T>>),
    /// Static true condition
    True,
    /// Static false condition
    False,
}

impl<T> Default for Condition<T> {
    fn default() -> Self {
        Self::True
    }
}

impl Condition<()> {
    pub(crate) fn empty<T>() -> Condition<T> {
        Condition::True
    }
}

#[derive(Deserialize, JsonSchema, Clone, Debug)]
#[cfg_attr(test, derive(PartialEq))]
#[serde(deny_unknown_fields, rename_all = "snake_case", untagged)]
pub(crate) enum SelectorOrValue<T> {
    /// A constant value.
    Value(AttributeValue),
    /// Selector to extract a value from the pipeline.
    Selector(T),
}

impl<T> Condition<T>
where
    T: Selector,
{
    pub(crate) fn evaluate_request(&mut self, request: &T::Request) -> Option<bool> {
        match self {
            Condition::Eq(eq) => match (eq[0].on_request(request), eq[1].on_request(request)) {
                (None, None) => None,
                (None, Some(right)) => {
                    eq[1] = SelectorOrValue::Value(right.into());
                    None
                }
                (Some(left), None) => {
                    eq[0] = SelectorOrValue::Value(left.into());
                    None
                }
                (Some(left), Some(right)) => {
                    if left == right {
                        *self = Condition::True;
                        Some(true)
                    } else {
                        Some(false)
                    }
                }
            },
            Condition::Exists(exist) => {
                if exist.on_request(request).is_some() {
                    *self = Condition::True;
                    Some(true)
                } else {
                    None
                }
            }
            Condition::All(all) => {
                if all.is_empty() {
                    *self = Condition::True;
                    return Some(true);
                }
                let mut response = Some(true);
                for cond in all {
                    match cond.evaluate_request(request) {
                        Some(resp) => {
                            response = response.map(|r| resp && r);
                        }
                        None => {
                            response = None;
                        }
                    }
                }

                response
            }
            Condition::Any(any) => {
                if any.is_empty() {
                    *self = Condition::True;
                    return Some(true);
                }
                let mut response: Option<bool> = Some(false);
                for cond in any {
                    match cond.evaluate_request(request) {
                        Some(resp) => {
                            response = response.map(|r| resp || r);
                        }
                        None => {
                            response = None;
                        }
                    }
                }

                response
            }
            Condition::Not(not) => not.evaluate_request(request).map(|r| !r),
            Condition::True => Some(true),
            Condition::False => Some(false),
        }
    }

    pub(crate) fn evaluate_response(&self, response: &T::Response) -> bool {
        match self {
            Condition::Eq(eq) => {
                let left = eq[0].on_response(response);
                let right = eq[1].on_response(response);
                left == right
            }
            Condition::Exists(exist) => exist.on_response(response).is_some(),
            Condition::All(all) => all.iter().all(|c| c.evaluate_response(response)),
            Condition::Any(any) => any.iter().any(|c| c.evaluate_response(response)),
            Condition::Not(not) => !not.evaluate_response(response),
            Condition::True => true,
            Condition::False => false,
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
    use opentelemetry::Value;

    use crate::plugins::telemetry::config_new::conditions::Condition;
    use crate::plugins::telemetry::config_new::conditions::SelectorOrValue;
    use crate::plugins::telemetry::config_new::Selector;

    struct TestSelector;
    impl Selector for TestSelector {
        type Request = Option<i64>;
        type Response = Option<i64>;

        fn on_request(&self, request: &Self::Request) -> Option<Value> {
            request.map(Value::I64)
        }

        fn on_response(&self, response: &Self::Response) -> Option<Value> {
            response.map(Value::I64)
        }
    }

    enum TestSelectorReqRes {
        Req,
        Resp,
    }

    impl Selector for TestSelectorReqRes {
        type Request = Option<i64>;
        type Response = Option<i64>;

        fn on_request(&self, request: &Self::Request) -> Option<Value> {
            match self {
                TestSelectorReqRes::Req => request.map(Value::I64),
                TestSelectorReqRes::Resp => None,
            }
        }

        fn on_response(&self, response: &Self::Response) -> Option<Value> {
            match self {
                TestSelectorReqRes::Req => None,
                TestSelectorReqRes::Resp => response.map(Value::I64),
            }
        }
    }

    #[test]
    fn test_condition_exist() {
        assert_eq!(
            None,
            Condition::<TestSelectorReqRes>::Exists(TestSelectorReqRes::Req)
                .evaluate_request(&None)
        );
        assert!(
            !Condition::<TestSelectorReqRes>::Exists(TestSelectorReqRes::Req)
                .evaluate_response(&Some(3i64))
        );
        assert_eq!(
            Some(true),
            Condition::<TestSelectorReqRes>::Exists(TestSelectorReqRes::Req)
                .evaluate_request(&Some(2i64))
        );
        assert!(
            Condition::<TestSelectorReqRes>::Exists(TestSelectorReqRes::Resp)
                .evaluate_response(&Some(3i64))
        );
    }

    #[test]
    fn test_condition_eq() {
        assert_eq!(
            Some(true),
            Condition::<TestSelector>::Eq([
                SelectorOrValue::Value(1i64.into()),
                SelectorOrValue::Value(1i64.into()),
            ])
            .evaluate_request(&None)
        );
        assert!(Condition::<TestSelector>::Eq([
            SelectorOrValue::Value(1i64.into()),
            SelectorOrValue::Value(1i64.into()),
        ])
        .evaluate_response(&None));
        assert!(!Condition::<TestSelector>::Eq([
            SelectorOrValue::Value(1i64.into()),
            SelectorOrValue::Value(2i64.into()),
        ])
        .evaluate_response(&None));
    }

    #[test]
    fn test_condition_eq_selector() {
        assert_eq!(
            Some(true),
            Condition::<TestSelector>::Eq([
                SelectorOrValue::Selector(TestSelector),
                SelectorOrValue::Value(1i64.into()),
            ])
            .evaluate_request(&Some(1i64))
        );
        assert_eq!(
            Some(true),
            Condition::<TestSelector>::Eq([
                SelectorOrValue::Value(1i64.into()),
                SelectorOrValue::Selector(TestSelector),
            ])
            .evaluate_request(&Some(1i64))
        );

        assert!(Condition::<TestSelector>::Eq([
            SelectorOrValue::Selector(TestSelector),
            SelectorOrValue::Value(2i64.into()),
        ])
        .evaluate_request(&None)
        .is_none());

        assert!(Condition::<TestSelector>::Eq([
            SelectorOrValue::Selector(TestSelector),
            SelectorOrValue::Value(2i64.into()),
        ])
        .evaluate_response(&Some(2i64)));
        assert!(Condition::<TestSelector>::Eq([
            SelectorOrValue::Value(2i64.into()),
            SelectorOrValue::Selector(TestSelector),
        ])
        .evaluate_response(&Some(2i64)));

        assert!(!Condition::<TestSelector>::Eq([
            SelectorOrValue::Selector(TestSelector),
            SelectorOrValue::Value(3i64.into()),
        ])
        .evaluate_response(&None));
        assert!(!Condition::<TestSelector>::Eq([
            SelectorOrValue::Value(3i64.into()),
            SelectorOrValue::Selector(TestSelector),
        ])
        .evaluate_response(&None));

        let mut condition = Condition::<TestSelectorReqRes>::Eq([
            SelectorOrValue::Value(3i64.into()),
            SelectorOrValue::Selector(TestSelectorReqRes::Req),
        ]);
        assert_eq!(Some(false), condition.evaluate_request(&Some(2i64)));
    }

    #[test]
    fn test_condition_not() {
        assert!(Condition::<TestSelector>::Not(Box::new(Condition::Eq([
            SelectorOrValue::Value(1i64.into()),
            SelectorOrValue::Value(2i64.into()),
        ])))
        .evaluate_response(&None));

        assert!(!Condition::<TestSelector>::Not(Box::new(Condition::Eq([
            SelectorOrValue::Value(1i64.into()),
            SelectorOrValue::Value(1i64.into()),
        ])))
        .evaluate_response(&None));
    }

    #[test]
    fn test_condition_all() {
        assert!(Condition::<TestSelector>::All(vec![
            Condition::Eq([
                SelectorOrValue::Value(1i64.into()),
                SelectorOrValue::Value(1i64.into()),
            ]),
            Condition::Eq([
                SelectorOrValue::Value(2i64.into()),
                SelectorOrValue::Value(2i64.into()),
            ])
        ])
        .evaluate_response(&None));

        let mut condition = Condition::<TestSelectorReqRes>::All(vec![
            Condition::Eq([
                SelectorOrValue::Value(1i64.into()),
                SelectorOrValue::Selector(TestSelectorReqRes::Req),
            ]),
            Condition::Eq([
                SelectorOrValue::Value(3i64.into()),
                SelectorOrValue::Selector(TestSelectorReqRes::Resp),
            ]),
        ]);

        assert!(condition.evaluate_request(&Some(1i64)).is_none());
        assert!(condition.evaluate_response(&Some(3i64)));

        let mut condition = Condition::<TestSelectorReqRes>::All(vec![
            Condition::Eq([
                SelectorOrValue::Value(1i64.into()),
                SelectorOrValue::Selector(TestSelectorReqRes::Req),
            ]),
            Condition::Eq([
                SelectorOrValue::Value(3i64.into()),
                SelectorOrValue::Selector(TestSelectorReqRes::Resp),
            ]),
        ]);

        assert!(condition.evaluate_request(&Some(1i64)).is_none());
        assert!(!condition.evaluate_response(&Some(2i64)));

        let mut condition = Condition::<TestSelectorReqRes>::All(vec![
            Condition::Eq([
                SelectorOrValue::Value(1i64.into()),
                SelectorOrValue::Selector(TestSelectorReqRes::Req),
            ]),
            Condition::Eq([
                SelectorOrValue::Value(3i64.into()),
                SelectorOrValue::Selector(TestSelectorReqRes::Req),
            ]),
        ]);

        assert_eq!(Some(false), condition.evaluate_request(&Some(1i64)));
        assert!(!condition.evaluate_response(&Some(2i64)));

        assert!(!Condition::<TestSelector>::All(vec![
            Condition::Eq([
                SelectorOrValue::Value(1i64.into()),
                SelectorOrValue::Value(1i64.into()),
            ]),
            Condition::Eq([
                SelectorOrValue::Value(1i64.into()),
                SelectorOrValue::Value(2i64.into()),
            ])
        ])
        .evaluate_response(&None));
    }

    #[test]
    fn test_condition_any() {
        assert!(Condition::<TestSelector>::Any(vec![
            Condition::Eq([
                SelectorOrValue::Value(1i64.into()),
                SelectorOrValue::Value(1i64.into()),
            ]),
            Condition::Eq([
                SelectorOrValue::Value(1i64.into()),
                SelectorOrValue::Value(2i64.into()),
            ])
        ])
        .evaluate_response(&None));

        assert!(!Condition::<TestSelector>::All(vec![
            Condition::Eq([
                SelectorOrValue::Value(1i64.into()),
                SelectorOrValue::Value(2i64.into()),
            ]),
            Condition::Eq([
                SelectorOrValue::Value(1i64.into()),
                SelectorOrValue::Value(2i64.into()),
            ])
        ])
        .evaluate_response(&None));
    }
}
