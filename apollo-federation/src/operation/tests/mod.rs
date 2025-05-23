use apollo_compiler::ExecutableDocument;
use apollo_compiler::collections::IndexSet;
use apollo_compiler::name;
use apollo_compiler::parser::Parser;
use apollo_compiler::schema::Schema;

use super::Field;
use super::Name;
use super::Operation;
use super::Selection;
use super::SelectionKey;
use super::SelectionSet;
use super::normalize_operation;
use crate::SingleFederationError;
use crate::error::FederationError;
use crate::query_graph::graph_path::operation::OpPathElement;
use crate::schema::ValidFederationSchema;
use crate::schema::position::InterfaceTypeDefinitionPosition;
use crate::schema::position::ObjectTypeDefinitionPosition;

macro_rules! assert_normalized {
    ($schema_doc: expr, $query: expr, @$expected: literal) => {{
        let schema = parse_schema($schema_doc);
        let without_fragments = parse_and_expand(&schema, $query).expect("operation is valid and can be normalized");
        insta::assert_snapshot!(without_fragments, @$expected);
        without_fragments
    }};
}

macro_rules! assert_normalized_equal {
    ($schema_doc: expr, $query: expr, $expected: literal) => {{
        let normalized = assert_normalized!($schema_doc, $query, @$expected);

        let schema = parse_schema($schema_doc);
        let original_document = ExecutableDocument::parse_and_validate(schema.schema(), $query, "query.graphql").expect("valid document");
        let normalized_document = normalized.clone().try_into().expect("valid normalized document");
        // since compare operations just check if a query is subset of another one
        // we verify that both A ⊆ B and B ⊆ A are true which means that A = B
        compare_operations(&schema, &original_document, &normalized_document).expect("original query is a subset of the normalized one");
        compare_operations(&schema, &normalized_document, &original_document).expect("normalized query is a subset of original one");
        normalized
    }};
}

macro_rules! assert_equal_ops {
    ($schema: expr, $original_document: expr, $minified_document: expr) => {
        // since compare operations just check if a query is subset of another one
        // we verify that both A ⊆ B and B ⊆ A are true which means that A = B
        compare_operations($schema, $original_document, $minified_document)
            .expect("original document is a subset of minified one");
        compare_operations($schema, $minified_document, $original_document)
            .expect("minified document is a subset of original one");
    };
}
pub(super) use assert_equal_ops;

use crate::correctness::compare_operations;

pub(super) fn parse_schema_and_operation(
    schema_and_operation: &str,
) -> (ValidFederationSchema, ExecutableDocument) {
    let (schema, executable_document) = Parser::new()
        .parse_mixed_validate(schema_and_operation, "document.graphql")
        .expect("valid schema and operation");
    let executable_document = executable_document.into_inner();
    let schema = ValidFederationSchema::new(schema).expect("valid federation schema");
    (schema, executable_document)
}

pub(super) fn parse_schema(schema_doc: &str) -> ValidFederationSchema {
    let schema = Schema::parse_and_validate(schema_doc, "schema.graphql").expect("valid schema");
    ValidFederationSchema::new(schema).expect("valid federation schema")
}

pub(super) fn parse_operation(schema: &ValidFederationSchema, query: &str) -> Operation {
    Operation::parse(schema.clone(), query, "query.graphql").expect("valid operation")
}

pub(super) fn parse_and_expand(
    schema: &ValidFederationSchema,
    query: &str,
) -> Result<Operation, FederationError> {
    let doc = ExecutableDocument::parse_and_validate(schema.schema(), query, "query.graphql")?;

    let operation = doc
        .operations
        .iter()
        .next()
        .expect("must have an operation");

    normalize_operation(
        operation,
        &doc.fragments,
        schema,
        &Default::default(),
        &never_cancel,
    )
}

