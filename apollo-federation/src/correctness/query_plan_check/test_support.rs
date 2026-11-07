//! What the checker's unit tests share: a buffer of what a plan has fetched, written as an
//! operation.

use apollo_compiler::ExecutableDocument;
use apollo_compiler::executable::Selection;

use crate::schema::ValidFederationSchema;

/// The root selections of an operation against `schema`, which is how a test writes what a plan
/// has fetched: the checker reads that rooted at the query type.
pub(super) fn selections(schema: &ValidFederationSchema, operation: &str) -> Vec<Selection> {
    ExecutableDocument::parse_and_validate(schema.schema(), operation, "op.graphql")
        .unwrap()
        .operations
        .get(None)
        .unwrap()
        .selection_set
        .selections
        .clone()
}

/// The selections under the first field named `key`, at any depth.
pub(super) fn under(selections: &[Selection], key: &str) -> Vec<Selection> {
    for selection in selections {
        let nested = match selection {
            Selection::Field(field) if field.response_name() == key => {
                return field.selection_set.selections.clone();
            }
            Selection::Field(field) => &field.selection_set.selections,
            Selection::InlineFragment(fragment) => &fragment.selection_set.selections,
            Selection::FragmentSpread(_) => continue,
        };
        let found = under(nested, key);
        if !found.is_empty() {
            return found;
        }
    }
    Vec::new()
}
