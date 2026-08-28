//! Key-hop input path helpers: building the unconditioned input path and
//! extracting trailing condition fragments for entity fetch op paths.

use std::sync::Arc;

use super::super::shared_path::SharedPath;
use crate::operation::DirectiveList;
use crate::query_graph::graph_path::operation::OpPathElement;

/// The trailing inline-fragment elements of `op_path` (after the last field)
/// that carry directives: @skip/@include conditions at the current position,
/// which a key hop must carry into the entity fetch's op path or the hopped
/// selections lose their gating.
pub(super) fn trailing_condition_fragments(
    op_path: &SharedPath<Arc<OpPathElement>>,
) -> Vec<Arc<OpPathElement>> {
    let mut trailing = Vec::new();
    for element in op_path.iter() {
        match element.as_ref() {
            OpPathElement::Field(_) => trailing.clear(),
            OpPathElement::InlineFragment(frag) => {
                if !frag.directives.is_empty() {
                    trailing.push(element.clone());
                }
            }
        }
    }
    trailing
}

/// `op_path` with @skip/@include stripped from its inline-fragment elements,
/// for appending key input selections. Inputs must be selected
/// unconditionally: an input gated by one branch's Boolean condition leaves
/// the representation incomplete whenever a different branch executes.
/// Condition-only fragments are dropped; type-conditioned fragments keep the
/// downcast without the conditions.
pub(super) fn unconditioned_input_path(
    op_path: &SharedPath<Arc<OpPathElement>>,
) -> SharedPath<Arc<OpPathElement>> {
    let mut elements = Vec::with_capacity(op_path.len());
    for element in op_path.iter() {
        match element.as_ref() {
            OpPathElement::Field(_) => elements.push(element.clone()),
            OpPathElement::InlineFragment(frag) => {
                let stripped: DirectiveList = frag
                    .directives
                    .iter()
                    .filter(|d| d.name != "skip" && d.name != "include")
                    .cloned()
                    .collect();
                if stripped.len() == frag.directives.len() {
                    elements.push(element.clone());
                } else if !stripped.is_empty() || frag.type_condition_position.is_some() {
                    elements.push(Arc::new(OpPathElement::InlineFragment(
                        frag.with_updated_directives(stripped),
                    )));
                }
            }
        }
    }
    SharedPath::from_vec(elements)
}
