use std::collections::HashMap;
use std::sync::Arc;

use opentelemetry::metrics::MeterProvider;
use opentelemetry_api::Key;
use opentelemetry_api::KeyValue;
use parking_lot::Mutex;
use schemars::JsonSchema;
use serde::Deserialize;
use tower::BoxError;

use super::instruments::Increment;
use super::instruments::StaticInstrument;
use crate::metrics;
use crate::plugins::demand_control::COST_ACTUAL_CONTEXT_KEY;
use crate::plugins::demand_control::COST_ESTIMATED_CONTEXT_KEY;
use crate::plugins::demand_control::COST_RESULT_CONTEXT_KEY;
use crate::plugins::demand_control::COST_STRATEGY_CONTEXT_KEY;
use crate::plugins::telemetry::config::AttributeValue;
use crate::plugins::telemetry::config_new::attributes::SupergraphAttributes;
use crate::plugins::telemetry::config_new::conditions::Condition;
use crate::plugins::telemetry::config_new::extendable::Extendable;
use crate::plugins::telemetry::config_new::instruments::CustomHistogram;
use crate::plugins::telemetry::config_new::instruments::CustomHistogramInner;
use crate::plugins::telemetry::config_new::instruments::DefaultedStandardInstrument;
use crate::plugins::telemetry::config_new::instruments::Instrumented;
use crate::plugins::telemetry::config_new::selectors::SupergraphSelector;
use crate::plugins::telemetry::config_new::Selectors;
use crate::services::supergraph;
use crate::services::supergraph::Request;
use crate::services::supergraph::Response;
use crate::Context;

pub(crate) const APOLLO_PRIVATE_COST_ESTIMATED: Key =
    Key::from_static_str("apollo_private.cost.estimated");
pub(crate) const APOLLO_PRIVATE_COST_ACTUAL: Key =
    Key::from_static_str("apollo_private.cost.actual");
pub(crate) const APOLLO_PRIVATE_COST_STRATEGY: Key =
    Key::from_static_str("apollo_private.cost.strategy");
pub(crate) const APOLLO_PRIVATE_COST_RESULT: Key =
    Key::from_static_str("apollo_private.cost.result");

static COST_ESTIMATED: &str = "cost.estimated";
static COST_ACTUAL: &str = "cost.actual";
static COST_DELTA: &str = "cost.delta";

/// Attributes for Cost
#[derive(Deserialize, JsonSchema, Clone, Default, Debug, PartialEq)]
#[serde(deny_unknown_fields, default)]
pub(crate) struct SupergraphCostAttributes {
    /// The estimated cost of the operation using the currently configured cost model
    #[serde(rename = "cost.estimated")]
    cost_estimated: Option<bool>,
    /// The actual cost of the operation using the currently configured cost model
    #[serde(rename = "cost.actual")]
    cost_actual: Option<bool>,
    /// The delta (estimated - actual) cost of the operation using the currently configured cost model
    #[serde(rename = "cost.delta")]
    cost_delta: Option<bool>,
    /// The cost result, this is an error code returned by the cost calculation or COST_OK
    #[serde(rename = "cost.result")]
    cost_result: Option<bool>,
}

impl Selectors for SupergraphCostAttributes {
    type Request = supergraph::Request;
    type Response = supergraph::Response;
    type EventResponse = crate::graphql::Response;

    fn on_request(&self, _request: &Self::Request) -> Vec<KeyValue> {
        Vec::default()
    }

    fn on_response(&self, _response: &Self::Response) -> Vec<KeyValue> {
        Vec::default()
    }

    fn on_error(&self, _error: &BoxError, _ctx: &Context) -> Vec<KeyValue> {
        Vec::default()
    }

    fn on_response_event(&self, _response: &Self::EventResponse, ctx: &Context) -> Vec<KeyValue> {
        let mut attrs = Vec::with_capacity(4);
        if let (Some(true), Ok(Some(cost))) = (
            self.cost_estimated,
            ctx.get::<&str, f64>(COST_ESTIMATED_CONTEXT_KEY),
        ) {
            attrs.push(KeyValue::new("cost.estimated", cost));
        }
        if let (Some(true), Ok(Some(cost))) = (
            self.cost_actual,
            ctx.get::<&str, f64>(COST_ACTUAL_CONTEXT_KEY),
        ) {
            attrs.push(KeyValue::new("cost.actual", cost));
        }
        if let (Some(true), Ok(Some(estimated_cost)), Ok(Some(actual_cost))) = (
            self.cost_delta,
            ctx.get::<&str, f64>(COST_ESTIMATED_CONTEXT_KEY),
            ctx.get::<&str, f64>(COST_ESTIMATED_CONTEXT_KEY),
        ) {
            attrs.push(KeyValue::new("cost.delta", estimated_cost - actual_cost));
        }
        if let (Some(true), Ok(Some(result))) = (
            self.cost_result,
            ctx.get::<&str, String>(COST_RESULT_CONTEXT_KEY),
        ) {
            attrs.push(KeyValue::new("cost.result", result));
        }
        attrs
    }
}

