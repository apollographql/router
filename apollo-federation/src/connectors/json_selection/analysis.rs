//! [`SelectionAnalysis`] is the primary artifact produced by statically
//! analyzing a [`JSONSelection`]. It caches everything a downstream consumer
//! typically wants to ask of a selection — the output shape, the per-variable
//! input-consumption trie — behind a cheap-to-clone [`Arc`]-wrapping handle.
//!
//! The consumption trie is computed as a byproduct of producing the analysis,
//! not as a standalone traversal, so callers that need both the output shape
//! and the consumption view only pay for one pass over the AST. (Fusing the
//! two traversals literally into one is a follow-up; the first landing just
//! fixes the public API.)
//!
//! # Example
//!
//! ```ignore
//! use std::sync::Arc;
//! # use apollo_federation::connectors::json_selection::{JSONSelection, SelectionAnalysis};
//! # use apollo_federation::connectors::ConnectSpec;
//! let selection = Arc::new(JSONSelection::parse_with_spec(
//!     "id name: $args.name",
//!     ConnectSpec::V0_4,
//! ).unwrap());
//! let analysis = SelectionAnalysis::new(selection);
//!
//! // What inputs does this selection consume?
//! let args_trie = analysis.consumption().get("$args");
//! let root_trie = analysis.consumption().get("$root");
//! ```

#![allow(dead_code)] // Consumers land in follow-up PRs (requestless diagnostic,
// runtime materialization of only-consumed-fields).

use std::sync::Arc;

use shape::Shape;
use shape::location::SourceId;

use super::JSONSelection;
use super::SelectionTrie;
use super::apply_to::ShapeContext;

/// Static analysis of a [`JSONSelection`]. Holds a shared handle to the
/// original selection plus cached views derived from it.
///
/// The canonical entry point is [`SelectionAnalysis::new`], which eagerly
/// computes every cached view. Re-running the analysis against a different
/// input shape is cheap via [`SelectionAnalysis::with_input_shape`] because
/// the underlying selection is [`Arc`]-shared.
#[derive(Debug, Clone)]
pub(crate) struct SelectionAnalysis {
    /// The selection under analysis. Arc-wrapped so that [`SelectionAnalysis`]
    /// is cheap to clone and share across threads / call sites.
    selection: Arc<JSONSelection>,

    /// The static output shape of the selection, computed once with the
    /// symbolic `$root` shape as its input. Downstream consumers can re-run
    /// the analysis with a concrete input shape via
    /// [`Self::with_input_shape`].
    output_shape: Shape,

    /// A trie of every input path the selection consumes, keyed at the top
    /// level by variable name (`$root`, `$args`, `$this`, `$config`, …).
    /// Empty tries for namespaces the selection never touches.
    consumption: SelectionTrie,
}

impl SelectionAnalysis {
    /// Analyze the given selection. Performs the shape and consumption-trie
    /// computations eagerly so the results are ready for later queries
    /// without further work.
    ///
    /// Accepts anything convertible into `Arc<JSONSelection>`, so both an
    /// owned `JSONSelection` (via std's blanket `From<T> for Arc<T>`) and an
    /// existing `Arc<JSONSelection>` work without ceremony at the call site.
    pub(crate) fn new(selection: impl Into<Arc<JSONSelection>>) -> Self {
        let selection: Arc<JSONSelection> = selection.into();
        let context =
            ShapeContext::new(SourceId::Other("JSONSelection".into())).with_spec(selection.spec());
        let output_shape =
            selection.compute_output_shape(&context, Shape::name("$root", Vec::new()));
        let consumption = context.consumption().borrow().clone();
        Self {
            selection,
            output_shape,
            consumption,
        }
    }

    /// The selection this analysis was computed from. The returned `Arc`
    /// is cheap to clone, so callers can take ownership without scoping
    /// the borrow against the analysis.
    pub(crate) fn selection(&self) -> Arc<JSONSelection> {
        Arc::clone(&self.selection)
    }

