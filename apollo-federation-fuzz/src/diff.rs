//! Layer 4: run both planners on the same input and diff their plans.
//!
//! Plans are compared as `serde_json::Value` after light normalization. The
//! goal is to detect *semantic* divergence, not formatting differences — but
//! we are deliberately conservative for now and grow normalization rules
//! only when we hit a confirmed false-positive.

use serde_json::Value;

use crate::harness::{CommonConfig, CommonOptions, HarnessError, PlannerHarness};

/// Run two planner versions against the same supergraph + operation and
/// return the diff outcome.
#[derive(Debug)]
pub enum DiffOutcome {
    /// Both planners produced byte-equal normalized plans.
    Identical {
        plan: Value,
    },
    /// Planners produced different plans.
    Divergent {
        head: Value,
        base: Value,
        unified_diff: String,
    },
    /// At least one side errored. `head` and `base` are the per-side results.
    EitherFailed {
        head: Result<Value, HarnessError>,
        base: Result<Value, HarnessError>,
    },
}

pub fn run_diff<H, B>(
    supergraph_sdl: &str,
    operation: &str,
    operation_name: Option<&str>,
    cfg: &CommonConfig,
    opts: &CommonOptions,
) -> DiffOutcome
where
    H: PlannerHarness,
    B: PlannerHarness,
{
    let head_plan = H::build(supergraph_sdl, cfg).and_then(|p| p.plan(operation, operation_name, opts));
    let base_plan = B::build(supergraph_sdl, cfg).and_then(|p| p.plan(operation, operation_name, opts));

    match (head_plan, base_plan) {
        (Ok(h), Ok(b)) => {
            let nh = normalize(&h);
            let nb = normalize(&b);
            if nh == nb {
                DiffOutcome::Identical { plan: nh }
            } else {
                let unified_diff = render_diff(&nh, &nb);
                DiffOutcome::Divergent {
                    head: nh,
                    base: nb,
                    unified_diff,
                }
            }
        }
        (h, b) => DiffOutcome::EitherFailed { head: h, base: b },
    }
}

/// Normalize a serialized plan so order-insensitive nodes (e.g. `ParallelNode`)
/// don't trigger spurious diffs. Conservative: only sorts arrays we know are
/// semantically a set.
fn normalize(value: &Value) -> Value {
    match value {
        Value::Object(map) => {
            let mut out = serde_json::Map::with_capacity(map.len());
            for (k, v) in map {
                let normalized = normalize(v);
                // The `Parallel` plan node has a `nodes` array whose ordering
                // is not semantically meaningful. We lex-sort by canonical
                // serialization. Note: we only do this when we're certain
                // the parent is a Parallel — checking shape via sibling keys.
                let is_parallel_nodes = k == "nodes"
                    && map.get("kind").and_then(Value::as_str) == Some("Parallel");
                if let (true, Value::Array(items)) = (is_parallel_nodes, &normalized) {
                    let mut sorted: Vec<Value> = items.clone();
                    sorted.sort_by_key(|v| serde_json::to_string(v).unwrap_or_default());
                    out.insert(k.clone(), Value::Array(sorted));
                } else {
                    out.insert(k.clone(), normalized);
                }
            }
            Value::Object(out)
        }
        Value::Array(items) => Value::Array(items.iter().map(normalize).collect()),
        other => other.clone(),
    }
}

fn render_diff(head: &Value, base: &Value) -> String {
    let head_pretty = serde_json::to_string_pretty(head).unwrap_or_default();
    let base_pretty = serde_json::to_string_pretty(base).unwrap_or_default();
    let diff = similar::TextDiff::from_lines(&base_pretty, &head_pretty);
    let mut out = String::with_capacity(head_pretty.len() + base_pretty.len());
    out.push_str("--- base\n+++ head\n");
    for change in diff.iter_all_changes() {
        let sign = match change.tag() {
            similar::ChangeTag::Delete => "-",
            similar::ChangeTag::Insert => "+",
            similar::ChangeTag::Equal => " ",
        };
        out.push_str(sign);
        out.push_str(change.value());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parallel_children_are_order_insensitive() {
        let a = json!({"kind": "Parallel", "nodes": [{"a": 1}, {"b": 2}]});
        let b = json!({"kind": "Parallel", "nodes": [{"b": 2}, {"a": 1}]});
        assert_eq!(normalize(&a), normalize(&b));
    }

    #[test]
    fn sequence_children_are_order_sensitive() {
        let a = json!({"kind": "Sequence", "nodes": [{"a": 1}, {"b": 2}]});
        let b = json!({"kind": "Sequence", "nodes": [{"b": 2}, {"a": 1}]});
        assert_ne!(normalize(&a), normalize(&b));
    }
}
