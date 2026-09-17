mod directives;
pub(in crate::plugins::demand_control) mod schema;
pub(crate) mod static_cost;

use std::collections::HashMap;
use std::ops::AddAssign;

use crate::plugins::demand_control::DemandControlError;

#[derive(Clone, Default, Debug, serde::Serialize, serde::Deserialize)]
pub(crate) struct CostBySubgraph(HashMap<String, f64>);
impl CostBySubgraph {
    pub(crate) fn new(subgraph: &str, value: f64) -> Self {
        Self(HashMap::from([(subgraph.to_string(), value)]))
    }

    pub(crate) fn add_or_insert(&mut self, subgraph: &str, value: f64) {
        if let Some(subgraph_cost) = self.0.get_mut(subgraph) {
            *subgraph_cost += value;
        } else {
            self.0.insert(subgraph.to_string(), value);
        }
    }

    pub(crate) fn get(&self, subgraph: &str) -> Option<f64> {
        self.0.get(subgraph).copied()
    }

    pub(crate) fn total(&self) -> f64 {
        self.0.values().sum()
    }
}

impl AddAssign for CostBySubgraph {
    fn add_assign(&mut self, rhs: Self) {
        for (subgraph, value) in rhs.0.into_iter() {
            if let Some(subgraph_cost) = self.0.get_mut(&subgraph) {
                *subgraph_cost += value;
            } else {
                self.0.insert(subgraph, value);
            }
        }
    }
}

#[cfg(test)]
impl From<&[(&str, f64)]> for CostBySubgraph {
    fn from(values: &[(&str, f64)]) -> Self {
        let mut cost = Self(HashMap::default());
        for (subgraph, value) in values {
            cost.add_or_insert(subgraph, *value);
        }
        cost
    }
}