#[derive(Deserialize, JsonSchema, Clone, Default, Debug)]
#[serde(deny_unknown_fields, default)]
pub(crate) struct CostInstrumentsConfig {
    /// A histogram of the estimated cost of the operation using the currently configured cost model
    #[serde(rename = "cost.estimated")]
    pub(crate) cost_estimated:
        DefaultedStandardInstrument<Extendable<SupergraphAttributes, SupergraphSelector>>,
    /// A histogram of the actual cost of the operation using the currently configured cost model
    #[serde(rename = "cost.actual")]
    pub(crate) cost_actual:
        DefaultedStandardInstrument<Extendable<SupergraphAttributes, SupergraphSelector>>,
    /// A histogram of the delta between the estimated and actual cost of the operation using the currently configured cost model
    #[serde(rename = "cost.delta")]
    pub(crate) cost_delta:
        DefaultedStandardInstrument<Extendable<SupergraphAttributes, SupergraphSelector>>,
}

impl CostInstrumentsConfig {
    pub(crate) fn new_static_instruments(&self) -> HashMap<String, StaticInstrument> {
        let meter = metrics::meter_provider()
            .meter(crate::plugins::telemetry::config_new::instruments::METER_NAME);

        [(
            COST_ESTIMATED.to_string(),
            StaticInstrument::Histogram(meter.f64_histogram(COST_ESTIMATED).with_description("Estimated cost of the operation using the currently configured cost model").init()),
        ),(
            COST_ACTUAL.to_string(),
            StaticInstrument::Histogram(meter.f64_histogram(COST_ACTUAL).with_description("Actual cost of the operation using the currently configured cost model").init()),
        ),(
            COST_DELTA.to_string(),
            StaticInstrument::Histogram(meter.f64_histogram(COST_DELTA).with_description("Delta between the estimated and actual cost of the operation using the currently configured cost model").init()),
        )]
        .into_iter()
        .collect()
    }

    pub(crate) fn to_instruments(
        &self,
        static_instruments: Arc<HashMap<String, StaticInstrument>>,
    ) -> CostInstruments {
        let cost_estimated = self.cost_estimated.is_enabled().then(|| {
            Self::histogram(
                COST_ESTIMATED,
                &self.cost_estimated,
                SupergraphSelector::Cost {
                    cost: CostValue::Estimated,
                },
                &static_instruments,
            )
        });

        let cost_actual = self.cost_actual.is_enabled().then(|| {
            Self::histogram(
                COST_ACTUAL,
                &self.cost_actual,
                SupergraphSelector::Cost {
                    cost: CostValue::Actual,
                },
                &static_instruments,
            )
        });

        let cost_delta = self.cost_delta.is_enabled().then(|| {
            Self::histogram(
                COST_DELTA,
                &self.cost_delta,
                SupergraphSelector::Cost {
                    cost: CostValue::Delta,
                },
                &static_instruments,
            )
        });
        CostInstruments {
            cost_estimated,
            cost_actual,
            cost_delta,
        }
    }

    fn histogram(
        name: &'static str,
        config: &DefaultedStandardInstrument<Extendable<SupergraphAttributes, SupergraphSelector>>,
        selector: SupergraphSelector,
        static_instruments: &Arc<HashMap<String, StaticInstrument>>,
    ) -> CustomHistogram<Request, Response, SupergraphAttributes, SupergraphSelector> {
        let mut nb_attributes = 0;
        let selectors = match config {
            DefaultedStandardInstrument::Bool(_) | DefaultedStandardInstrument::Unset => None,
            DefaultedStandardInstrument::Extendable { attributes } => {
                nb_attributes = attributes.custom.len();
                Some(attributes.clone())
            }
        };

        CustomHistogram {
            inner: Mutex::new(CustomHistogramInner {
                increment: Increment::EventCustom(None),
                condition: Condition::True,
                histogram: Some(
                    static_instruments
                        .get(name)
                        .expect("cannot get static instrument for cost; this should not happen")
                        .as_histogram()
                        .expect("cannot convert instrument to histogram for cost; this should not happen").clone(),
                ),
                attributes: Vec::with_capacity(nb_attributes),
                selector: Some(Arc::new(selector)),
                selectors,
                updated: false,
            }),
        }
    }
}

