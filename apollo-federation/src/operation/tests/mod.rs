use std::sync::Arc;

use apollo_compiler::collections::IndexSet;
use apollo_compiler::name;
use apollo_compiler::schema::Schema;
use apollo_compiler::ExecutableDocument;

use super::normalize_operation;
use super::Name;
use super::NamedFragments;
use super::Operation;
use super::Selection;
use super::SelectionKey;
use super::SelectionSet;
use crate::query_graph::graph_path::OpPathElement;
use crate::schema::position::InterfaceTypeDefinitionPosition;
use crate::schema::position::ObjectTypeDefinitionPosition;
use crate::schema::ValidFederationSchema;
use crate::subgraph::Subgraph;

pub(super) fn parse_schema_and_operation(
    schema_and_operation: &str,
) -> (ValidFederationSchema, ExecutableDocument) {
    let (schema, executable_document) =
        apollo_compiler::parse_mixed_validate(schema_and_operation, "document.graphql").unwrap();
    let executable_document = executable_document.into_inner();
    let schema = ValidFederationSchema::new(schema).unwrap();
    (schema, executable_document)
}

pub(super) fn parse_subgraph(name: &str, schema: &str) -> ValidFederationSchema {
    let parsed_schema =
        Subgraph::parse_and_expand(name, &format!("https://{name}"), schema).unwrap();
    ValidFederationSchema::new(parsed_schema.schema).unwrap()
}

pub(super) fn parse_schema(schema_doc: &str) -> ValidFederationSchema {
    let schema = Schema::parse_and_validate(schema_doc, "schema.graphql").unwrap();
    ValidFederationSchema::new(schema).unwrap()
}

pub(super) fn parse_operation(schema: &ValidFederationSchema, query: &str) -> Operation {
    let executable_document = apollo_compiler::ExecutableDocument::parse_and_validate(
        schema.schema(),
        query,
        "query.graphql",
    )
    .unwrap();
    let operation = executable_document.operations.get(None).unwrap();
    let named_fragments = NamedFragments::new(&executable_document.fragments, schema);
    let selection_set =
        SelectionSet::from_selection_set(&operation.selection_set, &named_fragments, schema)
            .unwrap();

    Operation {
        schema: schema.clone(),
        root_kind: operation.operation_type.into(),
        name: operation.name.clone(),
        variables: Arc::new(operation.variables.clone()),
        directives: Arc::new(operation.directives.clone()),
        selection_set,
        named_fragments,
    }
}

/// Parse and validate the query similarly to `parse_operation`, but does not construct the
/// `Operation` struct.
pub(super) fn validate_operation(schema: &ValidFederationSchema, query: &str) {
    apollo_compiler::ExecutableDocument::parse_and_validate(
        schema.schema(),
        query,
        "query.graphql",
    )
    .unwrap();
}

