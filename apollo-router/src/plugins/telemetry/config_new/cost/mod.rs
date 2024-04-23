use crate::plugins::demand_control::CostResult;
use crate::plugins::telemetry::config_new::attributes::SupergraphAttributes;
use crate::plugins::telemetry::config_new::conditions::Condition;
use crate::plugins::telemetry::config_new::extendable::Extendable;
use crate::plugins::telemetry::config_new::instruments::Increment::Unit;
use crate::plugins::telemetry::config_new::instruments::{
    CustomHistogram, CustomHistogramInner, DefaultedStandardInstrument, Instrumented,
};
use crate::plugins::telemetry::config_new::selectors::SupergraphSelector;
use crate::plugins::telemetry::config_new::Selectors;
use crate::services::supergraph;
use crate::{metrics, Context};
use opentelemetry::metrics::MeterProvider;
use opentelemetry_api::KeyValue;
use parking_lot::Mutex;
use schemars::JsonSchema;
use serde::Deserialize;
use std::sync::Arc;
use tower::BoxError;

/// Attributes for Cost
#[derive(Deserialize, JsonSchema, Clone, Default, Debug)]
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

        if let Some(true) = self.cost_estimated {
            if let Some(cost_result) = &response.context.extensions().lock().get::<CostResult>() {
                attrs.push(KeyValue::new("cost.estimated", cost_result.estimated));
            }
        }
        if let Some(true) = self.cost_actual {
            if let Some(cost_result) = &response.context.extensions().lock().get::<CostResult>() {
                attrs.push(KeyValue::new("cost.actual", cost_result.actual));
            }
        }
        if let Some(true) = self.cost_delta {
            if let Some(cost_result) = &response.context.extensions().lock().get::<CostResult>() {
                attrs.push(KeyValue::new("cost.delta", cost_result.delta()));
            }
        }
        if let Some(true) = self.cost_result {
            if let Some(cost_result) = &response.context.extensions().lock().get::<CostResult>() {
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
        let meter = metrics::meter_provider()
            .meter(crate::plugins::telemetry::config_new::instruments::METER_NAME);

        let cost_estimated = self.cost_estimated.is_enabled().then(|| {
            let mut nb_attributes = 0;
            let selectors = match &self.cost_estimated {
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
                    histogram: Some(
                        meter
                            .f64_histogram("apollo.router.operation.cost.estimated")
                            .init(),
                    ),
                    attributes: Vec::with_capacity(nb_attributes),
                    selector: Some(Arc::new(SupergraphSelector::Cost {
                        cost: CostValue::Estimated,
                    })),
                    selectors,
                }),
            }
        });

        let cost_actual = self.cost_actual.is_enabled().then(|| {
            let mut nb_attributes = 0;
            let selectors = match &self.cost_actual {
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
                    histogram: Some(
                        meter
                            .f64_histogram("apollo.router.operation.cost.actual")
                            .init(),
                    ),
                    attributes: Vec::with_capacity(nb_attributes),
                    selector: Some(Arc::new(SupergraphSelector::Cost {
                        cost: CostValue::Actual,
                    })),
                    selectors,
                }),
            }
        });

        let cost_delta = self.cost_estimated.is_enabled().then(|| {
            let mut nb_attributes = 0;
            let selectors = match &self.cost_delta {
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
                    histogram: Some(
                        meter
                            .f64_histogram("apollo.router.operation.cost.delta")
                            .init(),
                    ),
                    attributes: Vec::with_capacity(nb_attributes),
                    selector: Some(Arc::new(SupergraphSelector::Cost {
                        cost: CostValue::Delta,
                    })),
                    selectors,
                }),
            }
        });
        CostInstruments {
            cost_estimated,
            cost_actual,
            cost_delta,
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

#[derive(Deserialize, JsonSchema, Clone, Debug)]
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
