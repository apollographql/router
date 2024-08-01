use opentelemetry::Value;
use schemars::JsonSchema;
use serde::Deserialize;
use tower::BoxError;

use super::Stage;
use crate::plugins::telemetry::config::AttributeValue;
use crate::plugins::telemetry::config_new::Selector;
use crate::Context;

#[derive(Deserialize, JsonSchema, Clone, Debug, PartialEq)]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
pub(crate) enum Condition<T> {
    /// A condition to check a selection against a value.
    Eq([SelectorOrValue<T>; 2]),
    /// The first selection must be greater than the second selection.
    Gt([SelectorOrValue<T>; 2]),
    /// The first selection must be less than the second selection.
    Lt([SelectorOrValue<T>; 2]),
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

#[derive(Deserialize, JsonSchema, Clone, Debug, PartialEq)]
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
            Condition::Eq(eq) => {
                if !eq[0].is_active(Stage::Request) && !eq[1].is_active(Stage::Request) {
                    // Nothing to compute here
                    return None;
                }
                match (eq[0].on_request(request), eq[1].on_request(request)) {
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
                }
            }
            Condition::Gt(gt) => {
                if !gt[0].is_active(Stage::Request) && !gt[1].is_active(Stage::Request) {
                    // Nothing to compute here
                    return None;
                }
                let left_att = gt[0].on_request(request).map(AttributeValue::from);
                let right_att = gt[1].on_request(request).map(AttributeValue::from);
                match (left_att, right_att) {
                    (None, None) => None,
                    (Some(l), None) => {
                        gt[0] = SelectorOrValue::Value(l);
                        None
                    }
                    (None, Some(r)) => {
                        gt[1] = SelectorOrValue::Value(r);
                        None
                    }
                    (Some(l), Some(r)) => {
                        if l > r {
                            *self = Condition::True;
                            Some(true)
                        } else {
                            *self = Condition::False;
                            Some(false)
                        }
                    }
                }
            }
            Condition::Lt(lt) => {
                if !lt[0].is_active(Stage::Request) && !lt[1].is_active(Stage::Request) {
                    // Nothing to compute here
                    return None;
                }
                let left_att = lt[0].on_request(request).map(AttributeValue::from);
                let right_att = lt[1].on_request(request).map(AttributeValue::from);
                match (left_att, right_att) {
                    (None, None) => None,
                    (Some(l), None) => {
                        lt[0] = SelectorOrValue::Value(l);
                        None
                    }
                    (None, Some(r)) => {
                        lt[1] = SelectorOrValue::Value(r);
                        None
                    }
                    (Some(l), Some(r)) => {
                        if l < r {
                            *self = Condition::True;
                            Some(true)
                        } else {
                            *self = Condition::False;
                            Some(false)
                        }
                    }
                }
            }
            Condition::Exists(exist) => {
                if exist.is_active(Stage::Request) {
                    if exist.on_request(request).is_some() {
                        *self = Condition::True;
                        Some(true)
                    } else {
                        Some(false)
                    }
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

    pub(crate) fn evaluate_event_response(
        &self,
        response: &T::EventResponse,
        ctx: &Context,
    ) -> bool {
        match self {
            Condition::Eq(eq) => {
                let left = eq[0].on_response_event(response, ctx);
                let right = eq[1].on_response_event(response, ctx);
                left == right
            }
            Condition::Gt(gt) => {
                let left_att = gt[0]
                    .on_response_event(response, ctx)
                    .map(AttributeValue::from);
                let right_att = gt[1]
                    .on_response_event(response, ctx)
                    .map(AttributeValue::from);
                left_att.zip(right_att).map_or(false, |(l, r)| l > r)
            }
            Condition::Lt(gt) => {
                let left_att = gt[0]
                    .on_response_event(response, ctx)
                    .map(AttributeValue::from);
                let right_att = gt[1]
                    .on_response_event(response, ctx)
                    .map(AttributeValue::from);
                left_att.zip(right_att).map_or(false, |(l, r)| l < r)
            }
            Condition::Exists(exist) => exist.on_response_event(response, ctx).is_some(),
            Condition::All(all) => all.iter().all(|c| c.evaluate_event_response(response, ctx)),
            Condition::Any(any) => any.iter().any(|c| c.evaluate_event_response(response, ctx)),
            Condition::Not(not) => !not.evaluate_event_response(response, ctx),
            Condition::True => true,
            Condition::False => false,
        }
    }

    pub(crate) fn evaluate_response(&self, response: &T::Response) -> bool {
        match self {
            Condition::Eq(eq) => {
                let left = eq[0].on_response(response);
                let right = eq[1].on_response(response);
                left == right
            }
            Condition::Gt(gt) => {
                let left_att = gt[0].on_response(response).map(AttributeValue::from);
                let right_att = gt[1].on_response(response).map(AttributeValue::from);
                left_att.zip(right_att).map_or(false, |(l, r)| l > r)
            }
            Condition::Lt(gt) => {
                let left_att = gt[0].on_response(response).map(AttributeValue::from);
                let right_att = gt[1].on_response(response).map(AttributeValue::from);
                left_att.zip(right_att).map_or(false, |(l, r)| l < r)
            }
            Condition::Exists(exist) => exist.on_response(response).is_some(),
            Condition::All(all) => all.iter().all(|c| c.evaluate_response(response)),
            Condition::Any(any) => any.iter().any(|c| c.evaluate_response(response)),
            Condition::Not(not) => !not.evaluate_response(response),
            Condition::True => true,
            Condition::False => false,
        }
    }

    pub(crate) fn evaluate_error(&self, error: &BoxError, ctx: &Context) -> bool {
        match self {
            Condition::Eq(eq) => {
                let left = eq[0].on_error(error, ctx);
                let right = eq[1].on_error(error, ctx);
                left == right
            }
            Condition::Gt(gt) => {
                let left_att = gt[0].on_error(error, ctx).map(AttributeValue::from);
                let right_att = gt[1].on_error(error, ctx).map(AttributeValue::from);
                left_att.zip(right_att).map_or(false, |(l, r)| l > r)
            }
            Condition::Lt(gt) => {
                let left_att = gt[0].on_error(error, ctx).map(AttributeValue::from);
                let right_att = gt[1].on_error(error, ctx).map(AttributeValue::from);
                left_att.zip(right_att).map_or(false, |(l, r)| l < r)
            }
            Condition::Exists(exist) => exist.on_error(error, ctx).is_some(),
            Condition::All(all) => all.iter().all(|c| c.evaluate_error(error, ctx)),
            Condition::Any(any) => any.iter().any(|c| c.evaluate_error(error, ctx)),
            Condition::Not(not) => !not.evaluate_error(error, ctx),
            Condition::True => true,
            Condition::False => false,
        }
    }

    pub(crate) fn evaluate_response_field(
        &self,
        ty: &apollo_compiler::executable::NamedType,
        field: &apollo_compiler::executable::Field,
        value: &serde_json_bytes::Value,
        ctx: &Context,
    ) -> bool {
        match self {
            Condition::Eq(eq) => {
                let left = eq[0].on_response_field(ty, field, value, ctx);
                let right = eq[1].on_response_field(ty, field, value, ctx);
                left == right
            }
            Condition::Gt(gt) => {
                let left_att = gt[0]
                    .on_response_field(ty, field, value, ctx)
                    .map(AttributeValue::from);
                let right_att = gt[1]
                    .on_response_field(ty, field, value, ctx)
                    .map(AttributeValue::from);
                left_att.zip(right_att).map_or(false, |(l, r)| l > r)
            }
            Condition::Lt(gt) => {
                let left_att = gt[0]
                    .on_response_field(ty, field, value, ctx)
                    .map(AttributeValue::from);
                let right_att = gt[1]
                    .on_response_field(ty, field, value, ctx)
                    .map(AttributeValue::from);
                left_att.zip(right_att).map_or(false, |(l, r)| l < r)
            }
            Condition::Exists(exist) => exist.on_response_field(ty, field, value, ctx).is_some(),
            Condition::All(all) => all
                .iter()
                .all(|c| c.evaluate_response_field(ty, field, value, ctx)),
            Condition::Any(any) => any
                .iter()
                .any(|c| c.evaluate_response_field(ty, field, value, ctx)),
            Condition::Not(not) => !not.evaluate_response_field(ty, field, value, ctx),
            Condition::True => true,
            Condition::False => false,
        }
    }

    pub(crate) fn evaluate_drop(&self) -> Option<bool> {
        match self {
            Condition::Eq(eq) => match (eq[0].on_drop(), eq[1].on_drop()) {
                (Some(left), Some(right)) => {
                    if left == right {
                        Some(true)
                    } else {
                        Some(false)
                    }
                }
                _ => None,
            },
            Condition::Gt(gt) => {
                let left_att = gt[0].on_drop().map(AttributeValue::from);
                let right_att = gt[1].on_drop().map(AttributeValue::from);
                match (left_att, right_att) {
                    (Some(l), Some(r)) => {
                        if l > r {
                            Some(true)
                        } else {
                            Some(false)
                        }
                    }
                    _ => None,
                }
            }
            Condition::Lt(lt) => {
                let left_att = lt[0].on_drop().map(AttributeValue::from);
                let right_att = lt[1].on_drop().map(AttributeValue::from);
                match (left_att, right_att) {
                    (Some(l), Some(r)) => {
                        if l < r {
                            Some(true)
                        } else {
                            Some(false)
                        }
                    }
                    _ => None,
                }
            }
            Condition::Exists(exist) => {
                if exist.on_drop().is_some() {
                    Some(true)
                } else {
                    None
                }
            }
            Condition::All(all) => {
                if all.is_empty() {
                    return Some(true);
                }
                let mut response = Some(true);
                for cond in all {
                    match cond.evaluate_drop() {
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
                    return Some(true);
                }
                let mut response: Option<bool> = Some(false);
                for cond in any {
                    match cond.evaluate_drop() {
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
            Condition::Not(not) => not.evaluate_drop().map(|r| !r),
            Condition::True => Some(true),
            Condition::False => Some(false),
        }
    }
}

impl<T> Selector for SelectorOrValue<T>
where
    T: Selector,
{
    type Request = T::Request;
    type Response = T::Response;
    type EventResponse = T::EventResponse;

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

    fn on_response_event(&self, response: &T::EventResponse, ctx: &Context) -> Option<Value> {
        match self {
            SelectorOrValue::Value(value) => Some(value.clone().into()),
            SelectorOrValue::Selector(selector) => selector.on_response_event(response, ctx),
        }
    }

    fn on_error(&self, error: &BoxError, ctx: &Context) -> Option<Value> {
        match self {
            SelectorOrValue::Value(value) => Some(value.clone().into()),
            SelectorOrValue::Selector(selector) => selector.on_error(error, ctx),
        }
    }

    fn on_response_field(
        &self,
        ty: &apollo_compiler::executable::NamedType,
        field: &apollo_compiler::executable::Field,
        value: &serde_json_bytes::Value,
        ctx: &Context,
    ) -> Option<Value> {
        match self {
            SelectorOrValue::Value(value) => Some(value.clone().into()),
            SelectorOrValue::Selector(selector) => {
                selector.on_response_field(ty, field, value, ctx)
            }
        }
    }

    fn on_drop(&self) -> Option<Value> {
        match self {
            SelectorOrValue::Value(value) => Some(value.clone().into()),
            SelectorOrValue::Selector(selector) => selector.on_drop(),
        }
    }

    fn is_active(&self, stage: super::Stage) -> bool {
        match self {
            SelectorOrValue::Value(_) => true,
            SelectorOrValue::Selector(selector) => selector.is_active(stage),
        }
    }
}

#[cfg(test)]
mod test {
    use opentelemetry::Value;
    use serde_json_bytes::json;
    use tower::BoxError;
    use TestSelector::Req;
    use TestSelector::Resp;
    use TestSelector::Static;

    use crate::plugins::telemetry::config_new::conditions::Condition;
    use crate::plugins::telemetry::config_new::conditions::SelectorOrValue;
    use crate::plugins::telemetry::config_new::test::field;
    use crate::plugins::telemetry::config_new::test::ty;
    use crate::plugins::telemetry::config_new::Selector;
    use crate::Context;

    enum TestSelector {
        Req,
        Resp,
        Static(i64),
    }

    impl Selector for TestSelector {
        type Request = Option<i64>;
        type Response = Option<i64>;
        type EventResponse = Option<i64>;

        fn on_request(&self, request: &Self::Request) -> Option<Value> {
            match self {
                Req => request.map(Value::I64),
                Resp => None,
                Static(v) => Some((*v).into()),
            }
        }

        fn on_response_event(
            &self,
            response: &Self::EventResponse,
            _ctx: &crate::Context,
        ) -> Option<opentelemetry::Value> {
            match self {
                Req => None,
                Resp => response.map(Value::I64),
                Static(v) => Some((*v).into()),
            }
        }

        fn on_response(&self, response: &Self::Response) -> Option<Value> {
            match self {
                Req => None,
                Resp => response.map(Value::I64),
                Static(v) => Some((*v).into()),
            }
        }

        fn on_error(
            &self,
            error: &tower::BoxError,
            _ctx: &Context,
        ) -> Option<opentelemetry::Value> {
            if error.to_string() != "<empty>" {
                Some("error".into())
            } else {
                None
            }
        }

        fn on_response_field(
            &self,
            _ty: &apollo_compiler::executable::NamedType,
            _field: &apollo_compiler::executable::Field,
            value: &serde_json_bytes::Value,
            _ctx: &Context,
        ) -> Option<Value> {
            if let serde_json_bytes::Value::Number(val) = value {
                Some(Value::I64(val.as_i64().expect("mut be i64")))
            } else {
                None
            }
        }

        fn on_drop(&self) -> Option<Value> {
            match self {
                Static(v) => Some((*v).into()),
                _ => None,
            }
        }

        fn is_active(&self, _stage: crate::plugins::telemetry::config_new::Stage) -> bool {
            true
        }
    }

    #[test]
    fn test_condition_exist() {
        assert_eq!(exists(Req).req(None), Some(false));
        assert_eq!(exists(Req).req(Some(1i64)), Some(true));
        assert!(!exists(Resp).resp(None));
        assert!(exists(Resp).resp(Some(1i64)));
        assert!(!exists(Resp).resp_event(None));
        assert!(exists(Resp).resp_event(Some(1i64)));
        assert!(!exists(Resp).error(None));
        assert!(exists(Resp).error(Some("error")));
        assert!(!exists(Resp).field(None));
        assert!(exists(Resp).field(Some(1i64)));
    }

    #[test]
    fn test_condition_eq() {
        assert_eq!(eq(1, 2).req(None), Some(false));
        assert_eq!(eq(1, 1).req(None), Some(true));
        assert!(!eq(1, 2).resp(None));
        assert!(eq(1, 1).resp(None));
        assert!(!eq(1, 2).resp_event(None));
        assert!(eq(1, 1).resp_event(None));
        assert!(!eq(1, 2).error(None));
        assert!(eq(1, 1).error(None));
        assert!(!eq(1, 2).field(None));
        assert!(eq(1, 1).field(None));
    }

    #[test]
    fn test_condition_gt() {
        test_gt(2, 1, 1);
        test_gt(2.0, 1.0, 1.0);
        test_gt("b", "a", "a");
        assert_eq!(gt(true, false).req(None), Some(true));
        assert_eq!(gt(false, true).req(None), Some(false));
        assert_eq!(gt(true, true).req(None), Some(false));
        assert_eq!(gt(false, false).req(None), Some(false));
    }

    fn test_gt<T>(l1: T, l2: T, r: T)
    where
        T: Into<SelectorOrValue<TestSelector>> + Clone,
    {
        assert_eq!(gt(l1.clone(), r.clone()).req(None), Some(true));
        assert_eq!(gt(l2.clone(), r.clone()).req(None), Some(false));
        assert!(gt(l1.clone(), r.clone()).resp(None));
        assert!(!gt(l2.clone(), r.clone()).resp(None));
        assert!(gt(l1.clone(), r.clone()).resp_event(None));
        assert!(!gt(l2.clone(), r.clone()).resp_event(None));
        assert!(gt(l1.clone(), r.clone()).error(None));
        assert!(!gt(l2.clone(), r.clone()).error(None));
        assert!(gt(l1.clone(), r.clone()).field(None));
        assert!(!gt(l2.clone(), r.clone()).field(None));
    }

    #[test]
    fn test_condition_lt() {
        test_lt(1, 2, 2);
        test_lt(1.0, 2.0, 2.0);
        test_lt("a", "b", "b");
        assert_eq!(lt(true, false).req(None), Some(false));
        assert_eq!(lt(false, true).req(None), Some(true));
        assert_eq!(lt(true, true).req(None), Some(false));
        assert_eq!(lt(false, false).req(None), Some(false));
    }

    fn test_lt<T>(l1: T, l2: T, r: T)
    where
        T: Into<SelectorOrValue<TestSelector>> + Clone,
    {
        assert_eq!(lt(l1.clone(), r.clone()).req(None), Some(true));
        assert_eq!(lt(l2.clone(), r.clone()).req(None), Some(false));
        assert!(lt(l1.clone(), r.clone()).resp(None));
        assert!(!lt(l2.clone(), r.clone()).resp(None));
        assert!(lt(l1.clone(), r.clone()).resp_event(None));
        assert!(!lt(l2.clone(), r.clone()).resp_event(None));
        assert!(lt(l1.clone(), r.clone()).error(None));
        assert!(!lt(l2.clone(), r.clone()).error(None));
        assert!(lt(l1.clone(), r.clone()).field(None));
        assert!(!lt(l2.clone(), r.clone()).field(None));
    }

    #[test]
    fn test_condition_not() {
        assert_eq!(not(eq(1, 2)).req(None), Some(true));
        assert_eq!(not(eq(1, 1)).req(None), Some(false));
        assert!(not(eq(1, 2)).resp(None));
        assert!(!not(eq(1, 1)).resp(None));
        assert!(not(eq(1, 2)).resp_event(None));
        assert!(!not(eq(1, 1)).resp_event(None));
        assert!(not(eq(1, 2)).error(None));
        assert!(!not(eq(1, 1)).error(None));
        assert!(not(eq(1, 2)).field(None));
        assert!(!not(eq(1, 1)).field(None));
    }

    #[test]
    fn test_condition_all() {
        assert_eq!(all(eq(1, 1), eq(1, 2)).req(None), Some(false));
        assert_eq!(all(eq(1, 1), eq(2, 2)).req(None), Some(true));
        assert!(!all(eq(1, 1), eq(1, 2)).resp(None));
        assert!(all(eq(1, 1), eq(2, 2)).resp(None));
        assert!(!all(eq(1, 1), eq(1, 2)).resp_event(None));
        assert!(all(eq(1, 1), eq(2, 2)).resp_event(None));
        assert!(!all(eq(1, 1), eq(1, 2)).error(None));
        assert!(all(eq(1, 1), eq(2, 2)).error(None));
        assert!(!all(eq(1, 1), eq(1, 2)).field(None));
        assert!(all(eq(1, 1), eq(2, 2)).field(None));
    }

    #[test]
    fn test_condition_any() {
        assert_eq!(any(eq(1, 1), eq(1, 2)).req(None), Some(true));
        assert_eq!(any(eq(1, 1), eq(2, 2)).req(None), Some(true));
        assert!(any(eq(1, 1), eq(1, 2)).resp(None));
        assert!(any(eq(1, 1), eq(2, 2)).resp(None));
        assert!(any(eq(1, 1), eq(1, 2)).resp_event(None));
        assert!(any(eq(1, 1), eq(2, 2)).resp_event(None));
        assert!(any(eq(1, 1), eq(1, 2)).error(None));
        assert!(any(eq(1, 1), eq(2, 2)).error(None));
        assert!(any(eq(1, 1), eq(1, 2)).field(None));
        assert!(any(eq(1, 1), eq(2, 2)).field(None));
    }

    #[test]
    fn test_rewrite() {
        // These conditions are stateful and require that the request first evaluated before the response will yield true.
        let mut condition = all(eq(1, Req), eq(3, Resp));
        assert!(!condition.resp(Some(3i64)));
        assert!(condition.req(Some(1i64)).is_none());
        assert!(condition.resp(Some(3i64)));

        let mut condition = all(gt(2, Req), eq(3, Resp));
        assert!(!condition.resp(Some(3i64)));
        assert!(condition.req(Some(1i64)).is_none());
        assert!(condition.resp(Some(3i64)));

        let mut condition = all(lt(1, Req), eq(3, Resp));
        assert!(!condition.resp(Some(3i64)));
        assert!(condition.req(Some(2i64)).is_none());
        assert!(condition.resp(Some(3i64)));

        let mut condition = all(exists(Req), eq(3, Resp));
        assert!(!condition.resp(Some(3i64)));
        assert!(condition.req(Some(1i64)).is_none());
        assert!(condition.resp(Some(3i64)));
    }

    #[test]
    fn test_condition_selector() {
        // req handling is special so needs extensive testing. Other methods are just passthoughs, so we can so a single check on eq
        assert_eq!(eq(Req, 1).req(Some(1i64)), Some(true));
        assert_eq!(eq(Req, 1).req(None), None);
        assert_eq!(eq(1, Req).req(Some(1i64)), Some(true));
        assert_eq!(eq(1, Req).req(None), None);
        assert_eq!(eq(Req, Req).req(Some(1i64)), Some(true));
        assert_eq!(eq(Req, Req).req(None), None);

        assert_eq!(gt(Req, 1).req(Some(2i64)), Some(true));
        assert_eq!(gt(Req, 1).req(None), None);
        assert_eq!(gt(2, Req).req(Some(1i64)), Some(true));
        assert_eq!(gt(2, Req).req(None), None);
        assert_eq!(gt(Req, Req).req(Some(1i64)), Some(false));
        assert_eq!(gt(Req, 1).req(None), None);

        assert_eq!(lt(Req, 2).req(Some(1i64)), Some(true));
        assert_eq!(lt(Req, 2).req(None), None);
        assert_eq!(lt(1, Req).req(Some(2i64)), Some(true));
        assert_eq!(lt(1, Req).req(None), None);
        assert_eq!(lt(Req, Req).req(Some(1i64)), Some(false));
        assert_eq!(lt(Req, Req).req(None), None);

        assert_eq!(exists(Req).req(Some(1i64)), Some(true));
        assert_eq!(exists(Req).req(None), Some(false));
        assert_eq!(exists(Resp).resp(None), false);

        assert_eq!(all(eq(1, 1), eq(1, Req)).req(Some(1i64)), Some(true));
        assert_eq!(all(eq(1, 1), eq(1, Req)).req(None), None);
        assert_eq!(any(eq(1, 2), eq(1, Req)).req(Some(1i64)), Some(true));
        assert_eq!(any(eq(1, 2), eq(1, Req)).req(None), None);

        assert!(eq(Resp, 1).resp(Some(1i64)));
        assert!(eq(Resp, 1).resp_event(Some(1i64)));
        assert!(eq(Resp, 1).field(Some(1i64)));
        assert!(eq(Resp, "error").error(Some("error")));
    }

    #[test]
    fn test_evaluate_drop() {
        assert!(eq(Req, 1).evaluate_drop().is_none());
        assert!(eq(1, Req).evaluate_drop().is_none());
        assert_eq!(eq(1, 1).evaluate_drop(), Some(true));
        assert_eq!(eq(1, 2).evaluate_drop(), Some(false));
        assert_eq!(eq(Static(1), 1).evaluate_drop(), Some(true));
        assert_eq!(eq(1, Static(2)).evaluate_drop(), Some(false));
        assert_eq!(lt(1, 2).evaluate_drop(), Some(true));
        assert_eq!(lt(2, 1).evaluate_drop(), Some(false));
        assert_eq!(lt(Static(1), 2).evaluate_drop(), Some(true));
        assert_eq!(lt(2, Static(1)).evaluate_drop(), Some(false));
        assert_eq!(gt(2, 1).evaluate_drop(), Some(true));
        assert_eq!(gt(1, 2).evaluate_drop(), Some(false));
        assert_eq!(gt(Static(2), 1).evaluate_drop(), Some(true));
        assert_eq!(gt(1, Static(2)).evaluate_drop(), Some(false));
        assert_eq!(not(eq(1, 2)).evaluate_drop(), Some(true));
        assert_eq!(not(eq(1, 1)).evaluate_drop(), Some(false));
        assert_eq!(all(eq(1, 1), eq(2, 2)).evaluate_drop(), Some(true));
        assert_eq!(all(eq(1, 1), eq(2, 1)).evaluate_drop(), Some(false));
        assert_eq!(any(eq(1, 1), eq(1, 2)).evaluate_drop(), Some(true));
        assert_eq!(any(eq(1, 2), eq(1, 2)).evaluate_drop(), Some(false));
    }

    fn exists(selector: TestSelector) -> Condition<TestSelector> {
        Condition::<TestSelector>::Exists(selector)
    }

    fn not(selector: Condition<TestSelector>) -> Condition<TestSelector> {
        Condition::<TestSelector>::Not(Box::new(selector))
    }

    fn eq<L, R>(left: L, right: R) -> Condition<TestSelector>
    where
        L: Into<SelectorOrValue<TestSelector>>,
        R: Into<SelectorOrValue<TestSelector>>,
    {
        Condition::<TestSelector>::Eq([left.into(), right.into()])
    }

    fn gt<L, R>(left: L, right: R) -> Condition<TestSelector>
    where
        L: Into<SelectorOrValue<TestSelector>>,
        R: Into<SelectorOrValue<TestSelector>>,
    {
        Condition::<TestSelector>::Gt([left.into(), right.into()])
    }

    fn lt<L, R>(left: L, right: R) -> Condition<TestSelector>
    where
        L: Into<SelectorOrValue<TestSelector>>,
        R: Into<SelectorOrValue<TestSelector>>,
    {
        Condition::<TestSelector>::Lt([left.into(), right.into()])
    }

    fn all(l: Condition<TestSelector>, r: Condition<TestSelector>) -> Condition<TestSelector>
where {
        Condition::<TestSelector>::All(vec![l, r])
    }

    fn any(l: Condition<TestSelector>, r: Condition<TestSelector>) -> Condition<TestSelector>
where {
        Condition::<TestSelector>::Any(vec![l, r])
    }

    impl From<bool> for SelectorOrValue<TestSelector> {
        fn from(value: bool) -> Self {
            SelectorOrValue::Value(value.into())
        }
    }

    impl From<i64> for SelectorOrValue<TestSelector> {
        fn from(value: i64) -> Self {
            SelectorOrValue::Value(value.into())
        }
    }

    impl From<f64> for SelectorOrValue<TestSelector> {
        fn from(value: f64) -> Self {
            SelectorOrValue::Value(value.into())
        }
    }
    impl From<&str> for SelectorOrValue<TestSelector> {
        fn from(value: &str) -> Self {
            SelectorOrValue::Value(value.to_string().into())
        }
    }

    impl From<TestSelector> for SelectorOrValue<TestSelector> {
        fn from(value: TestSelector) -> Self {
            SelectorOrValue::Selector(value)
        }
    }

    impl Condition<TestSelector> {
        fn req(&mut self, value: Option<i64>) -> Option<bool> {
            self.evaluate_request(&value)
        }
        fn resp(&mut self, value: Option<i64>) -> bool {
            self.evaluate_response(&value)
        }
        fn resp_event(&mut self, value: Option<i64>) -> bool {
            self.evaluate_event_response(&value, &Context::new())
        }
        fn error(&mut self, value: Option<&str>) -> bool {
            self.evaluate_error(
                &BoxError::from(value.unwrap_or("<empty>")),
                &Default::default(),
            )
        }
        fn field(&mut self, value: Option<i64>) -> bool {
            match value {
                None => {
                    self.evaluate_response_field(&ty(), field(), &json!(false), &Context::new())
                }
                Some(value) => {
                    self.evaluate_response_field(&ty(), field(), &json!(value), &Context::new())
                }
            }
        }
    }
}