#[test]
fn expands_named_fragments() {
    let operation_with_named_fragment = r#"
query NamedFragmentQuery {
  foo {
    id
    ...Bar
  }
}

fragment Bar on Foo {
  bar
  baz
}

type Query {
  foo: Foo
}

type Foo {
  id: ID!
  bar: String!
  baz: Int
}
"#;
    let (schema, mut executable_document) =
        parse_schema_and_operation(operation_with_named_fragment);
    if let Some(operation) = executable_document
        .operations
        .named
        .get_mut("NamedFragmentQuery")
    {
        let mut normalized_operation = normalize_operation(
            operation,
            NamedFragments::new(&executable_document.fragments, &schema),
            &schema,
            &IndexSet::default(),
        )
        .unwrap();
        normalized_operation.named_fragments = Default::default();
        insta::assert_snapshot!(normalized_operation, @r###"
                query NamedFragmentQuery {
                  foo {
                    id
                    bar
                    baz
                  }
                }
            "###);
    }
}

#[test]
fn expands_and_deduplicates_fragments() {
    let operation_with_named_fragment = r#"
query NestedFragmentQuery {
  foo {
    ...FirstFragment
    ...SecondFragment
  }
}

fragment FirstFragment on Foo {
  id
  bar
  baz
}

fragment SecondFragment on Foo {
  id
  bar
}

type Query {
  foo: Foo
}

type Foo {
  id: ID!
  bar: String!
  baz: String
}
"#;
    let (schema, mut executable_document) =
        parse_schema_and_operation(operation_with_named_fragment);
    if let Some((_, operation)) = executable_document.operations.named.first_mut() {
        let mut normalized_operation = normalize_operation(
            operation,
            NamedFragments::new(&executable_document.fragments, &schema),
            &schema,
            &IndexSet::default(),
        )
        .unwrap();
        normalized_operation.named_fragments = Default::default();
        insta::assert_snapshot!(normalized_operation, @r###"
              query NestedFragmentQuery {
                foo {
                  id
                  bar
                  baz
                }
              }
            "###);
    }
}

#[test]
fn can_remove_introspection_selections() {
    let operation_with_introspection = r#"
query TestIntrospectionQuery {
  __schema {
    types {
      name
    }
  }
}

type Query {
  foo: String
}
"#;
    let (schema, mut executable_document) =
        parse_schema_and_operation(operation_with_introspection);
    if let Some(operation) = executable_document
        .operations
        .named
        .get_mut("TestIntrospectionQuery")
    {
        let normalized_operation = normalize_operation(
            operation,
            NamedFragments::new(&executable_document.fragments, &schema),
            &schema,
            &IndexSet::default(),
        )
        .unwrap();

        assert!(normalized_operation.selection_set.selections.is_empty());
    }
}

#[test]
fn merge_same_fields_without_directives() {
    let operation_string = r#"
query Test {
  t {
    v1
  }
  t {
    v2
 }
}

type Query {
  t: T
}

type T {
  v1: Int
  v2: String
}
"#;
    let (schema, mut executable_document) = parse_schema_and_operation(operation_string);
    if let Some((_, operation)) = executable_document.operations.named.first_mut() {
        let normalized_operation = normalize_operation(
            operation,
            NamedFragments::new(&executable_document.fragments, &schema),
            &schema,
            &IndexSet::default(),
        )
        .unwrap();
        let expected = r#"query Test {
  t {
    v1
    v2
  }
}"#;
        let actual = normalized_operation.to_string();
        assert_eq!(expected, actual);
    } else {
        panic!("unable to parse document")
    }
}

#[test]
fn merge_same_fields_with_same_directive() {
    let operation_with_directives = r#"
query Test($skipIf: Boolean!) {
  t @skip(if: $skipIf) {
    v1
  }
  t @skip(if: $skipIf) {
    v2
  }
}

type Query {
  t: T
}

type T {
  v1: Int
  v2: String
}
"#;
    let (schema, mut executable_document) = parse_schema_and_operation(operation_with_directives);
    if let Some((_, operation)) = executable_document.operations.named.first_mut() {
        let normalized_operation = normalize_operation(
            operation,
            NamedFragments::new(&executable_document.fragments, &schema),
            &schema,
            &IndexSet::default(),
        )
        .unwrap();
        let expected = r#"query Test($skipIf: Boolean!) {
  t @skip(if: $skipIf) {
    v1
    v2
  }
}"#;
        let actual = normalized_operation.to_string();
        assert_eq!(expected, actual);
    } else {
        panic!("unable to parse document")
    }
}

#[test]
fn merge_same_fields_with_same_directive_but_different_arg_order() {
    let operation_with_directives_different_arg_order = r#"
query Test($skipIf: Boolean!) {
  t @customSkip(if: $skipIf, label: "foo") {
    v1
  }
  t @customSkip(label: "foo", if: $skipIf) {
    v2
  }
}

directive @customSkip(if: Boolean!, label: String!) on FIELD | INLINE_FRAGMENT

type Query {
  t: T
}

type T {
  v1: Int
  v2: String
}
"#;
    let (schema, mut executable_document) =
        parse_schema_and_operation(operation_with_directives_different_arg_order);
    if let Some((_, operation)) = executable_document.operations.named.first_mut() {
        let normalized_operation = normalize_operation(
            operation,
            NamedFragments::new(&executable_document.fragments, &schema),
            &schema,
            &IndexSet::default(),
        )
        .unwrap();
        let expected = r#"query Test($skipIf: Boolean!) {
  t @customSkip(if: $skipIf, label: "foo") {
    v1
    v2
  }
}"#;
        let actual = normalized_operation.to_string();
        assert_eq!(expected, actual);
    } else {
        panic!("unable to parse document")
    }
}

#[test]
fn do_not_merge_when_only_one_field_specifies_directive() {
    let operation_one_field_with_directives = r#"
query Test($skipIf: Boolean!) {
  t {
    v1
  }
  t @skip(if: $skipIf) {
    v2
  }
}

type Query {
  t: T
}

type T {
  v1: Int
  v2: String
}
"#;
    let (schema, mut executable_document) =
        parse_schema_and_operation(operation_one_field_with_directives);
    if let Some((_, operation)) = executable_document.operations.named.first_mut() {
        let normalized_operation = normalize_operation(
            operation,
            NamedFragments::new(&executable_document.fragments, &schema),
            &schema,
            &IndexSet::default(),
        )
        .unwrap();
        let expected = r#"query Test($skipIf: Boolean!) {
  t {
    v1
  }
  t @skip(if: $skipIf) {
    v2
  }
}"#;
        let actual = normalized_operation.to_string();
        assert_eq!(expected, actual);
    } else {
        panic!("unable to parse document")
    }
}

