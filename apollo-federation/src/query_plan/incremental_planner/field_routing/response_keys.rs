//! Response keys the client operation claims, for deciding whether a
//! @requires condition field can share a key with other selections or must
//! be aliased.
//!
//! Condition fields never carry aliases, so two argument-less condition
//! fields with the same name are the same field and always merge. A key can
//! only be contested by the client (a different field or arguments under
//! that name) or by another condition that passes different arguments. Both
//! are indexed up front: the client's selections, and the @requires
//! conditions of every field it selects, which sit beside that field.

use apollo_compiler::Name;
use hashbrown::HashMap;

use crate::link::join_spec_definition::JoinSpecDefinition;
use crate::operation::ArgumentList;
use crate::operation::Field;
use crate::operation::FieldSelection;
use crate::operation::Selection;
use crate::operation::SelectionSet;
use crate::operation::TYPENAME_FIELD;
use crate::query_plan::FetchDataPathElement;
use crate::schema::ValidFederationSchema;

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

/// Field identities selected by the client operation or by a @requires
/// condition it can trigger, by response path and response name. Inline
/// fragments are transparent, so selections on different types under one
/// key are pooled, which can only add aliases.
#[derive(Debug, Default)]
pub(crate) struct OperationResponseKeys {
    claims: HashMap<Vec<Name>, HashMap<Name, Vec<FieldIdentity>>>,
}

impl OperationResponseKeys {
    pub(crate) fn new(selection_set: &SelectionSet, supergraph: &ValidFederationSchema) -> Self {
        let mut keys = Self::default();
        let mut requires = RequiresLookup::new(supergraph);
        keys.collect(
            selection_set,
            &mut Vec::new(),
            &mut requires,
            &mut Vec::new(),
        );
        keys
    }

    /// `expanding` holds the fields whose conditions are being walked, so a
    /// condition that leads back to its own field is not walked again.
    fn collect(
        &mut self,
        selection_set: &SelectionSet,
        path: &mut Vec<Name>,
        requires: &mut RequiresLookup,
        expanding: &mut Vec<(Name, Name)>,
    ) {
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
                    for (site, conditions) in requires.conditions_of(field) {
                        if expanding.contains(&site) {
                            continue;
                        }
                        expanding.push(site);
                        self.collect(&conditions, path, requires, expanding);
                        expanding.pop();
                    }
                    if let Some(sub) = &field_sel.selection_set {
                        path.push(field.response_name().clone());
                        self.collect(sub, path, requires, expanding);
                        path.pop();
                    }
                }
                Selection::InlineFragment(frag) => {
                    self.collect(&frag.selection_set, path, requires, expanding)
                }
            }
        }
    }

    /// Whether a condition field selected at `merge_at` must be aliased:
    /// it or a field below it could share a response key with a different
    /// field. A field with arguments nothing claims is aliased too, since a
    /// condition the index does not reach could pass other arguments.
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

/// The supergraph's @requires conditions by object field, parsed once each.
struct RequiresLookup<'a> {
    supergraph: &'a ValidFederationSchema,
    join: Option<(&'static JoinSpecDefinition, Name)>,
    parsed: HashMap<(Name, Name), Vec<SelectionSet>>,
}

impl<'a> RequiresLookup<'a> {
    fn new(supergraph: &'a ValidFederationSchema) -> Self {
        // A schema without the join spec has no @requires to find.
        let join = crate::validate_supergraph_for_query_planning(supergraph)
            .ok()
            .and_then(|(_, join, _)| {
                let directive = join.field_directive_definition(supergraph).ok()?;
                Some((join, directive.name.clone()))
            });
        Self {
            supergraph,
            join,
            parsed: HashMap::new(),
        }
    }

    /// Each @requires on `field`, for every object type it can be selected
    /// on and the interfaces those implement, with the field it belongs to.
    fn conditions_of(&mut self, field: &Field) -> Vec<((Name, Name), SelectionSet)> {
        let Some((join, directive)) = &self.join else {
            return Vec::new();
        };
        let Ok(objects) = self
            .supergraph
            .possible_runtime_types(field.field_position.parent())
        else {
            return Vec::new();
        };
        // An @interfaceObject's @requires sits on the interface field, so the
        // interfaces the runtime types implement are sites too.
        let mut types: Vec<Name> = Vec::new();
        for object in objects {
            if let Some(def) = self.supergraph.schema().get_object(&object.type_name) {
                for itf in def.implements_interfaces.iter() {
                    if !types.contains(itf) {
                        types.push((**itf).clone());
                    }
                }
            }
            types.push(object.type_name);
        }
        let mut found = Vec::new();
        for type_name in types {
            let site = (type_name, field.name().clone());
            let conditions = self.parsed.entry(site.clone()).or_insert_with(|| {
                parse_requires(self.supergraph, join, directive, &site.0, field.name())
            });
            found.extend(conditions.iter().map(|c| (site.clone(), c.clone())));
        }
        found
    }
}

