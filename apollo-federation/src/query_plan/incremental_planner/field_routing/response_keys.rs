//! Response keys the client operation claims, for deciding whether a
//! @requires condition field can share a key with other selections or must
//! be aliased.
//!
//! Condition fields never carry aliases, so two argument-less condition
//! fields with the same name are the same field and always merge. A key can
//! only be contested by the client (a different field or arguments under
//! that name) or by another condition that passes different arguments.

use apollo_compiler::Name;
use hashbrown::HashMap;

use crate::operation::ArgumentList;
use crate::operation::Field;
use crate::operation::FieldSelection;
use crate::operation::Selection;
use crate::operation::SelectionSet;
use crate::operation::TYPENAME_FIELD;
use crate::query_plan::FetchDataPathElement;

/// What a selection under a response key resolves to: the field and its
/// arguments. Directives are left out because they do not affect merging.
#[derive(Clone, Debug, PartialEq, Eq)]
struct FieldIdentity {
    name: Name,
    arguments: ArgumentList,
}

impl FieldIdentity {
    fn of(field: &Field) -> Self {
        Self {
            name: field.name().clone(),
            arguments: field.arguments.clone(),
        }
    }
}

/// The identity of an aliased condition field: the field plus every
/// argument-bearing field below it, by path. Two conditions with the same
/// identity can share one alias, since nothing under it can conflict.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AliasIdentity {
    field: FieldIdentity,
    nested: Vec<(Vec<Name>, FieldIdentity)>,
}

impl AliasIdentity {
    pub(crate) fn of(field_sel: &FieldSelection) -> Self {
        let mut nested = Vec::new();
        if let Some(sub) = &field_sel.selection_set {
            collect_argument_fields(sub, &mut Vec::new(), &mut nested);
        }
        Self {
            field: FieldIdentity::of(&field_sel.field),
            nested,
        }
    }
}

fn collect_argument_fields(
    selection_set: &SelectionSet,
    path: &mut Vec<Name>,
    out: &mut Vec<(Vec<Name>, FieldIdentity)>,
) {
    for selection in selection_set.selections.values() {
        match selection {
            Selection::Field(field_sel) => {
                let field = &field_sel.field;
                if !field.arguments.is_empty() {
                    let entry = (path.clone(), FieldIdentity::of(field));
                    if !out.contains(&entry) {
                        out.push(entry);
                    }
                }
                if let Some(sub) = &field_sel.selection_set {
                    path.push(field.response_name().clone());
                    collect_argument_fields(sub, path, out);
                    path.pop();
                }
            }
            Selection::InlineFragment(frag) => {
                collect_argument_fields(&frag.selection_set, path, out);
            }
        }
    }
}

/// Field identities the client operation selects, by response path and
/// response name. Inline fragments are transparent, so selections on
/// different types under one key are pooled, which can only add aliases.
#[derive(Debug, Default)]
pub(crate) struct OperationResponseKeys {
    claims: HashMap<Vec<Name>, HashMap<Name, Vec<FieldIdentity>>>,
}

impl OperationResponseKeys {
    pub(crate) fn new(selection_set: &SelectionSet) -> Self {
        let mut keys = Self::default();
        keys.collect(selection_set, &mut Vec::new());
        keys
    }

    fn collect(&mut self, selection_set: &SelectionSet, path: &mut Vec<Name>) {
        for selection in selection_set.selections.values() {
            match selection {
                Selection::Field(field_sel) => {
                    let field = &field_sel.field;
                    let identity = FieldIdentity::of(field);
                    let claimed = self
                        .claims
                        .entry(path.clone())
                        .or_default()
                        .entry(field.response_name().clone())
                        .or_default();
                    if !claimed.contains(&identity) {
                        claimed.push(identity);
                    }
                    if let Some(sub) = &field_sel.selection_set {
                        path.push(field.response_name().clone());
                        self.collect(sub, path);
                        path.pop();
                    }
                }
                Selection::InlineFragment(frag) => self.collect(&frag.selection_set, path),
            }
        }
    }