#[test]
fn do_not_merge_when_fields_have_different_directives() {
    let operation_different_directives = r#"
query Test($skip1: Boolean!, $skip2: Boolean!) {
  t @skip(if: $skip1) {
    v1
  }
  t @skip(if: $skip2) {
    v2
  }
}

type Query {
  t: T
}

type T {
  v1: Int
  v2: String
}
"#;
    let (schema, mut executable_document) =
        parse_schema_and_operation(operation_different_directives);
    if let Some((_, operation)) = executable_document.operations.named.first_mut() {
        let normalized_operation = normalize_operation(
            operation,
            NamedFragments::new(&executable_document.fragments, &schema),
            &schema,
            &IndexSet::default(),
        )
        .unwrap();
        let expected = r#"query Test($skip1: Boolean!, $skip2: Boolean!) {
  t @skip(if: $skip1) {
    v1
  }
  t @skip(if: $skip2) {
    v2
  }
}"#;
        let actual = normalized_operation.to_string();
        assert_eq!(expected, actual);
    } else {
        panic!("unable to parse document")
    }
}

#[test]
fn do_not_merge_fields_with_defer_directive() {
    let operation_defer_fields = r#"
query Test {
  t {
    ... @defer {
      v1
    }
  }
  t {
    ... @defer {
      v2
    }
  }
}

directive @defer(label: String, if: Boolean! = true) on FRAGMENT_SPREAD | INLINE_FRAGMENT

type Query {
  t: T
}

type T {
  v1: Int
  v2: String
}
"#;
    let (schema, mut executable_document) = parse_schema_and_operation(operation_defer_fields);
    if let Some((_, operation)) = executable_document.operations.named.first_mut() {
        let normalized_operation = normalize_operation(
            operation,
            NamedFragments::new(&executable_document.fragments, &schema),
            &schema,
            &IndexSet::default(),
        )
        .unwrap();
        let expected = r#"query Test {
  t {
    ... @defer {
      v1
    }
    ... @defer {
      v2
    }
  }
}"#;
        let actual = normalized_operation.to_string();
        assert_eq!(expected, actual);
    } else {
        panic!("unable to parse document")
    }
}

#[test]
fn merge_nested_field_selections() {
    let nested_operation = r#"
query Test {
  t {
    t1
    ... @defer {
      v {
        v1
      }
    }
  }
  t {
    t1
    t2
    ... @defer {
      v {
        v2
      }
    }
  }
}

directive @defer(label: String, if: Boolean! = true) on FRAGMENT_SPREAD | INLINE_FRAGMENT

type Query {
  t: T
}

type T {
  t1: Int
  t2: String
  v: V
}

type V {
  v1: Int
  v2: String
}
"#;
    let (schema, mut executable_document) = parse_schema_and_operation(nested_operation);
    if let Some((_, operation)) = executable_document.operations.named.first_mut() {
        let normalized_operation = normalize_operation(
            operation,
            NamedFragments::new(&executable_document.fragments, &schema),
            &schema,
            &IndexSet::default(),
        )
        .unwrap();
        let expected = r#"query Test {
  t {
    t1
    ... @defer {
      v {
        v1
      }
    }
    t2
    ... @defer {
      v {
        v2
      }
    }
  }
}"#;
        let actual = normalized_operation.to_string();
        assert_eq!(expected, actual);
    } else {
        panic!("unable to parse document")
    }
}

//
// inline fragments
//

#[test]
fn merge_same_fragment_without_directives() {
    let operation_with_fragments = r#"
query Test {
  t {
    ... on T {
      v1
    }
    ... on T {
      v2
    }
  }
}

type Query {
  t: T
}

type T {
  v1: Int
  v2: String
}
"#;
    let (schema, mut executable_document) = parse_schema_and_operation(operation_with_fragments);
    if let Some((_, operation)) = executable_document.operations.named.first_mut() {
        let normalized_operation = normalize_operation(
            operation,
            NamedFragments::new(&executable_document.fragments, &schema),
            &schema,
            &IndexSet::default(),
        )
        .unwrap();
        let expected = r#"query Test {
  t {
    v1
    v2
  }
}"#;
        let actual = normalized_operation.to_string();
        assert_eq!(expected, actual);
    } else {
        panic!("unable to parse document")
    }
}

