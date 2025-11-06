//! Validations to make sure that all `@key` directives in the schema correspond to at least
//! one connector.

use std::fmt;
use std::fmt::Formatter;

use apollo_compiler::Name;
use apollo_compiler::Node;
use apollo_compiler::Schema;
use apollo_compiler::ast::Directive;
use apollo_compiler::collections::HashMap;
use apollo_compiler::executable::FieldSet;
use apollo_compiler::executable::Selection;
use apollo_compiler::validation::Valid;
use itertools::Itertools;

use crate::connectors::Connector;
use crate::connectors::Namespace;
use crate::connectors::validation::Code;
use crate::connectors::validation::Message;
use crate::connectors::variable::VariableReference;
use crate::link::federation_spec_definition::FEDERATION_FIELDS_ARGUMENT_NAME;

/// Collects keys and entity connectors for comparison and validation.
#[derive(Default)]
pub(crate) struct EntityKeyChecker<'schema> {
    /// Any time we see `type T @key(fields: "f")` (with resolvable: true)
    resolvable_keys: Vec<(FieldSet, &'schema Node<Directive>, &'schema Name)>,
    /// Any time we see either:
    /// - `type Query { t(f: X): T @connect(entity: true) }` (Explicit entity resolver)
    /// - `type T { f: X g: Y @connect(... $this.f ...) }`  (Implicit entity resolver)
    entity_connectors: HashMap<Name, Vec<Valid<FieldSet>>>,
}

impl<'schema> EntityKeyChecker<'schema> {
    pub(crate) fn add_key(&mut self, field_set: &FieldSet, directive: &'schema Node<Directive>) {
        self.resolvable_keys
            .push((field_set.clone(), directive, &directive.name));
    }

    pub(crate) fn add_connector(&mut self, field_set: Valid<FieldSet>) {
        self.entity_connectors
            .entry(field_set.selection_set.ty.clone())
            .or_default()
            .push(field_set);
    }

    /// For each @key we've seen, check if there's a corresponding entity connector
    /// by semantically comparing the @key field set with the synthesized field set
    /// from the connector's arguments.
    ///
    /// The comparison is done by checking if the @key field set is a subset of the
    /// entity connector's field set. It's not equality because we convert `@external`/
    /// `@requires` fields to keys for simplicity's sake.
    pub(crate) fn check_for_missing_entity_connectors(&self, schema: &Schema) -> Vec<Message> {
        let mut messages = Vec::new();

        for (key, directive, _) in &self.resolvable_keys {
            let for_type = self.entity_connectors.get(&key.selection_set.ty);
            let key_exists = for_type.is_some_and(|connectors| {
                connectors
                    .iter()
                    .any(|connector| field_set_is_subset(key, connector))
            });
            if !key_exists {
                messages.push(Message {
                    code: Code::MissingEntityConnector,
                    message: format!(
                        "Entity resolution for `@key(fields: \"{}\")` on `{}` is not implemented by a connector. See https://go.apollo.dev/connectors/entity-rules",
                        directive.argument_by_name(&FEDERATION_FIELDS_ARGUMENT_NAME, schema).ok().and_then(|arg| arg.as_str()).unwrap_or_default(),
                        key.selection_set.ty,
                    ),
                    locations: directive
                        .line_column_range(&schema.sources)
                        .into_iter()
                        .collect(),
                });
            }
        }

        messages
    }
}

impl fmt::Debug for EntityKeyChecker<'_> {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.debug_struct("EntityKeyChecker")
            .field(
                "resolvable_keys",
                &self
                    .resolvable_keys
                    .iter()
                    .map(|(fs, _, _)| {
                        format!(
                            "... on {} {}",
                            fs.selection_set.ty,
                            fs.selection_set.serialize().no_indent()
                        )
                    })
                    .collect_vec(),
            )
            .field(
                "entity_connectors",
                &self
                    .entity_connectors
                    .values()
                    .flatten()
                    .map(|fs| {
                        format!(
                            "... on {} {}",
                            fs.selection_set.ty,
                            fs.selection_set.serialize().no_indent()
                        )
                    })
                    .collect_vec(),
            )
            .finish()
    }
}

