use apollo_compiler::Name;
use apollo_compiler::Schema;
use apollo_compiler::executable::FieldSet;
use apollo_compiler::validation::Valid;
use apollo_compiler::validation::WithErrors;
use itertools::Itertools;

use super::VariableReference;
use crate::connectors::Namespace;
use crate::connectors::json_selection::SelectionTrie;

/// Given the variables relevant to entity fetching, synthesize a FieldSet
/// appropriate for use in a @key directive.
pub(crate) fn make_key_field_set_from_variables(
    schema: &Schema,
    object_type_name: &Name,
    variables: impl Iterator<Item = VariableReference<Namespace>>,
    namespace: Namespace,
) -> Result<Option<Valid<FieldSet>>, WithErrors<FieldSet>> {
    let params = variables
        .filter(|var| var.namespace.namespace == namespace)
        .unique()
        .collect_vec();

    if params.is_empty() {
        return Ok(None);
    }

    // let mut merged = TrieNode::default();
    let mut merged = SelectionTrie::new();
    for param in params {
        merged.extend(&param.selection);
    }

    FieldSet::parse_and_validate(
        Valid::assume_valid_ref(schema),
        object_type_name.clone(),
        merged.to_string(),
        "",
    )
    .map(Some)
}

#[cfg(test)]
mod tests {
    use apollo_compiler::Schema;
    use apollo_compiler::name;

    use super::make_key_field_set_from_variables;
    use crate::connectors::Namespace;
    use crate::connectors::PathSelection;

    #[test]
    fn test_make_args_field_set_from_variables() {
        let result = make_key_field_set_from_variables(
            &Schema::parse_and_validate("type Query { t: T } type T { a: A b: ID } type A { b: B c: ID d: ID } type B { c: ID d: ID e: ID }", "").unwrap(),
            &name!("T"),
            vec![
                PathSelection::parse("$args.a.b.c".into()).unwrap().1.variable_reference().unwrap(),
                PathSelection::parse("$args.a.b { d e }".into()).unwrap().1.variable_reference().unwrap(),
                PathSelection::parse("$args.a.c".into()).unwrap().1.variable_reference().unwrap(),
                PathSelection::parse("$args.a.d".into()).unwrap().1.variable_reference().unwrap(),
                PathSelection::parse("$args { b }".into()).unwrap().1.variable_reference().unwrap(),
            ].into_iter(),
            Namespace::Args,
        )
        .unwrap()
        .unwrap();

        assert_eq!(
            result.serialize().no_indent().to_string(),
            "a { b { c d e } c d } b"
        );
    }

    #[test]
    fn test_make_batch_field_set_from_variables() {
        let result = make_key_field_set_from_variables(
            &Schema::parse_and_validate("type Query { t: T } type T { a: A b: ID } type A { b: B c: ID d: ID } type B { c: ID d: ID e: ID }", "").unwrap(),
            &name!("T"),
            vec![
                PathSelection::parse("$batch.a.b.c".into()).unwrap().1.variable_reference().unwrap(),
                PathSelection::parse("$batch.a.b { d e }".into()).unwrap().1.variable_reference().unwrap(),
                PathSelection::parse("$batch.a.c".into()).unwrap().1.variable_reference().unwrap(),
                PathSelection::parse("$batch.a.d".into()).unwrap().1.variable_reference().unwrap(),
                PathSelection::parse("$batch { b }".into()).unwrap().1.variable_reference().unwrap(),
            ].into_iter(),
            Namespace::Batch,
        )
        .unwrap()
        .unwrap();

        assert_eq!(
            result.serialize().no_indent().to_string(),
            "a { b { c d e } c d } b"
        );
    }
}