#[test]
fn merge_same_fragments_with_same_directives() {
    let operation_fragments_with_directives = r#"
query Test($skipIf: Boolean!) {
  t {
    ... on T @skip(if: $skipIf) {
      v1
    }
    ... on T @skip(if: $skipIf) {
      v2
    }
  }
}

type Query {
  t: T
}

type T {
  v1: Int
  v2: String
}
"#;
    let (schema, mut executable_document) =
        parse_schema_and_operation(operation_fragments_with_directives);
    if let Some((_, operation)) = executable_document.operations.named.first_mut() {
        let normalized_operation = normalize_operation(
            operation,
            NamedFragments::new(&executable_document.fragments, &schema),
            &schema,
            &IndexSet::default(),
        )
        .unwrap();
        let expected = r#"query Test($skipIf: Boolean!) {
  t {
    ... on T @skip(if: $skipIf) {
      v1
      v2
    }
  }
}"#;
        let actual = normalized_operation.to_string();
        assert_eq!(expected, actual);
    } else {
        panic!("unable to parse document")
    }
}

#[test]
fn merge_same_fragments_with_same_directive_but_different_arg_order() {
    let operation_fragments_with_directives_args_order = r#"
query Test($skipIf: Boolean!) {
  t {
    ... on T @customSkip(if: $skipIf, label: "foo") {
      v1
    }
    ... on T @customSkip(label: "foo", if: $skipIf) {
      v2
    }
  }
}

directive @customSkip(if: Boolean!, label: String!) on FIELD | INLINE_FRAGMENT

type Query {
  t: T
}

type T {
  v1: Int
  v2: String
}
"#;
    let (schema, mut executable_document) =
        parse_schema_and_operation(operation_fragments_with_directives_args_order);
    if let Some((_, operation)) = executable_document.operations.named.first_mut() {
        let normalized_operation = normalize_operation(
            operation,
            NamedFragments::new(&executable_document.fragments, &schema),
            &schema,
            &IndexSet::default(),
        )
        .unwrap();
        let expected = r#"query Test($skipIf: Boolean!) {
  t {
    ... on T @customSkip(if: $skipIf, label: "foo") {
      v1
      v2
    }
  }
}"#;
        let actual = normalized_operation.to_string();
        assert_eq!(expected, actual);
    } else {
        panic!("unable to parse document")
    }
}

#[test]
fn do_not_merge_when_only_one_fragment_specifies_directive() {
    let operation_one_fragment_with_directive = r#"
query Test($skipIf: Boolean!) {
  t {
    ... on T {
      v1
    }
    ... on T @skip(if: $skipIf) {
      v2
    }
  }
}

type Query {
  t: T
}

type T {
  v1: Int
  v2: String
}
"#;
    let (schema, mut executable_document) =
        parse_schema_and_operation(operation_one_fragment_with_directive);
    if let Some((_, operation)) = executable_document.operations.named.first_mut() {
        let normalized_operation = normalize_operation(
            operation,
            NamedFragments::new(&executable_document.fragments, &schema),
            &schema,
            &IndexSet::default(),
        )
        .unwrap();
        let expected = r#"query Test($skipIf: Boolean!) {
  t {
    v1
    ... on T @skip(if: $skipIf) {
      v2
    }
  }
}"#;
        let actual = normalized_operation.to_string();
        assert_eq!(expected, actual);
    } else {
        panic!("unable to parse document")
    }
}

#[test]
fn do_not_merge_when_fragments_have_different_directives() {
    let operation_fragments_with_different_directive = r#"
query Test($skip1: Boolean!, $skip2: Boolean!) {
  t {
    ... on T @skip(if: $skip1) {
      v1
    }
    ... on T @skip(if: $skip2) {
      v2
    }
  }
}

type Query {
  t: T
}

type T {
  v1: Int
  v2: String
}
"#;
    let (schema, mut executable_document) =
        parse_schema_and_operation(operation_fragments_with_different_directive);
    if let Some((_, operation)) = executable_document.operations.named.first_mut() {
        let normalized_operation = normalize_operation(
            operation,
            NamedFragments::new(&executable_document.fragments, &schema),
            &schema,
            &IndexSet::default(),
        )
        .unwrap();
        let expected = r#"query Test($skip1: Boolean!, $skip2: Boolean!) {
  t {
    ... on T @skip(if: $skip1) {
      v1
    }
    ... on T @skip(if: $skip2) {
      v2
    }
  }
}"#;
        let actual = normalized_operation.to_string();
        assert_eq!(expected, actual);
    } else {
        panic!("unable to parse document")
    }
}