pub(crate) fn field_set_error(
    variables: &[VariableReference<Namespace>],
    connector: &Connector,
    schema: &Schema,
) -> Message {
    Message {
        code: Code::ConnectorsCannotResolveKey,
        message: format!(
            "Variables used in connector (`{}`) on type `{}` cannot be used to create a valid `@key` directive.",
            variables.iter().join("`, `"),
            connector.id.directive.simple_name()
        ),
        locations: connector
            .name()
            .line_column_range(&schema.sources)
            .into_iter()
            .collect(),
    }
}

fn selection_is_subset(x: &Selection, y: &Selection) -> bool {
    match (x, y) {
        (Selection::Field(x), Selection::Field(y)) => {
            x.name == y.name
                && x.alias == y.alias
                && vec_includes_as_set(
                    &x.selection_set.selections,
                    &y.selection_set.selections,
                    selection_is_subset,
                )
        }
        (Selection::InlineFragment(x), Selection::InlineFragment(y)) => {
            x.type_condition == y.type_condition
                && vec_includes_as_set(
                    &x.selection_set.selections,
                    &y.selection_set.selections,
                    selection_is_subset,
                )
        }
        _ => false,
    }
}

/// Returns true if `inner` is a subset of `outer`.
///
/// Note: apollo_federation::operation::SelectionSet has its own `contains`
/// method I'd love to use, but it requires a ValidFederationSchema, which
/// we don't have during validation. This code can be removed after we rewrite
/// composition in rust and connector validations happen after schema validation
/// and `@link` enrichment.
pub(crate) fn field_set_is_subset(inner: &FieldSet, outer: &FieldSet) -> bool {
    inner.selection_set.ty == outer.selection_set.ty
        && vec_includes_as_set(
            &outer.selection_set.selections,
            &inner.selection_set.selections,
            selection_is_subset,
        )
}

// `this` vector includes `other` vector as a set
fn vec_includes_as_set<T>(this: &[T], other: &[T], item_matches: impl Fn(&T, &T) -> bool) -> bool {
    other.iter().all(|other_node| {
        this.iter()
            .any(|this_node| item_matches(this_node, other_node))
    })
}

#[cfg(test)]
mod tests {
    use apollo_compiler::Schema;
    use apollo_compiler::executable::FieldSet;
    use apollo_compiler::name;
    use apollo_compiler::validation::Valid;
    use rstest::rstest;

    use super::field_set_is_subset;

    fn schema() -> Valid<Schema> {
        Schema::parse_and_validate(
            r#"
        type Query {
            t: T
        }

        type T {
            a: String
            b: B
            c: String
        }

        type B {
            x: String
            y: String
        }
        "#,
            "",
        )
        .unwrap()
    }

    #[rstest]
    #[case("a", "a")]
    #[case("a b { x } c", "a b { x } c")]
    #[case("a", "a c")]
    #[case("b { x }", "b { x y }")]
    fn test_field_set_is_subset(#[case] inner: &str, #[case] outer: &str) {
        let schema = schema();
        let inner = FieldSet::parse_and_validate(&schema, name!(T), inner, "inner").unwrap();
        let outer = FieldSet::parse_and_validate(&schema, name!(T), outer, "outer").unwrap();
        assert!(field_set_is_subset(&inner, &outer));
    }

    #[rstest]
    #[case("a b { x } c", "a")]
    #[case("b { x y }", "b { x }")]
    fn test_field_set_is_not_subset(#[case] inner: &str, #[case] outer: &str) {
        let schema = schema();
        let inner = FieldSet::parse_and_validate(&schema, name!(T), inner, "inner").unwrap();
        let outer = FieldSet::parse_and_validate(&schema, name!(T), outer, "outer").unwrap();
        assert!(!field_set_is_subset(&inner, &outer));
    }
}