/// Instruments for cost
#[derive(Default)]
pub(crate) struct CostInstruments {
    /// A histogram of the estimated cost of the operation using the currently configured cost model
    cost_estimated: Option<
        CustomHistogram<
            supergraph::Request,
            supergraph::Response,
            SupergraphAttributes,
            SupergraphSelector,
        >,
    >,

    /// A histogram of the actual cost of the operation using the currently configured cost model
    cost_actual: Option<
        CustomHistogram<
            supergraph::Request,
            supergraph::Response,
            SupergraphAttributes,
            SupergraphSelector,
        >,
    >,
    /// A histogram of the delta between the estimated and actual cost of the operation using the currently configured cost model
    cost_delta: Option<
        CustomHistogram<
            supergraph::Request,
            supergraph::Response,
            SupergraphAttributes,
            SupergraphSelector,
        >,
    >,
}

impl Instrumented for CostInstruments {
    type Request = supergraph::Request;
    type Response = supergraph::Response;
    type EventResponse = crate::graphql::Response;

    fn on_request(&self, request: &Self::Request) {
        if let Some(cost_estimated) = &self.cost_estimated {
            cost_estimated.on_request(request);
        }
        if let Some(cost_actual) = &self.cost_actual {
            cost_actual.on_request(request);
        }
        if let Some(cost_delta) = &self.cost_delta {
            cost_delta.on_request(request);
        }
    }

    fn on_response(&self, response: &Self::Response) {
        if let Some(cost_estimated) = &self.cost_estimated {
            cost_estimated.on_response(response);
        }
        if let Some(cost_actual) = &self.cost_actual {
            cost_actual.on_response(response);
        }
        if let Some(cost_delta) = &self.cost_delta {
            cost_delta.on_response(response);
        }
    }

    fn on_error(&self, error: &BoxError, ctx: &Context) {
        if let Some(cost_estimated) = &self.cost_estimated {
            cost_estimated.on_error(error, ctx);
        }
        if let Some(cost_actual) = &self.cost_actual {
            cost_actual.on_error(error, ctx);
        }
        if let Some(cost_delta) = &self.cost_delta {
            cost_delta.on_error(error, ctx);
        }
    }

    fn on_response_event(&self, response: &Self::EventResponse, ctx: &Context) {
        if let Some(cost_estimated) = &self.cost_estimated {
            cost_estimated.on_response_event(response, ctx);
        }
        if let Some(cost_actual) = &self.cost_actual {
            cost_actual.on_response_event(response, ctx);
        }
        if let Some(cost_delta) = &self.cost_delta {
            cost_delta.on_response_event(response, ctx);
        }
    }
}

#[derive(Deserialize, JsonSchema, Clone, Debug, PartialEq)]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
pub(crate) enum CostValue {
    /// The estimated cost of the operation using the currently configured cost model
    Estimated,
    /// The actual cost of the operation using the currently configured cost model
    Actual,
    /// The delta between the estimated and actual cost of the operation using the currently configured cost model
    Delta,
    /// The result of the cost calculation. This is the error code returned by the cost calculation.
    Result,
}

pub(crate) fn add_cost_attributes(context: &Context, custom_attributes: &mut Vec<KeyValue>) {
    if let Ok(Some(cost)) = context.get::<&str, f64>(COST_ESTIMATED_CONTEXT_KEY) {
        custom_attributes.push(KeyValue::new(
            APOLLO_PRIVATE_COST_ESTIMATED.clone(),
            AttributeValue::F64(cost),
        ));
    }
    if let Ok(Some(cost)) = context.get::<&str, f64>(COST_ACTUAL_CONTEXT_KEY) {
        custom_attributes.push(KeyValue::new(
            APOLLO_PRIVATE_COST_ACTUAL.clone(),
            AttributeValue::F64(cost),
        ));
    }
    if let Ok(Some(result)) = context.get::<&str, String>(COST_RESULT_CONTEXT_KEY) {
        custom_attributes.push(KeyValue::new(
            APOLLO_PRIVATE_COST_RESULT.clone(),
            AttributeValue::String(result),
        ));
    }
    if let Ok(Some(strategy)) = context.get::<&str, String>(COST_STRATEGY_CONTEXT_KEY) {
        custom_attributes.push(KeyValue::new(
            APOLLO_PRIVATE_COST_STRATEGY.clone(),
            AttributeValue::String(strategy),
        ));
    }
}

#[cfg(test)]
mod test {
    use std::sync::Arc;

