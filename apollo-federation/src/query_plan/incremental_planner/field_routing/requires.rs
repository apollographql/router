//! Key-hop input path helpers: building the unconditioned input path and
//! extracting trailing condition fragments for entity fetch op paths.

use std::sync::Arc;

use super::super::shared_path::SharedPath;
use crate::operation::DirectiveList;
use crate::query_graph::graph_path::operation::OpPathElement;

/// The trailing inline-fragment elements of `op_path` (after the last field)
/// that carry @skip/@include conditions at the current position, which a key
/// hop must carry into the entity fetch's op path or the hopped selections
/// lose their gating. Only the condition directives are carried. Conditions
/// before the last field are deliberately kept by neither helper: the
/// parent fetch's data dependence already gates them.
pub(super) fn trailing_condition_fragments(
    op_path: &SharedPath<Arc<OpPathElement>>,
) -> Vec<Arc<OpPathElement>> {
    let mut trailing = Vec::new();
    for element in op_path.iter() {
        match element.as_ref() {
            OpPathElement::Field(_) => trailing.clear(),
            OpPathElement::InlineFragment(frag) => {
                let conditions: DirectiveList = frag
                    .directives
                    .iter()
                    .filter(|d| d.name == "skip" || d.name == "include")
                    .cloned()
                    .collect();
                if conditions.is_empty() {
                    continue;
                }
                if conditions.len() == frag.directives.len() {
                    trailing.push(element.clone());
                } else {
                    trailing.push(Arc::new(OpPathElement::InlineFragment(
                        frag.with_updated_directives(conditions),
                    )));
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

#[cfg(test)]
mod tests {
    use apollo_compiler::name;

    use super::*;
    use crate::operation::InlineFragment;
    use crate::operation::SelectionId;
    use crate::schema::ValidFederationSchema;
    use crate::schema::position::CompositeTypeDefinitionPosition;
    use crate::schema::position::ObjectTypeDefinitionPosition;

    fn fragment_with_directive(directive: &str) -> Arc<OpPathElement> {
        let schema = apollo_compiler::schema::Schema::parse_and_validate(
            "type Query { x: Int }",
            "schema.graphql",
        )
        .expect("valid schema");
        let schema = ValidFederationSchema::new(schema).expect("valid federation schema");
        let parent: CompositeTypeDefinitionPosition =
            CompositeTypeDefinitionPosition::Object(ObjectTypeDefinitionPosition {
                type_name: name!("Query"),
            });
        let directives: DirectiveList = std::iter::once(apollo_compiler::ast::Directive::new(
            apollo_compiler::Name::new(directive).expect("valid name"),
        ))
        .collect();
        Arc::new(OpPathElement::InlineFragment(InlineFragment {
            schema,
            parent_type_position: parent.clone(),
            type_condition_position: Some(parent),
            directives,
            selection_id: SelectionId::new(),
        }))
    }

    /// Only @skip/@include are Boolean conditions a key hop must replay
    /// into the entity fetch. A fragment carrying an unrelated directive
    /// (e.g. @defer) is not a condition, matching the filter used by
    /// `unconditioned_input_path`.
    #[test]
    fn trailing_condition_fragments_only_match_skip_include() {
        let skip = SharedPath::new().pushed(fragment_with_directive("skip"));
        assert_eq!(trailing_condition_fragments(&skip).len(), 1);

        let other = SharedPath::new().pushed(fragment_with_directive("defer"));
        assert_eq!(
            trailing_condition_fragments(&other).len(),
            0,
            "a non-condition directive must not be treated as @skip/@include",
        );
    }
}
