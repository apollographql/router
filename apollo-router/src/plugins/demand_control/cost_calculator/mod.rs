mod directives;
pub(in crate::plugins::demand_control) mod schema;
pub(crate) mod static_cost;

use std::collections::BTreeMap;
use std::collections::HashMap;
use std::ops::AddAssign;

use apollo_federation::query_plan::serializable_document::SerializableDocument;
use serde::ser::SerializeMap;

use crate::plugins::demand_control::DemandControlError;

/// Cost of a single fetch operation in the query plan.
#[derive(Clone, Debug)]
pub(crate) struct FetchCostEntry<'a> {
    pub(crate) subgraph: &'a str,
    pub(crate) cost: f64,
    /// The response path leading to this fetch (empty for root fetches).
    pub(crate) response_path: Vec<String>,
    /// The subgraph operation document (for breakdown building).
    pub(crate) operation: &'a SerializableDocument,
}

/// A tree node representing per-field cost breakdown.
///
/// Each node has an `own_cost` (the cost directly attributable to this field)
/// and children representing nested fields. The subtotal is own_cost plus the
/// sum of all children's subtotals.
#[derive(Clone, Default, Debug)]
pub(crate) struct CostBreakdownNode {
    pub(crate) own_cost: f64,
    pub(crate) children: BTreeMap<String, CostBreakdownNode>,
}

impl CostBreakdownNode {
    /// Additively merge another breakdown node into this one.
    pub(crate) fn merge(&mut self, other: CostBreakdownNode) {
        self.own_cost += other.own_cost;
        for (key, child) in other.children {
            self.children.entry(key).or_default().merge(child);
        }
    }

    /// Navigate to the given path and merge the node there.
    pub(crate) fn merge_at_path(&mut self, path: &[String], other: CostBreakdownNode) {
        if path.is_empty() {
            self.merge(other);
        } else {
            self.children
                .entry(path[0].clone())
                .or_default()
                .merge_at_path(&path[1..], other);
        }
    }

    /// Returns true if this node and all descendants have zero cost.
    fn is_empty(&self) -> bool {
        self.own_cost == 0.0 && self.children.values().all(|c| c.is_empty())
    }
}

#[cfg(test)]
impl CostBreakdownNode {
    /// Total cost of this node: own cost plus all descendants.
    pub(crate) fn subtotal(&self) -> f64 {
        self.own_cost + self.children.values().map(|c| c.subtotal()).sum::<f64>()
    }
}

impl serde::Serialize for CostBreakdownNode {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        // Count non-empty children + 1 for __cost
        let non_empty_children: Vec<_> = self
            .children
            .iter()
            .filter(|(_, c)| !c.is_empty())
            .collect();
        let mut map = serializer.serialize_map(Some(non_empty_children.len() + 1))?;
        map.serialize_entry("__cost", &self.own_cost)?;
        for (key, child) in non_empty_children {
            map.serialize_entry(key, child)?;
        }
        map.end()
    }
}

/// Result of costing a query plan: the raw per-fetch entries.
/// Aggregations (e.g. per-subgraph totals) are computed on demand.
pub(crate) struct PlanCostResult<'a> {
    pub(crate) entries: Vec<FetchCostEntry<'a>>,
}

impl<'a> PlanCostResult<'a> {
    pub(crate) fn new(entries: Vec<FetchCostEntry<'a>>) -> Self {
        Self { entries }
    }

    /// Aggregate per-fetch costs by subgraph.
    pub(crate) fn by_subgraph(&self) -> CostBySubgraph {
        let mut by_subgraph = CostBySubgraph::default();
        for entry in &self.entries {
            by_subgraph.add_or_insert(entry.subgraph, entry.cost);
        }
        by_subgraph
    }
}

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