    use crate::context::OPERATION_NAME;
    use crate::plugins::demand_control::COST_ACTUAL_CONTEXT_KEY;
    use crate::plugins::demand_control::COST_ESTIMATED_CONTEXT_KEY;
    use crate::plugins::demand_control::COST_RESULT_CONTEXT_KEY;
    use crate::plugins::telemetry::config_new::cost::CostInstruments;
    use crate::plugins::telemetry::config_new::cost::CostInstrumentsConfig;
    use crate::plugins::telemetry::config_new::instruments::Instrumented;
    use crate::services::supergraph;
    use crate::Context;

    #[test]
    fn test_default_estimated() {
        let config = config(include_str!("fixtures/cost_estimated.router.yaml"));
        let instruments = config.to_instruments(Arc::new(config.new_static_instruments()));
        make_request(&instruments);

        assert_histogram_sum!("cost.estimated", 100.0);
        make_request(&instruments);

        assert_histogram_sum!("cost.estimated", 200.0);
    }

    #[test]
    fn test_default_actual() {
        let config = config(include_str!("fixtures/cost_actual.router.yaml"));
        let instruments = config.to_instruments(Arc::new(config.new_static_instruments()));
        make_request(&instruments);

        assert_histogram_sum!("cost.actual", 10.0);
        make_request(&instruments);

        assert_histogram_sum!("cost.actual", 20.0);
    }

    #[test]
    fn test_default_delta() {
        let config = config(include_str!("fixtures/cost_delta.router.yaml"));
        let instruments = config.to_instruments(Arc::new(config.new_static_instruments()));
        make_request(&instruments);

        assert_histogram_sum!("cost.delta", 90.0);
        make_request(&instruments);

        assert_histogram_sum!("cost.delta", 180.0);
    }

    #[test]
    fn test_default_estimated_with_attributes() {
        let config = config(include_str!(
            "fixtures/cost_estimated_with_attributes.router.yaml"
        ));
        let instruments = config.to_instruments(Arc::new(config.new_static_instruments()));
        make_request(&instruments);

        assert_histogram_sum!("cost.estimated", 100.0, cost.result = "COST_TOO_EXPENSIVE");
        make_request(&instruments);

        assert_histogram_sum!("cost.estimated", 200.0, cost.result = "COST_TOO_EXPENSIVE");
    }

    #[test]
    fn test_default_actual_with_attributes() {
        let config = config(include_str!(
            "fixtures/cost_actual_with_attributes.router.yaml"
        ));
        let instruments = config.to_instruments(Arc::new(config.new_static_instruments()));
        make_request(&instruments);

        assert_histogram_sum!("cost.actual", 10.0, cost.result = "COST_TOO_EXPENSIVE");
        make_request(&instruments);

        assert_histogram_sum!("cost.actual", 20.0, cost.result = "COST_TOO_EXPENSIVE");
    }

    #[test]
    fn test_default_delta_with_attributes() {
        let config = config(include_str!(
            "fixtures/cost_delta_with_attributes.router.yaml"
        ));
        let instruments = config.to_instruments(Arc::new(config.new_static_instruments()));
        make_request(&instruments);

        assert_histogram_sum!(
            "cost.delta",
            90.0,
            cost.result = "COST_TOO_EXPENSIVE",
            graphql.operation.name = "Test"
        );

        make_request(&instruments);
        assert_histogram_sum!(
            "cost.delta",
            180.0,
            cost.result = "COST_TOO_EXPENSIVE",
            graphql.operation.name = "Test"
        );
    }

    fn config(config: &'static str) -> CostInstrumentsConfig {
        let config: serde_json::Value = serde_yaml::from_str(config).expect("config");
        let supergraph_instruments = jsonpath_lib::select(&config, "$..supergraph");

        serde_json::from_value((*supergraph_instruments.unwrap().first().unwrap()).clone())
            .expect("config")
    }

    fn make_request(instruments: &CostInstruments) {
        let context = Context::new();
        context.insert(COST_ESTIMATED_CONTEXT_KEY, 100.0).unwrap();
        context.insert(COST_ACTUAL_CONTEXT_KEY, 10.0).unwrap();
        context
            .insert(COST_RESULT_CONTEXT_KEY, "COST_TOO_EXPENSIVE".to_string())
            .unwrap();
        let _ = context.insert(OPERATION_NAME, "Test".to_string()).unwrap();
        instruments.on_request(
            &supergraph::Request::fake_builder()
                .context(context.clone())
                .build()
                .expect("request"),
        );
        instruments.on_response(
            &supergraph::Response::fake_builder()
                .context(context.clone())
                .build()
                .expect("response"),
        );

        instruments.on_response_event(&crate::graphql::Response::default(), &context);
    }
}