fn parse_requires(
    supergraph: &ValidFederationSchema,
    join: &JoinSpecDefinition,
    directive: &Name,
    type_name: &Name,
    field_name: &Name,
) -> Vec<SelectionSet> {
    let Some(definition) = supergraph.schema().type_field(type_name, field_name).ok() else {
        return Vec::new();
    };
    definition
        .directives
        .get_all(directive)
        .filter_map(|application| join.field_directive_arguments(application).ok()?.requires)
        .filter_map(|text| {
            crate::schema::field_set::parse_field_set(supergraph, type_name.clone(), text, false)
                .ok()
        })
        .collect()
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

    fn schema() -> ValidFederationSchema {
        let sdl = r#"
            type Query { t: T }
            type T { listing: Listing other: Listing }
            type Listing { sku: String cost(currency: String): Int media: Media }
            interface Media { name: String }
            type Photo implements Media { name: String size(unit: String): Int }
        "#;
        let schema = apollo_compiler::Schema::parse_and_validate(sdl, "schema.graphql")
            .expect("valid schema");
        ValidFederationSchema::new(schema).expect("valid federation schema")
    }

    fn keys_for(operation: &str) -> OperationResponseKeys {
        let op = crate::operation::Operation::parse(schema(), operation, "op.graphql")
            .expect("valid operation");
        OperationResponseKeys::new(&op.selection_set, &schema())
    }

    /// The first field of a condition parsed on `T`.
    fn condition(text: &str) -> Arc<FieldSelection> {
        let schema = schema();
        let t = schema.get_type(&name!("T")).expect("T exists");
        let set = SelectionSet::parse(schema, t.try_into().expect("T is composite"), text)
            .expect("condition parses");
        match set.selections.values().next().expect("one selection") {
            Selection::Field(field) => field.clone(),
            Selection::InlineFragment(_) => panic!("expected a field"),
        }
    }

    fn needs_alias(operation: &str, condition_text: &str) -> bool {
        let at_t = [FetchDataPathElement::Key(name!("t"), None)];
        keys_for(operation).condition_needs_alias(&at_t, &condition(condition_text))
    }

    #[test]
    fn conditions_are_aliased_only_on_collisions() {
        // An unclaimed key, or one holding the same field, is shared.
        assert!(!needs_alias("{ t { listing { sku } } }", "listing { sku }"));
        assert!(!needs_alias("{ t { listing { sku } } }", "other { sku }"));
        assert!(!needs_alias(
            "{ t { listing: other { sku } } }",
            "__typename"
        ));
        // The client puts a different field under the key.
        assert!(needs_alias(
            "{ t { listing: other { sku } } }",
            "listing { sku }"
        ));
        // Arguments are safe only on an exact client match, since another
        // condition could otherwise claim the key with other arguments.
        let usd = r#"{ t { listing { cost(currency: "USD") } } }"#;
        assert!(!needs_alias(usd, r#"listing { cost(currency: "USD") }"#));
        assert!(needs_alias(usd, r#"listing { cost(currency: "EUR") }"#));
        assert!(needs_alias(
            "{ t { other { sku } } }",
            r#"listing { cost(currency: "USD") }"#
        ));
        // Nested fields count, through fragments too.
        let px = r#"{ t { listing { media { ... on Photo { size(unit: "px") } } } } }"#;
        assert!(!needs_alias(
            px,
            r#"listing { media { ... on Photo { size(unit: "px") } } }"#
        ));
        assert!(needs_alias(
            px,
            r#"listing { media { ... on Photo { size(unit: "em") } } }"#
        ));
    }

    #[test]
    fn merge_path_steps() {
        let t = FetchDataPathElement::Key(name!("t"), None);
        let in_list = [t.clone(), FetchDataPathElement::AnyIndex(None)];
        let listing = condition("listing { sku }");
        // A list index adds no response key.
        assert!(!keys_for("{ t { listing { sku } } }").condition_needs_alias(&in_list, &listing));
        let aliased = keys_for("{ t { listing: other { sku } } }");
        assert!(aliased.condition_needs_alias(&in_list, &listing));
        // A step that is not a response key can't be looked up.
        let unknown = [t, FetchDataPathElement::Parent];
        assert!(keys_for("{ t { listing { sku } } }").condition_needs_alias(&unknown, &listing));
    }

    #[test]
    fn alias_identity_tracks_nested_arguments() {
        let id = |text: &str| AliasIdentity::of(&condition(text));
        assert_eq!(id("listing { sku }"), id("listing { __typename }"));
        assert_ne!(
            id("listing { sku }"),
            id(r#"listing { cost(currency: "USD") }"#)
        );
        assert_ne!(
            id(r#"listing { cost(currency: "USD") }"#),
            id(r#"listing { cost(currency: "EUR") }"#)
        );
        let px = r#"listing { media { ... on Photo { size(unit: "px") } } }"#;
        assert_eq!(
            id(px),
            id(r#"listing { media { name ... on Photo { name size(unit: "px") } } }"#)
        );
        assert_ne!(
            id(px),
            id(r#"listing { media { ... on Photo { size(unit: "em") } } }"#)
        );
    }
}
