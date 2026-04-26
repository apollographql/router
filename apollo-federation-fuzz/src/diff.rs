//! Layer 4: run both planners on the same input and diff their plans.
//!
//! Plans are compared as `serde_json::Value` after light normalization. The
//! goal is to detect *semantic* divergence, not formatting differences — but
//! we are deliberately conservative for now and grow normalization rules
//! only when we hit a confirmed false-positive.

use serde_json::Value;

use std::panic::{AssertUnwindSafe, catch_unwind};

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
    /// At least one side panicked. The panic message is captured. The
    /// non-panicking side's result, if any, is retained for context.
    /// Captured separately from `EitherFailed` because a panic is a real
    /// planner bug (an explicit assertion or unwrap that the planner
    /// shouldn't be hitting on valid input) — sweep harnesses save these
    /// as reproducers rather than skipping them silently.
    PanickedSide {
        head_panic: Option<String>,
        base_panic: Option<String>,
        head_ok: Option<Value>,
        base_ok: Option<Value>,
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
    // Wrap each planner side in `catch_unwind` so a panic on one version
    // doesn't abort the sweep. We only catch at this layer; the planner's
    // own internal panics (e.g. invariant assertions in
    // `fetch_dependency_graph::process_root_nodes`) bubble up through
    // `Result::Err` here as a payload string, captured into
    // `PanickedSide`.
    let head_attempt = catch_unwind(AssertUnwindSafe(|| {
        H::build(supergraph_sdl, cfg).and_then(|p| p.plan(operation, operation_name, opts))
    }));
    let base_attempt = catch_unwind(AssertUnwindSafe(|| {
        B::build(supergraph_sdl, cfg).and_then(|p| p.plan(operation, operation_name, opts))
    }));

    let panic_msg = |payload: Box<dyn std::any::Any + Send>| -> String {
        if let Some(s) = payload.downcast_ref::<&'static str>() {
            (*s).to_string()
        } else if let Some(s) = payload.downcast_ref::<String>() {
            s.clone()
        } else {
            "<non-string panic payload>".to_string()
        }
    };

    let (head_panic, head_plan) = match head_attempt {
        Ok(r) => (None, Some(r)),
        Err(p) => (Some(panic_msg(p)), None),
    };
    let (base_panic, base_plan) = match base_attempt {
        Ok(r) => (None, Some(r)),
        Err(p) => (Some(panic_msg(p)), None),
    };
    if head_panic.is_some() || base_panic.is_some() {
        return DiffOutcome::PanickedSide {
            head_panic,
            base_panic,
            head_ok: head_plan.and_then(Result::ok),
            base_ok: base_plan.and_then(Result::ok),
        };
    }
    let head_plan = head_plan.expect("non-panicking branch returns Some");
    let base_plan = base_plan.expect("non-panicking branch returns Some");

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
                // `statistics` is planner metadata, not the plan itself.
                // Cross-version drift here is well-known noise: e.g.
                // `best_plan_cost` was added to QueryPlanningStatistics
                // after 2.1.3, so keeping it would flag every plan as
                // divergent for that single metadata field.
                if k == "statistics" {
                    continue;
                }
                // Drop null-valued keys: older versions (e.g. 2.0.0) serialize
                // absent options as `"x": null`, newer ones add
                // `#[serde(skip_serializing_if = "Option::is_none")]`. These
                // are semantically identical (field absent) but produce a
                // diff line on every plan otherwise.
                if v.is_null() {
                    continue;
                }
                // The `requires:` array on entity-fetch nodes was serialized
                // as raw SDL strings in 2.0.0 ("... on T0 { __typename id }")
                // and as a structured AST tree in newer versions. Same plan,
                // different wire format. Render both back to canonical SDL
                // strings so the diff layer flags only genuine algorithm
                // differences.
                if let ("requires", Value::Array(items)) = (k.as_str(), v) {
                    let canon: Vec<Value> = items
                        .iter()
                        .map(|item| match item {
                            Value::String(s) => Value::String(canonicalize_sdl(s)),
                            _ => Value::String(render_selection_node(item)),
                        })
                        .collect();
                    out.insert(k.clone(), Value::Array(canon));
                    continue;
                }
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

/// Render a selection-set AST node back to its SDL string form so the
/// raw-SDL representation (older versions) and the AST representation
/// (newer versions) collapse to the same canonical text.
fn render_selection_node(node: &Value) -> String {
    let obj = match node.as_object() {
        Some(o) => o,
        None => return String::new(),
    };
    let kind = obj.get("kind").and_then(Value::as_str).unwrap_or("");
    match kind {
        "Field" => {
            let name = obj.get("name").and_then(Value::as_str).unwrap_or("");
            let alias = obj.get("alias").and_then(Value::as_str);
            let mut out = String::new();
            if let Some(a) = alias {
                out.push_str(a);
                out.push_str(": ");
            }
            out.push_str(name);
            if let Some(Value::Array(sels)) = obj.get("selections") {
                out.push_str(" { ");
                let parts: Vec<String> = sels.iter().map(render_selection_node).collect();
                out.push_str(&parts.join(" "));
                out.push_str(" }");
            }
            out
        }
        "InlineFragment" => {
            let cond = obj.get("typeCondition").and_then(Value::as_str).unwrap_or("");
            let sels = obj
                .get("selections")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            let parts: Vec<String> = sels.iter().map(render_selection_node).collect();
            format!("... on {cond} {{ {} }}", parts.join(" "))
        }
        _ => String::new(),
    }
}

/// Trim and squash whitespace so trivially-different SDL strings collapse
/// to the same canonical form.
fn canonicalize_sdl(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
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