#[test]
fn do_not_merge_fragments_with_defer_directive() {
    let operation_fragments_with_defer = r#"
query Test {
  t {
    ... on T @defer {
      v1
    }
    ... on T @defer {
      v2
    }
  }
}

directive @defer(label: String, if: Boolean! = true) on FRAGMENT_SPREAD | INLINE_FRAGMENT

type Query {
  t: T
}

type T {
  v1: Int
  v2: String
}
"#;
    let (schema, mut executable_document) =
        parse_schema_and_operation(operation_fragments_with_defer);
    if let Some((_, operation)) = executable_document.operations.named.first_mut() {
        let normalized_operation = normalize_operation(
            operation,
            NamedFragments::new(&executable_document.fragments, &schema),
            &schema,
            &IndexSet::default(),
        )
        .unwrap();
        let expected = r#"query Test {
  t {
    ... on T @defer {
      v1
    }
    ... on T @defer {
      v2
    }
  }
}"#;
        let actual = normalized_operation.to_string();
        assert_eq!(expected, actual);
    } else {
        panic!("unable to parse document")
    }
}

#[test]
fn merge_nested_fragments() {
    let operation_nested_fragments = r#"
query Test {
  t {
    ... on T {
      t1
    }
    ... on T {
      v {
        v1
      }
    }
  }
  t {
    ... on T {
      t1
      t2
    }
    ... on T {
      v {
        v2
      }
    }
  }
}

type Query {
  t: T
}

type T {
  t1: Int
  t2: String
  v: V
}

type V {
  v1: Int
  v2: String
}
"#;
    let (schema, mut executable_document) = parse_schema_and_operation(operation_nested_fragments);
    if let Some((_, operation)) = executable_document.operations.named.first_mut() {
        let normalized_operation = normalize_operation(
            operation,
            NamedFragments::new(&executable_document.fragments, &schema),
            &schema,
            &IndexSet::default(),
        )
        .unwrap();
        let expected = r#"query Test {
  t {
    t1
    v {
      v1
      v2
    }
    t2
  }
}"#;
        let actual = normalized_operation.to_string();
        assert_eq!(expected, actual);
    } else {
        panic!("unable to parse document")
    }
}

#[test]
fn removes_sibling_typename() {
    let operation_with_typename = r#"
query TestQuery {
  foo {
    __typename
    v1
    v2
  }
}

type Query {
  foo: Foo
}

type Foo {
  v1: ID!
  v2: String
}
"#;
    let (schema, mut executable_document) = parse_schema_and_operation(operation_with_typename);
    if let Some(operation) = executable_document.operations.named.get_mut("TestQuery") {
        let normalized_operation = normalize_operation(
            operation,
            NamedFragments::new(&executable_document.fragments, &schema),
            &schema,
            &IndexSet::default(),
        )
        .unwrap();
        let expected = r#"query TestQuery {
  foo {
    v1
    v2
  }
}"#;
        let actual = normalized_operation.to_string();
        assert_eq!(expected, actual);
    }
}

#[test]
fn keeps_typename_if_no_other_selection() {
    let operation_with_single_typename = r#"
query TestQuery {
  foo {
    __typename
  }
}

type Query {
  foo: Foo
}

type Foo {
  v1: ID!
  v2: String
}
"#;
    let (schema, mut executable_document) =
        parse_schema_and_operation(operation_with_single_typename);
    if let Some(operation) = executable_document.operations.named.get_mut("TestQuery") {
        let normalized_operation = normalize_operation(
            operation,
            NamedFragments::new(&executable_document.fragments, &schema),
            &schema,
            &IndexSet::default(),
        )
        .unwrap();
        let expected = r#"query TestQuery {
  foo {
    __typename
  }
}"#;
        let actual = normalized_operation.to_string();
        assert_eq!(expected, actual);
    }
}

#[test]
fn keeps_typename_for_interface_object() {
    let operation_with_intf_object_typename = r#"
query TestQuery {
  foo {
    __typename
    v1
    v2
  }
}

directive @interfaceObject on OBJECT
directive @key(fields: FieldSet!, resolvable: Boolean = true) repeatable on OBJECT | INTERFACE

type Query {
  foo: Foo
}

type Foo @interfaceObject @key(fields: "id") {
  v1: ID!
  v2: String
}

scalar FieldSet
"#;
    let (schema, mut executable_document) =
        parse_schema_and_operation(operation_with_intf_object_typename);
    if let Some(operation) = executable_document.operations.named.get_mut("TestQuery") {
        let mut interface_objects: IndexSet<InterfaceTypeDefinitionPosition> = IndexSet::default();
        interface_objects.insert(InterfaceTypeDefinitionPosition {
            type_name: name!("Foo"),
        });

        let normalized_operation = normalize_operation(
            operation,
            NamedFragments::new(&executable_document.fragments, &schema),
            &schema,
            &interface_objects,
        )
        .unwrap();
        let expected = r#"query TestQuery {
  foo {
    __typename
    v1
    v2
  }
}"#;
        let actual = normalized_operation.to_string();
        assert_eq!(expected, actual);
    }
}

