use std::fmt;
use std::fmt::Display;
use std::fmt::Formatter;

use apollo_compiler::ast::Directive;
use apollo_compiler::collections::HashMap;
use apollo_compiler::collections::IndexMap;
use apollo_compiler::executable::FieldSet;
use apollo_compiler::validation::Valid;
use apollo_compiler::Name;
use apollo_compiler::Node;
use apollo_compiler::Schema;
use itertools::Itertools;

use super::compare_keys::field_set_is_subset;
use super::VariableReference;
use super::FEDERATION_FIELDS_ARGUMENT_NAME;
use crate::sources::connect::validation::Code;
use crate::sources::connect::validation::Message;
use crate::sources::connect::EntityResolver;
use crate::sources::connect::Namespace;

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
    pub(crate) fn add_key(
        &mut self,
        field_set: &FieldSet,
        directive: &'schema Node<Directive>,
        type_name: &'schema Name,
    ) {
        self.resolvable_keys
            .push((field_set.clone(), directive, type_name));
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

        for (key, directive, type_name) in &self.resolvable_keys {
            let for_type = self.entity_connectors.get(&key.selection_set.ty);
            let key_exists = for_type
                .map(|connectors| {
                    connectors
                        .iter()
                        .any(|connector| field_set_is_subset(key, connector))
                })
                .unwrap_or(false);
            if !key_exists {
                messages.push(Message {
                    code: Code::MissingEntityConnector,
                    message: format!(
                        "Entity resolution for `@key(fields: \"{}\")` on `{}` is not implemented by a connector. See https://go.apollo.dev/connectors/directives/#rules-for-entity-true",
                        directive.argument_by_name(&FEDERATION_FIELDS_ARGUMENT_NAME, schema).ok().and_then(|arg| arg.as_str()).unwrap_or_default(),
                        type_name,
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

impl std::fmt::Debug for EntityKeyChecker<'_> {
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

/// Given the variables relevant to entity fetching, synthesize a FieldSet
/// appropriate for use in a @key directive.
pub(crate) fn make_key_field_set_from_variables(
    schema: &Schema,
    object_type_name: &Name,
    variables: &[VariableReference<Namespace>],
    resolver: EntityResolver,
) -> Result<Option<Valid<FieldSet>>, Message> {
    let params = variables
        .iter()
        .filter(|var| match resolver {
            EntityResolver::Explicit => var.namespace.namespace == Namespace::Args,
            EntityResolver::Implicit => var.namespace.namespace == Namespace::This,
        })
        .unique()
        .collect_vec();

    if params.is_empty() {
        return Ok(None);
    }

    let mut merged = TrieNode::default();
    for param in params {
        merged.insert(&param.path.iter().map(|p| p.as_str()).collect::<Vec<_>>());
    }

    FieldSet::parse_and_validate(
        Valid::assume_valid_ref(schema),
        object_type_name.clone(),
        merged.to_string(),
        "",
    )
    // This shouldn't happen because we've already validated the inputs using ArgumentVisitor
    .map_err(|_| Message {
        code: Code::GraphQLError,
        message: format!("Variables used in connector (`{}`) for `{}` cannot be used to create a valid `@key` directive.", variables.iter().join("`, `"), object_type_name),
        locations: vec![],
    }).map(Some)
}

#[derive(Default)]
struct TrieNode(IndexMap<String, TrieNode>);

impl TrieNode {
    fn insert(&mut self, path: &[&str]) {
        let mut node = self;
        for head in path {
            node = node.0.entry(head.to_string()).or_default();
        }
    }
}

impl Display for TrieNode {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        for (i, (key, node)) in self.0.iter().enumerate() {
            write!(f, "{}", key)?;
            if !node.0.is_empty() {
                write!(f, " {{ {} }}", node)?;
            }
            if i != self.0.len() - 1 {
                write!(f, " ")?;
            }
        }
        Ok(())
    }
}

#[test]
fn test_trie() {
    let mut trie = TrieNode::default();
    trie.insert(&["a", "b", "c"]);
    trie.insert(&["a", "b", "d"]);
    trie.insert(&["a", "b", "e"]);
    trie.insert(&["a", "c"]);
    trie.insert(&["a", "d"]);
    trie.insert(&["b"]);
    assert_eq!(trie.to_string(), "a { b { c d e } c d } b");
}

#[cfg(test)]
mod tests {
    use apollo_compiler::name;
    use apollo_compiler::Schema;

    use super::make_key_field_set_from_variables;
    use super::VariableReference;

    #[test]
    fn test_make_key_field_set_from_variables() {
        let result = make_key_field_set_from_variables(
            &Schema::parse_and_validate("type Query { t: T } type T { a: A b: ID } type A { b: B c: ID d: ID } type B { c: ID d: ID e: ID }", "").unwrap(),
            &name!("T"),
            &vec![
                VariableReference::parse("$args.a.b.c", 0).unwrap(),
                VariableReference::parse("$args.a.b.d", 0).unwrap(),
                VariableReference::parse("$args.a.b.e", 0).unwrap(),
                VariableReference::parse("$args.a.c", 0).unwrap(),
                VariableReference::parse("$args.a.d", 0).unwrap(),
                VariableReference::parse("$args.b", 0).unwrap()
            ],
            super::EntityResolver::Explicit,
        )
        .unwrap()
        .unwrap();

        assert_eq!(
            result.serialize().no_indent().to_string(),
            "a { b { c d e } c d } b"
        );
    }
}
