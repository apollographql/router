mod directives;
pub(in crate::plugins::demand_control) mod schema;
pub(crate) mod static_cost;

use std::collections::HashMap;

use crate::plugins::demand_control::DemandControlError;

#[derive(Clone, Default, Debug, serde::Serialize, serde::Deserialize)]
pub(crate) struct CostBySubgraph(HashMap<String, f64>);
impl CostBySubgraph {
    pub(crate) fn add_or_insert(&mut self, subgraph: &str, value: f64) {
        if let Some(subgraph_cost) = self.0.get_mut(subgraph) {
            *subgraph_cost += value;
        } else {
            self.0.insert(subgraph.to_string(), value);
        }
    }

    pub(crate) fn total(&self) -> f64 {
        self.0.values().sum()
    }
}