/// This regression-tests an assumption from
/// https://github.com/apollographql/federation-next/pull/290#discussion_r1587200664
#[test]
fn converting_operation_types() {
    let schema = apollo_compiler::Schema::parse_and_validate(
        r#"
        interface Intf {
            intfField: Int
        }
        type HasA implements Intf {
            a: Boolean
            intfField: Int
        }
        type Nested {
            a: Int
            b: Int
            c: Int
        }
        type Query {
            a: Int
            b: Int
            c: Int
            object: Nested
            intf: Intf
        }
        "#,
        "schema.graphql",
    )
    .unwrap();
    let schema = ValidFederationSchema::new(schema).unwrap();
    insta::assert_snapshot!(Operation::parse(
            schema.clone(),
            r#"
        {
            intf {
                ... on HasA { a }
                ... frag
            }
        }
        fragment frag on HasA { intfField }
        "#,
            "operation.graphql",
            None,
        )
        .unwrap(), @r###"
        fragment frag on HasA {
          intfField
        }

        {
          intf {
            ... on HasA {
              a
            }
            ...frag
          }
        }
        "###);
}

fn contains_field(ss: &SelectionSet, field_name: Name) -> bool {
    ss.selections.contains_key(&SelectionKey::Field {
        response_name: field_name,
        directives: Default::default(),
    })
}

fn is_named_field(sk: &SelectionKey, name: Name) -> bool {
    matches!(sk,
            SelectionKey::Field { response_name, directives: _ }
                if *response_name == name)
}

fn get_value_at_path<'a>(ss: &'a SelectionSet, path: &[Name]) -> Option<&'a Selection> {
    let Some((first, rest)) = path.split_first() else {
        // Error: empty path
        return None;
    };
    let result = ss.selections.get(&SelectionKey::Field {
        response_name: (*first).clone(),
        directives: Default::default(),
    });
    let Some(value) = result else {
        // Error: No matching field found.
        return None;
    };
    if rest.is_empty() {
        // Base case => We are done.
        Some(value)
    } else {
        // Recursive case
        match value.selection_set().unwrap() {
            None => None, // Error: Sub-selection expected, but not found.
            Some(ss) => get_value_at_path(ss, rest),
        }
    }
}

#[cfg(test)]
mod make_selection_tests {
    use super::super::*;
    use super::*;

    const SAMPLE_OPERATION_DOC: &str = r#"
        type Query {
            foo: Foo!
        }

        type Foo {
            a: Int!
            b: Int!
            c: Int!
        }

        query TestQuery {
            foo {
                a
                b
                c
            }
        }
        "#;

    // Tests if `make_selection`'s subselection ordering is preserved.
    #[test]
    fn test_make_selection_order() {
        let (schema, executable_document) = parse_schema_and_operation(SAMPLE_OPERATION_DOC);
        let normalized_operation = normalize_operation(
            executable_document.operations.get(None).unwrap(),
            Default::default(),
            &schema,
            &Default::default(),
        )
        .unwrap();

        let foo = get_value_at_path(&normalized_operation.selection_set, &[name!("foo")])
            .expect("foo should exist");
        assert_eq!(foo.to_string(), "foo { a b c }");

        // Create a new foo with a different selection order using `make_selection`.
        let clone_selection_at_path = |base: &Selection, path: &[Name]| {
            let base_selection_set = base.selection_set().unwrap().unwrap();
            let selection = get_value_at_path(base_selection_set, path).expect("path should exist");
            let subselections = SelectionSet::from_selection(
                base_selection_set.type_position.clone(),
                selection.clone(),
            );
            Selection::from_element(base.element().unwrap(), Some(subselections)).unwrap()
        };

        let foo_with_a = clone_selection_at_path(foo, &[name!("a")]);
        let foo_with_b = clone_selection_at_path(foo, &[name!("b")]);
        let foo_with_c = clone_selection_at_path(foo, &[name!("c")]);
        let new_selection = SelectionSet::make_selection(
            &schema,
            &foo.element().unwrap().parent_type_position(),
            [foo_with_c, foo_with_b, foo_with_a].iter(),
            /*named_fragments*/ &Default::default(),
        )
        .unwrap();
        // Make sure the ordering of c, b and a is preserved.
        assert_eq!(new_selection.to_string(), "foo { c b a }");
    }
}

#[cfg(test)]
mod lazy_map_tests {
    use super::super::*;
    use super::*;