    /// Whether a condition field selected at `merge_at` must be aliased:
    /// it or a field below it could share a response key with a different
    /// field. A field with arguments is safe only when the client selects
    /// exactly it, which keeps another condition from claiming that key
    /// with other arguments.
    pub(crate) fn condition_needs_alias(
        &self,
        merge_at: &[FetchDataPathElement],
        field_sel: &FieldSelection,
    ) -> bool {
        match response_path(merge_at) {
            Some(mut path) => self.field_needs_alias(&mut path, field_sel),
            // A path step that is not a response key cannot be looked up, so
            // keep the condition apart from everything else.
            None => *field_sel.field.name() != TYPENAME_FIELD,
        }
    }

    fn field_needs_alias(&self, path: &mut Vec<Name>, field_sel: &FieldSelection) -> bool {
        let field = &field_sel.field;
        if *field.name() == TYPENAME_FIELD {
            return false;
        }
        let identity = FieldIdentity::of(field);
        let claimed = self
            .claims
            .get(path.as_slice())
            .and_then(|by_key| by_key.get(field.response_name()))
            .map_or(&[][..], Vec::as_slice);
        if claimed.iter().any(|other| *other != identity) {
            return true;
        }
        if !field.arguments.is_empty() && claimed.is_empty() {
            return true;
        }
        let Some(sub) = &field_sel.selection_set else {
            return false;
        };
        path.push(field.response_name().clone());
        let nested = self.selections_need_alias(path, sub);
        path.pop();
        nested
    }

    fn selections_need_alias(&self, path: &mut Vec<Name>, selection_set: &SelectionSet) -> bool {
        selection_set
            .selections
            .values()
            .any(|selection| match selection {
                Selection::Field(field_sel) => self.field_needs_alias(path, field_sel),
                Selection::InlineFragment(frag) => {
                    self.selections_need_alias(path, &frag.selection_set)
                }
            })
    }
}