/// The `normalize_operation()` function has a `check_cancellation` parameter that we'll want to
/// configure to never cancel during tests. We create a convenience function here for that purpose.
pub(crate) fn never_cancel() -> Result<(), SingleFederationError> {
    Ok(())
}

#[test]
fn expands_named_fragments() {
    let schema = r#"
      type Query {
        foo: Foo
      }

      type Foo {
        id: ID!
        bar: String!
        baz: Int
      }
    "#;
    let operation = r#"
      query {
        foo {
          id
          ...Bar
        }
      }
      fragment Bar on Foo {
        bar
        baz
      }
    "#;
    assert_normalized_equal!(
        schema,
        operation,
        r###"
      {
        foo {
          id
          bar
          baz
        }
      }
    "###
    );
}

#[test]
fn expands_and_deduplicates_fragments() {
    let schema = r#"
      type Query {
        foo: Foo
      }

      type Foo {
        id: ID!
        bar: String!
        baz: String
      }
    "#;
    let operation = r#"
      query {
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
    "#;
    assert_normalized_equal!(
        schema,
        operation,
        r###"
      {
        foo {
          id
          bar
          baz
        }
      }
    "###
    );
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
            &executable_document.fragments,
            &schema,
            &IndexSet::default(),
            &never_cancel,
        )
        .unwrap();

        assert!(normalized_operation.selection_set.selections.is_empty());
    }
}

#[test]
fn merge_same_fields_without_directives() {
    let schema = r#"
      type Query {
        t: T
      }

      type T {
        v1: Int
        v2: String
      }
    "#;
    let operation = r#"
      query {
        t {
          v1
        }
        t {
          v2
        }
      }
    "#;
    assert_normalized_equal!(
        schema,
        operation,
        r###"
      {
        t {
          v1
          v2
        }
      }
    "###
    );
}

#[test]
fn merge_same_fields_with_same_directive() {
    let schema = r#"
      type Query {
        t: T
      }

      type T {
        v1: Int
        v2: String
      }
    "#;
    let operation = r#"
      query Test($skipIf: Boolean!) {
        t @skip(if: $skipIf) {
          v1
        }
        t @skip(if: $skipIf) {
          v2
        }
      }
    "#;
    assert_normalized_equal!(
        schema,
        operation,
        r###"
      query Test($skipIf: Boolean!) {
        t @skip(if: $skipIf) {
          v1
          v2
        }
      }
    "###
    );
}

#[test]
fn merge_same_fields_with_same_directive_but_different_arg_order() {
    let schema = r#"
      directive @customSkip(if: Boolean!, label: String!) on FIELD | INLINE_FRAGMENT

      type Query {
        t: T
      }

      type T {
        v1: Int
        v2: String
      }
    "#;
    let operation = r#"
      query Test($skipIf: Boolean!) {
        t @customSkip(if: $skipIf, label: "foo") {
          v1
        }
        t @customSkip(label: "foo", if: $skipIf) {
          v2
        }
      }
    "#;
    assert_normalized_equal!(
        schema,
        operation,
        r###"
      query Test($skipIf: Boolean!) {
        t @customSkip(if: $skipIf, label: "foo") {
          v1
          v2
        }
      }
    "###
    );
}

#[test]
fn do_not_merge_when_only_one_field_specifies_directive() {
    let schema = r#"
      type Query {
        t: T
      }

      type T {
        v1: Int
        v2: String
      }
    "#;
    let operation = r#"
      query Test($skipIf: Boolean!) {
        t {
          v1
        }
        t @skip(if: $skipIf) {
          v2
        }
      }
    "#;
    assert_normalized_equal!(
        schema,
        operation,
        r###"
      query Test($skipIf: Boolean!) {
        t {
          v1
        }
        t @skip(if: $skipIf) {
          v2
        }
      }
    "###
    );
}