    // recursive filter implementation using `lazy_map`
    fn filter_rec(
        ss: &SelectionSet,
        pred: &impl Fn(&Selection) -> bool,
    ) -> Result<SelectionSet, FederationError> {
        ss.lazy_map(/*named_fragments*/ &Default::default(), |s| {
            if !pred(s) {
                return Ok(SelectionMapperReturn::None);
            }
            match s.selection_set()? {
                // Base case: leaf field
                None => Ok(s.clone().into()),

                // Recursive case: non-leaf field
                Some(inner_ss) => {
                    let updated_ss = filter_rec(inner_ss, pred).map(Some)?;
                    // see if `updated_ss` is an non-empty selection set.
                    if matches!(updated_ss, Some(ref sub_ss) if !sub_ss.is_empty()) {
                        s.with_updated_selection_set(updated_ss).map(|ss| ss.into())
                    } else {
                        Ok(SelectionMapperReturn::None)
                    }
                }
            }
        })
    }

    const SAMPLE_OPERATION_DOC: &str = r#"
        type Query {
            foo: Foo!
            some_int: Int!
            foo2: Foo!
        }

        type Foo {
            id: ID!
            bar: String!
            baz: Int
        }

        query TestQuery {
            foo {
                id
                bar
            },
            some_int
            foo2 {
                bar
            }
        }
        "#;

    // Tests `lazy_map` via `filter_rec` function.
    #[test]
    fn test_lazy_map() {
        let (schema, executable_document) = parse_schema_and_operation(SAMPLE_OPERATION_DOC);
        let normalized_operation = normalize_operation(
            executable_document.operations.get(None).unwrap(),
            Default::default(),
            &schema,
            &Default::default(),
        )
        .unwrap();

        let selection_set = normalized_operation.selection_set;

        // Select none
        let select_none = filter_rec(&selection_set, &|_| false).unwrap();
        assert!(select_none.is_empty());

        // Select all
        let select_all = filter_rec(&selection_set, &|_| true).unwrap();
        assert!(select_all == selection_set);

        // Remove `foo`
        let remove_foo =
            filter_rec(&selection_set, &|s| !is_named_field(&s.key(), name!("foo"))).unwrap();
        assert!(contains_field(&remove_foo, name!("some_int")));
        assert!(contains_field(&remove_foo, name!("foo2")));
        assert!(!contains_field(&remove_foo, name!("foo")));

        // Remove `bar`
        let remove_bar =
            filter_rec(&selection_set, &|s| !is_named_field(&s.key(), name!("bar"))).unwrap();
        // "foo2" should be removed, since it has no sub-selections left.
        assert!(!contains_field(&remove_bar, name!("foo2")));
    }

    fn add_typename_if(
        ss: &SelectionSet,
        pred: &impl Fn(&Selection) -> bool,
    ) -> Result<SelectionSet, FederationError> {
        ss.lazy_map(/*named_fragments*/ &Default::default(), |s| {
            let to_add_typename = pred(s);
            let updated = s.map_selection_set(|ss| add_typename_if(ss, pred).map(Some))?;
            if !to_add_typename {
                return Ok(updated.into());
            }

            let parent_type_pos = s.element()?.parent_type_position();
            // "__typename" field
            let field_element =
                Field::new_introspection_typename(s.schema(), &parent_type_pos, None);
            let typename_selection =
                Selection::from_element(field_element.into(), /*subselection*/ None)?;
            // return `updated` and `typename_selection`
            Ok([updated, typename_selection].into_iter().collect())
        })
    }

    // Tests `lazy_map` via `add_typename_if` function.
    #[test]
    fn test_lazy_map2() {
        let (schema, executable_document) = parse_schema_and_operation(SAMPLE_OPERATION_DOC);
        let normalized_operation = normalize_operation(
            executable_document.operations.get(None).unwrap(),
            Default::default(),
            &schema,
            &Default::default(),
        )
        .unwrap();

        let selection_set = normalized_operation.selection_set;

        // Add __typename next to any "id" field.
        let result =
            add_typename_if(&selection_set, &|s| is_named_field(&s.key(), name!("id"))).unwrap();

        // The top level won't have __typename, since it doesn't have "id".
        assert!(!contains_field(&result, name!("__typename")));

        // Check if "foo" has "__typename".
        get_value_at_path(&result, &[name!("foo"), name!("__typename")])
            .expect("foo.__typename should exist");
    }
}

fn field_element(
    schema: &ValidFederationSchema,
    object: apollo_compiler::Name,
    field: apollo_compiler::Name,
) -> OpPathElement {
    OpPathElement::Field(super::Field::new(super::FieldData {
        schema: schema.clone(),
        field_position: ObjectTypeDefinitionPosition::new(object)
            .field(field)
            .into(),
        alias: None,
        arguments: Default::default(),
        directives: Default::default(),
        sibling_typename: None,
    }))
}