/// The response keys along `merge_at`, skipping list indices. `None` when a
/// step is not a plain key.
fn response_path(merge_at: &[FetchDataPathElement]) -> Option<Vec<Name>> {
    merge_at
        .iter()
        .filter_map(|element| match element {
            FetchDataPathElement::Key(name, _) => Some(Some(name.clone())),
            FetchDataPathElement::AnyIndex(_) => None,
            FetchDataPathElement::TypenameEquals(_) | FetchDataPathElement::Parent => Some(None),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use apollo_compiler::name;

    use super::*;
    use crate::schema::ValidFederationSchema;
    use crate::schema::position::CompositeTypeDefinitionPosition;

    const SCHEMA: &str = r#"
        type Query { t: T }
        type T { listing: Listing other: Listing }
        type Listing { sku: String cost(currency: String): Int media: Media }
        interface Media { name: String }
        type Photo implements Media { name: String size(unit: String): Int }
    "#;

    fn schema() -> ValidFederationSchema {
        let schema = apollo_compiler::Schema::parse_and_validate(SCHEMA, "schema.graphql")
            .expect("valid schema");
        ValidFederationSchema::new(schema).expect("valid federation schema")
    }

    fn keys_for(operation: &str) -> OperationResponseKeys {
        let op = crate::operation::Operation::parse(schema(), operation, "op.graphql")
            .expect("valid operation");
        OperationResponseKeys::new(&op.selection_set)
    }

    fn condition(text: &str) -> Arc<FieldSelection> {
        let schema = schema();
        let t: CompositeTypeDefinitionPosition = schema
            .get_type(&name!("T"))
            .expect("T exists")
            .try_into()
            .expect("T is composite");
        let set = SelectionSet::parse(schema, t, text).expect("condition parses");
        match set.selections.values().next().expect("one selection") {
            Selection::Field(field) => field.clone(),
            Selection::InlineFragment(_) => panic!("expected a field"),
        }
    }

    fn at_t() -> Vec<FetchDataPathElement> {
        vec![FetchDataPathElement::Key(name!("t"), None)]
    }

    #[test]
    fn plain_field_shares_an_unclaimed_key() {
        let keys = keys_for("{ t { listing { sku } } }");
        assert!(!keys.condition_needs_alias(&at_t(), &condition("listing { sku }")));
        assert!(!keys.condition_needs_alias(&at_t(), &condition("other { sku }")));
    }

    #[test]
    fn client_alias_on_the_key_forces_an_alias() {
        let keys = keys_for("{ t { listing: other { sku } } }");
        assert!(keys.condition_needs_alias(&at_t(), &condition("listing { sku }")));
    }

    #[test]
    fn arguments_need_an_exact_client_match() {
        let keys = keys_for(r#"{ t { listing { cost(currency: "USD") } } }"#);
        assert!(
            !keys
                .condition_needs_alias(&at_t(), &condition(r#"listing { cost(currency: "USD") }"#))
        );
        assert!(
            keys.condition_needs_alias(&at_t(), &condition(r#"listing { cost(currency: "EUR") }"#))
        );
        let unclaimed = keys_for("{ t { other { sku } } }");
        assert!(
            unclaimed
                .condition_needs_alias(&at_t(), &condition(r#"listing { cost(currency: "USD") }"#))
        );
    }

    #[test]
    fn unknown_path_step_forces_an_alias() {
        let keys = keys_for("{ t { listing { sku } } }");
        let merge_at = vec![
            FetchDataPathElement::Key(name!("t"), None),
            FetchDataPathElement::Parent,
        ];
        assert!(keys.condition_needs_alias(&merge_at, &condition("listing { sku }")));
    }

    #[test]
    fn alias_identity_ignores_argument_free_fields() {
        let sku = AliasIdentity::of(&condition("listing { sku }"));
        let usd = AliasIdentity::of(&condition(r#"listing { sku cost(currency: "USD") }"#));
        let eur = AliasIdentity::of(&condition(r#"listing { cost(currency: "EUR") }"#));
        assert_eq!(sku, AliasIdentity::of(&condition("listing { __typename }")));
        assert_ne!(sku, usd);
        assert_ne!(usd, eur);
        assert_eq!(
            usd,
            AliasIdentity::of(&condition(r#"listing { cost(currency: "USD") }"#))
        );
    }

    /// A list index adds no response key: the lookup still finds the client's
    /// selections, and still shares an unclaimed key.
    #[test]
    fn list_indices_are_skipped() {
        let merge_at = vec![
            FetchDataPathElement::Key(name!("t"), None),
            FetchDataPathElement::AnyIndex(None),
        ];
        let plain = keys_for("{ t { listing { sku } } }");
        assert!(!plain.condition_needs_alias(&merge_at, &condition("listing { sku }")));
        let aliased = keys_for("{ t { listing: other { sku } } }");
        assert!(aliased.condition_needs_alias(&merge_at, &condition("listing { sku }")));
    }

    #[test]
    fn typename_never_needs_an_alias() {
        let keys = keys_for("{ t { listing: other { sku } } }");
        assert!(!keys.condition_needs_alias(&at_t(), &condition("__typename")));
    }

    /// A nested argument under a fragment counts the same as one outside it,
    /// both for the alias decision and for the alias identity.
    #[test]
    fn nested_arguments_are_found_through_fragments() {
        let keys = keys_for(r#"{ t { listing { media { ... on Photo { size(unit: "px") } } } } }"#);
        assert!(!keys.condition_needs_alias(
            &at_t(),
            &condition(r#"listing { media { ... on Photo { size(unit: "px") } } }"#)
        ));
        assert!(keys.condition_needs_alias(
            &at_t(),
            &condition(r#"listing { media { ... on Photo { size(unit: "em") } } }"#)
        ));
        let pixels = AliasIdentity::of(&condition(
            r#"listing { media { ... on Photo { size(unit: "px") } } }"#,
        ));
        assert_eq!(
            pixels,
            AliasIdentity::of(&condition(
                r#"listing { media { name ... on Photo { name size(unit: "px") } } }"#
            )),
        );
        assert_ne!(
            pixels,
            AliasIdentity::of(&condition(
                r#"listing { media { ... on Photo { size(unit: "em") } } }"#
            )),
        );
    }
}