#[test]
fn do_not_merge_when_fields_have_different_directives() {
    let schema = r#"
      type Query {
        t: T
      }

      type T {
        v1: Int
        v2: String
      }
    "#;
    let operation = r#"
      query Test($skip1: Boolean!, $skip2: Boolean!) {
        t @skip(if: $skip1) {
          v1
        }
        t @skip(if: $skip2) {
          v2
        }
      }
    "#;
    assert_normalized_equal!(
        schema,
        operation,
        r###"
      query Test($skip1: Boolean!, $skip2: Boolean!) {
        t @skip(if: $skip1) {
          v1
        }
        t @skip(if: $skip2) {
          v2
        }
      }
    "###
    );
}

#[test]
fn do_not_merge_fields_with_defer_directive() {
    let schema = r#"
      directive @defer(label: String, if: Boolean! = true) on FRAGMENT_SPREAD | INLINE_FRAGMENT

      type Query {
        t: T
      }

      type T {
        v1: Int
        v2: String
      }
    "#;
    let operation = r#"
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
    "#;
    assert_normalized_equal!(
        schema,
        operation,
        r###"
      query Test {
        t {
          ... @defer {
            v1
          }
          ... @defer {
            v2
          }
        }
      }
    "###
    );
}

#[test]
fn merge_nested_field_selections() {
    let schema = r#"
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
    "#;
    assert_normalized_equal!(
        schema,
        nested_operation,
        r###"
      query Test {
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
      }
    "###
    );
}

//
// inline fragments
//
#[test]
fn merge_same_fragment_without_directives() {
    let schema = r#"
      type Query {
        t: T
      }

      type T {
        v1: Int
        v2: String
      }
    "#;
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
    "#;
    assert_normalized_equal!(
        schema,
        operation_with_fragments,
        r###"
      query Test {
        t {
          v1
          v2
        }
      }
    "###
    );
}

#[test]
fn merge_same_fragments_with_same_directives() {
    let schema = r#"
      type Query {
        t: T
      }

      type T {
        v1: Int
        v2: String
      }
    "#;
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
    "#;
    assert_normalized_equal!(
        schema,
        operation_fragments_with_directives,
        r###"
      query Test($skipIf: Boolean!) {
        t {
          ... on T @skip(if: $skipIf) {
            v1
            v2
          }
        }
      }
    "###
    );
}

#[test]
fn merge_same_fragments_with_same_directive_but_different_arg_order() {
    let schema = r#"
      directive @customSkip(if: Boolean!, label: String!) on FIELD | INLINE_FRAGMENT

      type Query {
        t: T
      }

      type T {
        v1: Int
        v2: String
      }
    "#;
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
    "#;
    assert_normalized_equal!(
        schema,
        operation_fragments_with_directives_args_order,
        r###"
      query Test($skipIf: Boolean!) {
        t {
          ... on T @customSkip(if: $skipIf, label: "foo") {
            v1
            v2
          }
        }
      }
    "###
    );
}

#[test]
fn do_not_merge_when_only_one_fragment_specifies_directive() {
    let schema = r#"
      type Query {
        t: T
      }

      type T {
        v1: Int
        v2: String
      }
    "#;
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
    "#;
    assert_normalized_equal!(
        schema,
        operation_one_fragment_with_directive,
        r###"
      query Test($skipIf: Boolean!) {
        t {
          v1
          ... on T @skip(if: $skipIf) {
            v2
          }
        }
      }
    "###
    );
}

#[test]
fn do_not_merge_when_fragments_have_different_directives() {
    let schema = r#"
      type Query {
        t: T
      }

      type T {
        v1: Int
        v2: String
      }
    "#;
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
    "#;
    assert_normalized_equal!(
        schema,
        operation_fragments_with_different_directive,
        r###"
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
    "###
    );
}

#[test]
fn do_not_merge_fragments_with_defer_directive() {
    let schema = r#"
      directive @defer(label: String, if: Boolean! = true) on FRAGMENT_SPREAD | INLINE_FRAGMENT

      type Query {
        t: T
      }

      type T {
        v1: Int
        v2: String
      }
    "#;
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
    "#;
    assert_normalized_equal!(
        schema,
        operation_fragments_with_defer,
        r###"
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
    "###
    );
}