const ADD_AT_PATH_TEST_SCHEMA: &str = r#"
        type A { b: B }
        type B { c: C }
        type C implements X {
            d: Int
            e(arg: Int): Int
        }
        type D implements X {
            d: Int
            e: Boolean
        }

        interface X {
            d: Int
        }
        type Query {
            a: A
            something: Boolean!
            scalar: String
            withArg(arg: Int): X
        }
    "#;

#[test]
fn add_at_path_merge_scalar_fields() {
    let schema =
        apollo_compiler::Schema::parse_and_validate(ADD_AT_PATH_TEST_SCHEMA, "schema.graphql")
            .unwrap();
    let schema = ValidFederationSchema::new(schema).unwrap();

    let mut selection_set = SelectionSet::empty(
        schema.clone(),
        ObjectTypeDefinitionPosition::new(name!("Query")).into(),
    );

    selection_set
        .add_at_path(
            &[field_element(&schema, name!("Query"), name!("scalar")).into()],
            None,
        )
        .unwrap();

    selection_set
        .add_at_path(
            &[field_element(&schema, name!("Query"), name!("scalar")).into()],
            None,
        )
        .unwrap();

    insta::assert_snapshot!(selection_set, @r#"{ scalar }"#);
}

#[test]
fn add_at_path_merge_subselections() {
    let schema =
        apollo_compiler::Schema::parse_and_validate(ADD_AT_PATH_TEST_SCHEMA, "schema.graphql")
            .unwrap();
    let schema = ValidFederationSchema::new(schema).unwrap();

    let mut selection_set = SelectionSet::empty(
        schema.clone(),
        ObjectTypeDefinitionPosition::new(name!("Query")).into(),
    );

    let path_to_c = [
        field_element(&schema, name!("Query"), name!("a")).into(),
        field_element(&schema, name!("A"), name!("b")).into(),
        field_element(&schema, name!("B"), name!("c")).into(),
    ];

    selection_set
        .add_at_path(
            &path_to_c,
            Some(
                &SelectionSet::parse(
                    schema.clone(),
                    ObjectTypeDefinitionPosition::new(name!("C")).into(),
                    "d",
                )
                .unwrap()
                .into(),
            ),
        )
        .unwrap();
    selection_set
        .add_at_path(
            &path_to_c,
            Some(
                &SelectionSet::parse(
                    schema.clone(),
                    ObjectTypeDefinitionPosition::new(name!("C")).into(),
                    "e(arg: 1)",
                )
                .unwrap()
                .into(),
            ),
        )
        .unwrap();

    insta::assert_snapshot!(selection_set, @r#"{ a { b { c { d e(arg: 1) } } } }"#);
}

#[test]
fn add_at_path_collapses_unnecessary_fragments() {
    let schema =
        apollo_compiler::Schema::parse_and_validate(ADD_AT_PATH_TEST_SCHEMA, "schema.graphql")
            .unwrap();
    let schema = ValidFederationSchema::new(schema).unwrap();

    let mut selection_set = SelectionSet::empty(
        schema.clone(),
        ObjectTypeDefinitionPosition::new(name!("Query")).into(),
    );
    selection_set
        .add_at_path(
            &[
                field_element(&schema, name!("Query"), name!("a")).into(),
                field_element(&schema, name!("A"), name!("b")).into(),
                field_element(&schema, name!("B"), name!("c")).into(),
            ],
            Some(
                &SelectionSet::parse(
                    schema.clone(),
                    InterfaceTypeDefinitionPosition::new(name!("X")).into(),
                    "... on C { d }",
                )
                .unwrap()
                .into(),
            ),
        )
        .unwrap();

    insta::assert_snapshot!(selection_set, @r#"{ a { b { c { d } } } }"#);
}

#[test]
fn test_expand_all_fragments1() {
    let operation_with_named_fragment = r#"
          type Query {
            i1: I
            i2: I
          }

          interface I {
            a: Int
            b: Int
          }

          type T implements I {
            a: Int
            b: Int
          }

          query {
            i1 {
              ... on T {
                ...Frag
              }
            }
            i2 {
              ... on T {
                ...Frag
              }
            }
          }

          fragment Frag on I {
            b
          }
        "#;
    let (schema, executable_document) = parse_schema_and_operation(operation_with_named_fragment);
    if let Ok(operation) = executable_document.operations.get(None) {
        let mut normalized_operation = normalize_operation(
            operation,
            NamedFragments::new(&executable_document.fragments, &schema),
            &schema,
            &IndexSet::default(),
        )
        .unwrap();
        normalized_operation.named_fragments = Default::default();
        insta::assert_snapshot!(normalized_operation, @r###"
            {
              i1 {
                ... on T {
                  b
                }
              }
              i2 {
                ... on T {
                  b
                }
              }
            }
            "###);
    }
}
