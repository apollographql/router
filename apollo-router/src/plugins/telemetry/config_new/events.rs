use std::collections::HashMap;
use std::fmt::Debug;
use std::sync::Arc;

use opentelemetry::KeyValue;
use parking_lot::Mutex;
use schemars::JsonSchema;
use serde::Deserialize;
use tower::BoxError;

use super::instruments::Instrumented;
use super::Selector;
use super::Selectors;
use crate::plugins::telemetry::config_new::attributes::RouterAttributes;
use crate::plugins::telemetry::config_new::attributes::SubgraphAttributes;
use crate::plugins::telemetry::config_new::attributes::SupergraphAttributes;
use crate::plugins::telemetry::config_new::conditions::Condition;
use crate::plugins::telemetry::config_new::extendable::Extendable;
use crate::plugins::telemetry::config_new::selectors::RouterSelector;
use crate::plugins::telemetry::config_new::selectors::SubgraphSelector;
use crate::plugins::telemetry::config_new::selectors::SupergraphSelector;
use crate::services::router;
use crate::Context;

/// Events are
#[derive(Deserialize, JsonSchema, Clone, Default, Debug)]
#[serde(deny_unknown_fields, default)]
pub(crate) struct Events {
    /// Router service events
    router: Extendable<RouterEvents, Event<RouterAttributes, RouterSelector>>,
    /// Subgraph service events
    supergraph: Extendable<SupergraphEvents, Event<SupergraphAttributes, SupergraphSelector>>,
    /// Supergraph service events
    subgraph: Extendable<SubgraphEvents, Event<SubgraphAttributes, SubgraphSelector>>,
}

impl Events {
    pub(crate) fn new_router_events(&self) -> RouterCustomEvents {
        let mut router_events = Vec::new();
        if self.router.attributes.request != EventLevel::Off {
            router_events.push(CustomEvent {
                inner: Mutex::new(CustomEventInner {
                    name: "router.request".to_string(),
                    level: self.router.attributes.request,
                    event_on: EventOn::Request,
                    message: Arc::new(String::from("router request")),
                    selectors: None,
                    condition: Condition::True,
                    attributes: Vec::new(),
                }),
            });
        }
        if self.router.attributes.response != EventLevel::Off {
            router_events.push(CustomEvent {
                inner: Mutex::new(CustomEventInner {
                    name: "router.response".to_string(),
                    level: self.router.attributes.response,
                    event_on: EventOn::Response,
                    message: Arc::new(String::from("router response")),
                    selectors: None,
                    condition: Condition::True,
                    attributes: Vec::new(),
                }),
            });
        }
        if self.router.attributes.error != EventLevel::Off {
            router_events.push(CustomEvent {
                inner: Mutex::new(CustomEventInner {
                    name: "router.error".to_string(),
                    level: self.router.attributes.error,
                    event_on: EventOn::Error,
                    message: Arc::new(String::from("router error")),
                    selectors: None,
                    condition: Condition::True,
                    attributes: Vec::new(),
                }),
            });
        }

        for (event_name, event_cfg) in &self.router.custom {
            router_events.push(CustomEvent {
                inner: Mutex::new(CustomEventInner {
                    name: event_name.clone(),
                    level: event_cfg.level,
                    event_on: event_cfg.on,
                    message: event_cfg.message.clone(),
                    selectors: event_cfg.attributes.clone().into(),
                    condition: event_cfg.condition.clone(),
                    attributes: Vec::new(),
                }),
            })
        }

        router_events
    }
}

pub(crate) type RouterCustomEvents =
    Vec<CustomEvent<router::Request, router::Response, RouterAttributes, RouterSelector>>;

impl Instrumented for RouterCustomEvents {
    type Request = router::Request;
    type Response = router::Response;

    fn on_request(&self, request: &Self::Request) {
        for custom_event in self {
            custom_event.on_request(request);
        }
    }

    fn on_response(&self, response: &Self::Response) {
        for custom_event in self {
            custom_event.on_response(response);
        }
    }

    fn on_error(&self, error: &BoxError, ctx: &Context) {
        for custom_event in self {
            custom_event.on_error(error, ctx);
        }
    }
}

#[derive(Clone, Deserialize, JsonSchema, Debug, Default)]
#[serde(deny_unknown_fields, default)]
struct RouterEvents {
    /// Log the router request
    request: EventLevel,
    /// Log the router response
    response: EventLevel,
    /// Log the router error
    error: EventLevel,
}