#[test]
fn merge_nested_fragments() {
    let schema = r#"
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
    "#;
    assert_normalized_equal!(
        schema,
        operation_nested_fragments,
        r###"
      query Test {
        t {
          t1
          v {
            v1
            v2
          }
          t2
        }
      }
    "###
    );
}

#[test]
fn removes_sibling_typename() {
    let schema = r#"
      type Query {
        foo: Foo
      }

      type Foo {
        v1: ID!
        v2: String
      }
    "#;
    let operation_with_typename = r#"
      query TestQuery {
        foo {
          __typename
          v1
          v2
        }
      }
    "#;
    // The __typename selection is hidden (attached to its sibling).
    assert_normalized!(schema, operation_with_typename, @r###"
      query TestQuery {
        foo {
          v1
          v2
        }
      }
    "###);
}

#[test]
fn keeps_typename_if_no_other_selection() {
    let schema = r#"
      type Query {
        foo: Foo
      }

      type Foo {
        v1: ID!
        v2: String
      }
    "#;
    let operation_with_single_typename = r#"
      query TestQuery {
        foo {
          __typename
        }
      }
    "#;
    // The __typename selection is kept because it's the only selection.
    assert_normalized_equal!(
        schema,
        operation_with_single_typename,
        r###"
      query TestQuery {
        foo {
          __typename
        }
      }
    "###
    );
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
            &executable_document.fragments,
            &schema,
            &interface_objects,
            &never_cancel,
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
    let schema = Schema::parse_and_validate(
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
            schema,
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
        )
        .unwrap(), @r###"
        {
          intf {
            ... on HasA {
              a
              intfField
            }
          }
        }
        "###);
}

fn contains_field(ss: &SelectionSet, field_name: Name) -> bool {
    ss.selections.contains_key(SelectionKey::Field {
        response_name: &field_name,
        directives: &Default::default(),
    })
}

fn is_named_field(sk: SelectionKey, name: Name) -> bool {
    matches!(sk,
            SelectionKey::Field { response_name, directives: _ }
                if *response_name == name)
}

fn get_value_at_path<'a>(ss: &'a SelectionSet, path: &[Name]) -> Option<&'a Selection> {
    let Some((first, rest)) = path.split_first() else {
        // Error: empty path
        return None;
    };
    let value = ss.selections.get(SelectionKey::field_name(first))?;
    if rest.is_empty() {
        // Base case => We are done.
        Some(value)
    } else {
        // Recursive case
        match value.selection_set() {
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
            &Default::default(),
            &schema,
            &Default::default(),
            &never_cancel,
        )
        .unwrap();

        let foo = get_value_at_path(&normalized_operation.selection_set, &[name!("foo")])
            .expect("foo should exist");
        assert_eq!(foo.to_string(), "foo { a b c }");

        // Create a new foo with a different selection order using `make_selection`.
        let clone_selection_at_path = |base: &Selection, path: &[Name]| {
            let base_selection_set = base.selection_set().unwrap();
            let selection = get_value_at_path(base_selection_set, path).expect("path should exist");
            let subselections = SelectionSet::from_selection(
                base_selection_set.type_position.clone(),
                selection.clone(),
            );
            Selection::from_element(base.element(), Some(subselections)).unwrap()
        };

        let foo_with_a = clone_selection_at_path(foo, &[name!("a")]);
        let foo_with_b = clone_selection_at_path(foo, &[name!("b")]);
        let foo_with_c = clone_selection_at_path(foo, &[name!("c")]);
        let new_selection = SelectionSet::make_selection(
            &schema,
            &foo.element().parent_type_position(),
            [foo_with_c, foo_with_b, foo_with_a].iter(),
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
        ss.lazy_map(|s| {
            if !pred(s) {
                return Ok(SelectionMapperReturn::None);
            }
            match s.selection_set() {
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
            &Default::default(),
            &schema,
            &Default::default(),
            &never_cancel,
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
            filter_rec(&selection_set, &|s| !is_named_field(s.key(), name!("foo"))).unwrap();
        assert!(contains_field(&remove_foo, name!("some_int")));
        assert!(contains_field(&remove_foo, name!("foo2")));
        assert!(!contains_field(&remove_foo, name!("foo")));

        // Remove `bar`
        let remove_bar =
            filter_rec(&selection_set, &|s| !is_named_field(s.key(), name!("bar"))).unwrap();
        // "foo2" should be removed, since it has no sub-selections left.
        assert!(!contains_field(&remove_bar, name!("foo2")));
    }

    fn add_typename_if(
        ss: &SelectionSet,
        pred: &impl Fn(&Selection) -> bool,
    ) -> Result<SelectionSet, FederationError> {
        ss.lazy_map(|s| {
            let to_add_typename = pred(s);
            let updated = s.map_selection_set(|ss| add_typename_if(ss, pred).map(Some))?;
            if !to_add_typename {
                return Ok(updated.into());
            }

            let parent_type_pos = s.element().parent_type_position();
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
            &Default::default(),
            &schema,
            &Default::default(),
            &never_cancel,
        )
        .unwrap();

        let selection_set = normalized_operation.selection_set;

        // Add __typename next to any "id" field.
        let result =
            add_typename_if(&selection_set, &|s| is_named_field(s.key(), name!("id"))).unwrap();

        // The top level won't have __typename, since it doesn't have "id".
        assert!(!contains_field(&result, name!("__typename")));

        // Check if "foo" has "__typename".
        get_value_at_path(&result, &[name!("foo"), name!("__typename")])
            .expect("foo.__typename should exist");
    }
}

fn field_element(schema: &ValidFederationSchema, object: Name, field: Name) -> OpPathElement {
    OpPathElement::Field(Field {
        schema: schema.clone(),
        field_position: ObjectTypeDefinitionPosition::new(object)
            .field(field)
            .into(),
        alias: None,
        arguments: Default::default(),
        directives: Default::default(),
        sibling_typename: None,
    })
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
    let schema = Schema::parse_and_validate(ADD_AT_PATH_TEST_SCHEMA, "schema.graphql").unwrap();
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
    let schema = Schema::parse_and_validate(ADD_AT_PATH_TEST_SCHEMA, "schema.graphql").unwrap();
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
                    schema,
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
    let schema = Schema::parse_and_validate(ADD_AT_PATH_TEST_SCHEMA, "schema.graphql").unwrap();
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
                    InterfaceTypeDefinitionPosition {
                        type_name: name!("X"),
                    }
                    .into(),
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
        let normalized_operation = normalize_operation(
            operation,
            &executable_document.fragments,
            &schema,
            &IndexSet::default(),
            &never_cancel,
        )
        .unwrap();
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

#[test]
fn used_variables() {
    let schema = r#"
        input Ints { a: Int }
        input LInts { a: [Int], b: LInts }
        type Query {
            f(ints: [Int]): Int
            g(ints: Ints): Int
            h(ints: LInts): Int
            subquery: Query
        }
    "#;
    let query = r#"
        query ($a: Int, $b: Int, $c: Int, $d: Int) {
            f(ints: [1, $a, 2])
            g(ints: { a: $b })
            subquery {
                h(ints: {
                    b: {
                        a: [$d, $d]
                        b: {
                            a: [$c, 3, 4]
                        }
                    }
                })
            }
        }
    "#;

    let valid = parse_schema(schema);
    let operation = Operation::parse(valid, query, "used_variables.graphql").unwrap();

    let mut variables = operation
        .selection_set
        .used_variables()
        .into_iter()
        .collect::<Vec<_>>();
    variables.sort();
    assert_eq!(variables, ["a", "b", "c", "d"]);

    let Selection::Field(subquery) = operation
        .selection_set
        .selections
        .get(SelectionKey::field_name(&name!("subquery")))
        .unwrap()
    else {
        unreachable!();
    };
    let mut variables = subquery
        .selection_set
        .as_ref()
        .unwrap()
        .used_variables()
        .into_iter()
        .collect::<Vec<_>>();
    variables.sort();
    assert_eq!(variables, ["c", "d"], "works for a subset of the query");
}

#[test]
fn directive_propagation() {
    let schema_doc = r#"
        type Query {
          t1: T
          t2: T
          t3: T
        }

        type T {
          a: Int
          b: Int
          c: Int
          d: Int
        }

        directive @fragDefOnly on FRAGMENT_DEFINITION
        directive @fragSpreadOnly on FRAGMENT_SPREAD
        directive @fragInlineOnly on INLINE_FRAGMENT
        directive @fragAll on FRAGMENT_DEFINITION | FRAGMENT_SPREAD | INLINE_FRAGMENT
    "#;

    let schema = parse_schema(schema_doc);

    let query = parse_and_expand(
        &schema,
        r#"
        fragment DirectiveOnDef on T @fragDefOnly @fragAll { a }
        query {
          t2 {
            ... on T @fragInlineOnly @fragAll { a }
          }
          t3 {
            ...DirectiveOnDef @fragAll
          }
        }
    "#,
    )
    .expect("directive applications to be valid");
    insta::assert_snapshot!(query, @r###"
    {
      t2 {
        ... on T @fragInlineOnly @fragAll {
          a
        }
      }
      t3 {
        ... on T @fragAll {
          a
        }
      }
    }
    "###);

    let err = parse_and_expand(
        &schema,
        r#"
        fragment DirectiveOnDef on T @fragDefOnly @fragAll { a }
        query {
          t1 {
            ...DirectiveOnDef @fragSpreadOnly @fragAll
          }
        }
    "#,
    )
    .expect_err("directive @fragSpreadOnly to be rejected");
    insta::assert_snapshot!(err, @"Unsupported custom directive @fragSpreadOnly on fragment spread. Due to query transformations during planning, the router requires directives on fragment spreads to support both the FRAGMENT_SPREAD and INLINE_FRAGMENT locations.");
}

#[test]
fn handles_fragment_matching_at_the_top_level_of_another_fragment() {
    let schema_doc = r#"
      type Query {
        t: T
      }

      type T {
        a: String
        u: U
      }

      type U {
        x: String
        y: String
      }
    "#;

    let query = r#"
        fragment Frag1 on T {
          a
        }

        fragment Frag2 on T {
          u {
            x
            y
          }
          ...Frag1
        }

        fragment Frag3 on Query {
          t {
            ...Frag2
          }
        }

        {
          ...Frag3
        }
    "#;

    assert_normalized_equal!(
        schema_doc,
        query,
        r###"
        {
          t {
            u {
              x
              y
            }
            a
          }
        }
    "###
    );
}

#[test]
fn handles_fragments_used_in_context_where_they_get_trimmed() {
    let schema_doc = r#"
      type Query {
        t1: T1
      }

      interface I {
        x: Int
      }

      type T1 implements I {
        x: Int
        y: Int
      }

      type T2 implements I {
        x: Int
        z: Int
      }
    "#;

    let query = r#"
        fragment FragOnI on I {
          ... on T1 {
            y
          }
          ... on T2 {
            z
          }
        }

        {
          t1 {
            ...FragOnI
          }
        }
    "#;

    assert_normalized_equal!(
        schema_doc,
        query,
        r###"
        {
          t1 {
            y
          }
        }
    "###
    );
}

#[test]
fn handles_fragments_on_union_in_context_with_limited_intersection() {
    let schema_doc = r#"
        type Query {
          t1: T1
        }

        union U = T1 | T2

        type T1 {
          x: Int
        }

        type T2 {
          y: Int
        }
    "#;

    let query = r#"
        fragment OnU on U {
          ... on T1 {
            x
          }
          ... on T2 {
            y
          }
        }

        {
          t1 {
            ...OnU
          }
        }
    "#;

    assert_normalized_equal!(
        schema_doc,
        query,
        r###"
        {
          t1 {
            x
          }
        }
    "###
    );
}

#[test]
fn off_by_1_error() {
    let schema = r#"
      type Query {
        t: T
      }
      type T {
        id: String!
        a: A
        v: V
      }
      type A {
        id: String!
      }
      type V {
        t: T!
      }
    "#;

    let query = r#"
      {
        t {
          ...TFrag
          v {
            t {
              id
              a {
                __typename
                id
              }
            }
          }
        }
      }

      fragment TFrag on T {
        __typename
        id
      }
    "#;

    // The __typename selections are hidden (attached to their siblings).
    assert_normalized!(schema, query,@r###"
      {
        t {
          id
          v {
            t {
              id
              a {
                id
              }
            }
          }
        }
      }
    "###
    );
}

///
/// applied directives
///

#[test]
fn fragments_with_same_directive_in_the_fragment_selection() {
    let schema_doc = r#"
        type Query {
          t1: T
          t2: T
          t3: T
        }

        type T {
          a: Int
          b: Int
          c: Int
          d: Int
        }
    "#;

    let query = r#"
      fragment DirectiveInDef on T {
        a @include(if: $cond1)
      }

      query ($cond1: Boolean!, $cond2: Boolean!) {
        t1 {
          a
        }
        t2 {
          ...DirectiveInDef
        }
        t3 {
          a @include(if: $cond2)
        }
      }
    "#;

    assert_normalized_equal!(
        schema_doc,
        query,
        r###"
      query($cond1: Boolean!, $cond2: Boolean!) {
        t1 {
          a
        }
        t2 {
          a @include(if: $cond1)
        }
        t3 {
          a @include(if: $cond2)
        }
      }
    "###
    );
}

#[test]
fn fragments_with_directive_on_typename() {
    let schema = r#"
        type Query {
          t1: T
          t2: T
          t3: T
        }

        type T {
          a: Int
          b: Int
          c: Int
          d: Int
        }
    "#;
    let query = r#"
        query ($if: Boolean!) {
          t1 { b a ...x }
          t2 { ...x }
        }
        fragment x on T {
            __typename @include(if: $if)
            a
            c
        }
    "#;

    // The __typename selections are kept since they have directive applications.
    assert_normalized_equal!(
        schema,
        query,
        r###"
        query($if: Boolean!) {
          t1 {
            b
            a
            __typename @include(if: $if)
            c
          }
          t2 {
            __typename @include(if: $if)
            a
            c
          }
        }
        "###
    );
}

#[test]
fn fragments_with_non_intersecting_types() {
    let schema = r#"
        type Query {
          t: T
          s: S
          i: I
        }

        interface I {
            a: Int
            b: Int
        }

        type T implements I {
          a: Int
          b: Int

          c: Int
          d: Int
        }
        type S implements I {
          a: Int
          b: Int

          f: Int
          g: Int
        }
    "#;
    let query = r#"
        query ($if: Boolean!) {
          t { ...x }
          s { ...x }
          i { ...x }
        }
        fragment x on I {
            __typename
            a
            b
            ... on T { c d @include(if: $if) }
        }
    "#;

    // The __typename selection is hidden (attached to its sibling).
    assert_normalized!(schema, query, @r###"
        query($if: Boolean!) {
          t {
            a
            b
            c
            d @include(if: $if)
          }
          s {
            a
            b
          }
          i {
            a
            b
            ... on T {
              c
              d @include(if: $if)
            }
          }
        }
    "###);
}
