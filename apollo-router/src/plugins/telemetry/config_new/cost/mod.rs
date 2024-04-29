use std::sync::Arc;

use opentelemetry::metrics::MeterProvider;
use opentelemetry_api::KeyValue;
use parking_lot::Mutex;
use schemars::JsonSchema;
use serde::Deserialize;
use tower::BoxError;

use crate::metrics;
use crate::plugins::demand_control::CostContext;
use crate::plugins::telemetry::config_new::attributes::SupergraphAttributes;
use crate::plugins::telemetry::config_new::conditions::Condition;
use crate::plugins::telemetry::config_new::extendable::Extendable;
use crate::plugins::telemetry::config_new::instruments::CustomHistogram;
use crate::plugins::telemetry::config_new::instruments::CustomHistogramInner;
use crate::plugins::telemetry::config_new::instruments::DefaultedStandardInstrument;
use crate::plugins::telemetry::config_new::instruments::Increment::Unit;
use crate::plugins::telemetry::config_new::instruments::Instrumented;
use crate::plugins::telemetry::config_new::selectors::SupergraphSelector;
use crate::plugins::telemetry::config_new::Selectors;
use crate::services::supergraph;
use crate::services::supergraph::Request;
use crate::services::supergraph::Response;
use crate::Context;

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

    fn on_request(&self, _request: &Self::Request) -> Vec<KeyValue> {
        Vec::default()
    }

    fn on_response(&self, response: &Self::Response) -> Vec<KeyValue> {
        let mut attrs = Vec::with_capacity(4);
        if let Some(cost_result) = &response.context.extensions().lock().get::<CostContext>() {
            if let Some(true) = self.cost_estimated {
                attrs.push(KeyValue::new("cost.estimated", cost_result.estimated));
            }
            if let Some(true) = self.cost_actual {
                attrs.push(KeyValue::new("cost.actual", cost_result.actual));
            }
            if let Some(true) = self.cost_delta {
                attrs.push(KeyValue::new("cost.delta", cost_result.delta()));
            }
            if let Some(true) = self.cost_result {
                attrs.push(KeyValue::new("cost.result", cost_result.result));
            }
        }
        attrs
    }

    fn on_error(&self, _error: &BoxError) -> Vec<KeyValue> {
        Vec::default()
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
    pub(crate) fn to_instruments(&self) -> CostInstruments {
        let cost_estimated = self.cost_estimated.is_enabled().then(|| {
            Self::histogram(
                COST_ESTIMATED,
                &self.cost_estimated,
                SupergraphSelector::Cost {
                    cost: CostValue::Estimated,
                },
            )
        });

        let cost_actual = self.cost_actual.is_enabled().then(|| {
            Self::histogram(
                COST_ACTUAL,
                &self.cost_actual,
                SupergraphSelector::Cost {
                    cost: CostValue::Actual,
                },
            )
        });

        let cost_delta = self.cost_delta.is_enabled().then(|| {
            Self::histogram(
                COST_DELTA,
                &self.cost_delta,
                SupergraphSelector::Cost {
                    cost: CostValue::Delta,
                },
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
    ) -> CustomHistogram<Request, Response, SupergraphAttributes, SupergraphSelector> {
        let meter = metrics::meter_provider()
            .meter(crate::plugins::telemetry::config_new::instruments::METER_NAME);
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
                increment: Unit,
                condition: Condition::True,
                histogram: Some(meter.f64_histogram(name).init()),
                attributes: Vec::with_capacity(nb_attributes),
                selector: Some(Arc::new(selector)),
                selectors,
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

#[cfg(test)]
mod test {
    use crate::plugins::demand_control::CostContext;
    use crate::plugins::telemetry::config_new::cost::CostInstruments;
    use crate::plugins::telemetry::config_new::cost::CostInstrumentsConfig;
    use crate::plugins::telemetry::config_new::instruments::Instrumented;
    use crate::services::supergraph;
    use crate::Context;

    #[test]
    fn test_default_estimated() {
        let config = config(include_str!("fixtures/cost_estimated.router.yaml"));
        let instruments = config.to_instruments();
        make_request(instruments);

        assert_histogram_sum!("cost.estimated", 100.0);
    }

    #[test]
    fn test_default_actual() {
        let config = config(include_str!("fixtures/cost_actual.router.yaml"));
        let instruments = config.to_instruments();
        make_request(instruments);

        assert_histogram_sum!("cost.actual", 10.0);
    }

    #[test]
    fn test_default_delta() {
        let config = config(include_str!("fixtures/cost_delta.router.yaml"));
        let instruments = config.to_instruments();
        make_request(instruments);

        assert_histogram_sum!("cost.delta", 90.0);
    }

    #[test]
    fn test_default_estimated_with_attributes() {
        let config = config(include_str!(
            "fixtures/cost_estimated_with_attributes.router.yaml"
        ));
        let instruments = config.to_instruments();
        make_request(instruments);

        assert_histogram_sum!("cost.estimated", 100.0, cost.result = "COST_TOO_EXPENSIVE");
    }

    #[test]
    fn test_default_actual_with_attributes() {
        let config = config(include_str!(
            "fixtures/cost_actual_with_attributes.router.yaml"
        ));
        let instruments = config.to_instruments();
        make_request(instruments);

        assert_histogram_sum!("cost.actual", 10.0, cost.result = "COST_TOO_EXPENSIVE");
    }

    #[test]
    fn test_default_delta_with_attributes() {
        let config = config(include_str!(
            "fixtures/cost_delta_with_attributes.router.yaml"
        ));
        let instruments = config.to_instruments();
        make_request(instruments);

        assert_histogram_sum!("cost.delta", 90.0, cost.result = "COST_TOO_EXPENSIVE");
    }

    fn config(config: &'static str) -> CostInstrumentsConfig {
        let config: serde_json::Value = serde_yaml::from_str(config).expect("config");
        let supergraph_instruments = jsonpath_lib::select(&config, "$..supergraph");

        serde_json::from_value((*supergraph_instruments.unwrap().first().unwrap()).clone())
            .expect("config")
    }

    fn make_request(instruments: CostInstruments) {
        let context = Context::new();
        {
            let mut extensions = context.extensions().lock();
            extensions.insert(CostContext::default());
            let cost_result = extensions.get_or_default_mut::<CostContext>();
            cost_result.estimated = 100.0;
            cost_result.actual = 10.0;
            cost_result.result = "COST_TOO_EXPENSIVE"
        }
        instruments.on_request(
            &supergraph::Request::fake_builder()
                .context(context.clone())
                .build()
                .expect("request"),
        );
        instruments.on_response(
            &supergraph::Response::fake_builder()
                .context(context)
                .build()
                .expect("response"),
        );
    }
}