#[derive(Clone, Deserialize, JsonSchema, Debug, Default)]
#[serde(deny_unknown_fields, default)]
struct SupergraphEvents {
    /// Log the supergraph request
    request: EventLevel,
    /// Log the supergraph response
    response: EventLevel,
    /// Log the supergraph error
    error: EventLevel,
}

#[derive(Clone, Deserialize, JsonSchema, Debug, Default)]
#[serde(deny_unknown_fields, default)]
struct SubgraphEvents {
    /// Log the subgraph request
    request: EventLevel,
    /// Log the subgraph response
    response: EventLevel,
    /// Log the subgraph error
    error: EventLevel,
}

#[derive(Deserialize, JsonSchema, Clone, Debug, Default, PartialEq, Copy)]
#[serde(rename_all = "snake_case")]
pub(crate) enum EventLevel {
    Info,
    Warn,
    Error,
    #[default]
    Off,
}

/// An event that can be logged as part of a trace.
/// The event has an implicit `type` attribute that matches the name of the event in the yaml
/// and a message that can be used to provide additional information.
#[derive(Deserialize, JsonSchema, Clone, Debug)]
pub(crate) struct Event<A, E>
where
    A: Default + Debug,
    E: Debug,
{
    /// The log level of the event.
    level: EventLevel,

    /// The event message.
    message: Arc<String>,

    /// When to trigger the event.
    on: EventOn,

    /// The event attributes.
    #[serde(default = "Extendable::empty_arc::<A, E>")]
    attributes: Arc<Extendable<A, E>>,

    /// The event conditions.
    #[serde(default = "Condition::empty::<E>")]
    condition: Condition<E>,
}

/// When to trigger the event.
#[derive(Deserialize, JsonSchema, Clone, Debug, Copy, PartialEq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum EventOn {
    /// Log the event on request
    Request,
    /// Log the event on response
    Response,
    /// Log the event on error
    Error,
}

struct CustomEvent<Request, Response, A, T>
where
    A: Selectors<Request = Request, Response = Response> + Default,
    T: Selector<Request = Request, Response = Response> + Debug,
{
    inner: Mutex<CustomEventInner<Request, Response, A, T>>,
}

struct CustomEventInner<Request, Response, A, T>
where
    A: Selectors<Request = Request, Response = Response> + Default,
    T: Selector<Request = Request, Response = Response> + Debug,
{
    name: String,
    level: EventLevel,
    event_on: EventOn,
    message: Arc<String>,
    selectors: Option<Arc<Extendable<A, T>>>,
    condition: Condition<T>,
    attributes: Vec<opentelemetry_api::KeyValue>,
}

impl<A, T, Request, Response> Instrumented for CustomEvent<Request, Response, A, T>
where
    A: Selectors<Request = Request, Response = Response> + Default,
    T: Selector<Request = Request, Response = Response> + Debug + Debug,
{
    type Request = Request;
    type Response = Response;

    fn on_request(&self, request: &Self::Request) {
        let mut inner = self.inner.lock();
        if inner.condition.evaluate_request(request) == Some(false) {
            return;
        }
        if let Some(selectors) = &inner.selectors {
            inner.attributes = selectors.on_request(request);
        }

        if let EventOn::Request = inner.event_on {
            inner.send_event();
        }
    }

    fn on_response(&self, response: &Self::Response) {
        let mut inner = self.inner.lock();
        if inner.event_on != EventOn::Response {
            return;
        }

        if !inner.condition.evaluate_response(response) {
            return;
        }
        if let Some(selectors) = &inner.selectors {
            inner
                .attributes
                .append(&mut selectors.on_response(response).into_iter().collect());
        }

        inner.send_event();
    }

    fn on_error(&self, error: &BoxError, _ctx: &Context) {
        let mut inner = self.inner.lock();
        if inner.event_on != EventOn::Error {
            return;
        }
        if let Some(selectors) = &inner.selectors {
            inner.attributes.append(&mut selectors.on_error(error));
        }

        inner.send_event();
    }
}

impl<A, T, Request, Response> CustomEventInner<Request, Response, A, T>
where
    A: Selectors<Request = Request, Response = Response> + Default,
    T: Selector<Request = Request, Response = Response> + Debug + Debug,
{
    fn send_event(&self) {
        let attributes: HashMap<&str, &str> = self
            .attributes
            .iter()
            .map(|kv| (kv.key.as_str(), kv.value.as_str().as_ref()))
            .collect();
        match self.level {
            EventLevel::Info => {
                ::tracing::info!(attributes = ?attributes, "{}", self.message);
            }
            EventLevel::Warn => todo!(),
            EventLevel::Error => todo!(),
            EventLevel::Off => todo!(),
        }
    }
}
