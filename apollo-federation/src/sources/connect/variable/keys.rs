use std::fmt;
use std::fmt::Display;
use std::fmt::Formatter;

use apollo_compiler::collections::IndexMap;
use apollo_compiler::executable::FieldSet;
use apollo_compiler::validation::Valid;
use apollo_compiler::validation::WithErrors;
use apollo_compiler::Name;
use apollo_compiler::Schema;
use itertools::Itertools;

use super::VariableReference;
use crate::sources::connect::EntityResolver;
use crate::sources::connect::Namespace;

/// Given the variables relevant to entity fetching, synthesize a FieldSet
/// appropriate for use in a @key directive.
pub(crate) fn make_key_field_set_from_variables(
    schema: &Schema,
    object_type_name: &Name,
    variables: &[VariableReference<Namespace>],
    resolver: EntityResolver,
) -> Result<Option<Valid<FieldSet>>, WithErrors<FieldSet>> {
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
    .map(Some)
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

#[cfg(test)]
mod tests {
    use apollo_compiler::name;
    use apollo_compiler::Schema;

    use super::make_key_field_set_from_variables;
    use super::TrieNode;
    use super::VariableReference;

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