    /// The static output shape of the selection.
    ///
    /// By default this is computed with the symbolic `$root` shape standing
    /// in for the input data — so paths the selection reads from the input
    /// show up in the output shape as subpaths of `$root` (e.g.
    /// `$root.books.4.isbn`). Use [`Self::with_input_shape`] to re-run the
    /// analysis against a concrete input shape.
    pub(crate) fn output_shape(&self) -> Shape {
        self.output_shape.clone()
    }

    /// Per-variable consumption trie. Top-level keys are variable names
    /// (`$root`, `$args`, `$this`, `$config`, `$context`, `$status`,
    /// `$request`, `$response`, `$env`, `$batch`); each subtree describes
    /// the subpaths of that variable the selection reads.
    ///
    /// An empty subtree for a given variable means the selection does not
    /// consume that variable at all — which, for `$root` on an HTTP-backed
    /// connector, means the HTTP response body is unused.
    pub(crate) fn consumption(&self) -> &SelectionTrie {
        &self.consumption
    }

    /// Re-analyze the same selection with a different input shape in place
    /// of the symbolic `$root`. The selection is shared via [`Arc`], so the
    /// only work done is recomputing the shape and consumption views.
    pub(crate) fn with_input_shape(&self, input: Shape) -> Self {
        let context = ShapeContext::new(SourceId::Other("JSONSelection".into()))
            .with_spec(self.selection.spec());
        let output_shape = self.selection.compute_output_shape(&context, input);
        let consumption = context.consumption().borrow().clone();
        Self {
            selection: Arc::clone(&self.selection),
            output_shape,
            consumption,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::SelectionAnalysis;
    use crate::connectors::ConnectSpec;
    use crate::connectors::json_selection::JSONSelection;

    fn analyze(input: &str) -> SelectionAnalysis {
        analyze_with_spec(input, ConnectSpec::V0_4)
    }

    fn analyze_with_spec(input: &str, spec: ConnectSpec) -> SelectionAnalysis {
        let selection = JSONSelection::parse_with_spec(input, spec).expect("valid selection");
        SelectionAnalysis::new(selection)
    }

    #[test]
    fn consumption_for_simple_field_selections() {
        // For selections that read directly from `$root` with no aliasing,
        // the fused consumption trie agrees with the legacy
        // `compute_selection_trie` walker.
        let cases = [
            ("a { b { c } d { e } }", "$root { a { b { c } d { e } } }"),
            ("id name email", "$root { email id name }"),
        ];
        for (input, expected) in cases {
            let analysis = analyze(input);
            assert_eq!(
                analysis.consumption().to_string(),
                expected,
                "consumption trie mismatch for {input:?}",
            );
        }
    }

    #[test]
    fn aliased_subselection_records_structural_root_navigation() {
        // Even though every leaf value here comes from `$args` / `$this`,
        // the structural path `$root.a.b` (and `$root.a.d`) was still
        // navigated to: those paths must exist on the upstream input for
        // the selection to make sense. The trie records that structural
        // consumption alongside the variable consumption.
        let analysis = analyze("a { b { c: $args.c } d { e: $this.e } }");
        assert_eq!(
            analysis.consumption().to_string(),
            "$args { c } $root { a { b d } } $this { e }",
        );
    }

    #[test]
    fn root_consumption_for_bare_subselection() {
        let analysis = analyze("a { b { c } d { e } }");
        let root = analysis.consumption().get("$root").expect("$root entry");
        assert_eq!(root.to_string(), "a { b { c } d { e } }");
    }

    #[test]
    fn args_and_this_consumption() {
        let analysis = analyze("id: $args.id name: $this.name email: $args.contact.email");
        let args = analysis.consumption().get("$args").expect("$args entry");
        assert_eq!(args.to_string(), "contact { email } id");
        let this = analysis.consumption().get("$this").expect("$this entry");
        assert_eq!(this.to_string(), "name");
    }

    #[test]
    fn pure_literal_selection_has_no_root_consumption() {
        // A selection that only emits literal values never reads from the
        // input data. `$root` either is absent or present as an empty trie;
        // either way, `root.is_empty()` should hold.
        let analysis = analyze("answer: $(42) greeting: $(\"hello\")");
        match analysis.consumption().get("$root") {
            None => {}
            Some(root) => assert!(root.is_empty(), "expected no $root consumption, got {root}",),
        }
    }

    // ---- RH-1345 / CNN-1093 regression coverage at the SelectionAnalysis
    // level. These tests assert directly on the consumption trie produced
    // by `SelectionAnalysis::new` rather than going through the validator,
    // so a regression in the fused-trie machinery surfaces here even if
    // the validator path is unchanged. ----

    #[test]
    fn rh_1345_filter_with_subselection_records_full_consumption() {
        // Regression for RH-1345 / CNN-1093: a selection of the form
        // `$this.items->filter(@.product) { id name }` must record
        // consumption of `items { id name product }` under `$this`.
        // Pre-fix the legacy walker truncated at the method boundary and
        // recorded `items` as a leaf, which produced `@key(fields:
        // "items")` and `CONNECTORS_CANNOT_RESOLVE_KEY` at composition
        // time.
        let analysis = analyze("$this.items->filter(@.product) { id name }");
        let this = analysis.consumption().get("$this").expect("$this entry");
        assert_eq!(this.to_string(), "items { id name product }");
    }

    #[test]
    fn filter_predicate_consumption_without_subselection() {
        // The predicate's `@.product` is recorded inside the filtered
        // array's element path even when no subselection follows the
        // method call. Confirms predicate consumption is independent of
        // post-method subselection recovery.
        let analysis = analyze("$this.items->filter(@.product)");
        let this = analysis.consumption().get("$this").expect("$this entry");
        assert_eq!(this.to_string(), "items { product }");
    }

    #[test]
    fn slice_with_subselection_recurses_into_element_shape() {
        // `->slice` returns a sub-array of the input, so post-method
        // subselection consumes from the same element shape as the input.
        // This is a different method from `->filter` but the same
        // shape-preserving structural pattern.
        let analysis = analyze("$this.items->slice(0, 5) { id name }");
        let this = analysis.consumption().get("$this").expect("$this entry");
        assert_eq!(this.to_string(), "items { id name }");
    }

    #[test]
    fn size_terminates_consumption_at_method_boundary() {
        // `->size` returns a scalar; there is no element shape to recurse
        // into. Consumption ends at `items` (the input the method is
        // called on). Demonstrates that the fused trie consults
        // `method.shape()` rather than blindly recursing through any
        // method's tail.
        let analysis = analyze("count: $this.items->size");
        let this = analysis.consumption().get("$this").expect("$this entry");
        assert_eq!(this.to_string(), "items");
    }

    #[test]
    fn chained_filter_then_slice_with_subselection() {
        // Both `->filter` and `->slice` are shape-preserving; chaining
        // them followed by a subselection records the union of the
        // predicate consumption (`active`) and the post-method
        // subselection (`id`) within the array's element shape.
        let analysis = analyze("$this.items->filter(@.active)->slice(0, 3) { id }");
        let this = analysis.consumption().get("$this").expect("$this entry");
        assert_eq!(this.to_string(), "items { active id }");
    }

    #[test]
    fn output_shape_is_cached_and_stable() {
        let analysis = analyze("id name");
        let first = analysis.output_shape().pretty_print();
        let second = analysis.output_shape().pretty_print();
        assert_eq!(first, second);
    }

    #[test]
    fn selection_is_arc_shared() {
        let selection =
            Arc::new(JSONSelection::parse_with_spec("id", ConnectSpec::V0_4).expect("valid"));
        let before = Arc::strong_count(&selection);
        let analysis = SelectionAnalysis::new(Arc::clone(&selection));
        assert_eq!(Arc::strong_count(&selection), before + 1);
        // Cloning the analysis must not clone the underlying selection.
        #[allow(clippy::redundant_clone)] // Hold the clone alive across the
        // strong_count assertion below — the whole point of the test.
        let cloned = analysis.clone();
        assert_eq!(Arc::strong_count(&selection), before + 2);
        drop(cloned);
        assert_eq!(Arc::strong_count(&selection), before + 1);
    }
}
