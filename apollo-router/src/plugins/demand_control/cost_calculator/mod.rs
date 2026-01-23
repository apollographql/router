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

    pub(crate) fn total(&self) -> f64 {
        self.0.values().sum()
    }

    /// Creates a new `CostBySubgraph` where each value in the map is the maximum of its value
    /// in the two input `CostBySubgraph`s.
    ///
    /// ```rust
    /// let cost1 = CostBySubgraph::new("hello", 1.0);
    /// let mut cost2 = CostBySubgraph::new("hello", 2.0);
    /// cost2.add_or_insert("world", 1.0);
    ///
    /// let max = CostBySubgraph::maximum(cost1, cost2);
    /// assert_eq!(max.0.get("hello"), Some(2.0));
    /// assert_eq!(max.0.get("world"), Some(1.0));
    /// ```
    pub(crate) fn maximum(mut cost1: Self, cost2: Self) -> Self {
        for (subgraph, value) in cost2.0.into_iter() {
            if let Some(subgraph_cost) = cost1.0.get_mut(&subgraph) {
                *subgraph_cost = subgraph_cost.max(value);
            } else {
                cost1.0.insert(subgraph, value);
            }
        }

        cost1
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
