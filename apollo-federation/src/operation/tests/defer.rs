use apollo_compiler::Node;

use super::parse_operation;
use super::parse_schema;
use crate::operation::Selection;

const DEFAULT_SCHEMA: &str = r#"
type A {
  one: Int
  two: Int
  three: Int
  b: B
}

type B {
  one: Boolean
  two: Boolean
  three: Boolean
  a: A
}

union AorB = A | B

type Query {
  a: A
  b: B
  either: AorB
}

directive @defer(if: Boolean! = true, label: String) on FRAGMENT_SPREAD | INLINE_FRAGMENT
"#;

#[test]
fn without_defer_simple() {
    let schema = parse_schema(DEFAULT_SCHEMA);

    let operation = parse_operation(
        &schema,
        r#"
      {
        ... @defer { a { one } }
        b {
          ... @defer { two }
        }
      }
    "#,
    );

    let without_defer = operation.without_defer().unwrap();

    insta::assert_snapshot!(without_defer, @r#"
      {
        a {
          one
        }
        b {
          two
        }
      }
    "#);
}

#[test]
fn without_defer_named_fragment() {
    let schema = parse_schema(DEFAULT_SCHEMA);

    let operation = parse_operation(
        &schema,
        r#"
      {
        b { ...frag @defer }
        either { ...frag }
      }
      fragment frag on B {
        two
      }
    "#,
    );

    let without_defer = operation.without_defer().unwrap();

    insta::assert_snapshot!(without_defer, @r#"
      fragment frag on B {
        two
      }

      {
        b {
          ...frag
        }
        either {
          ...frag
        }
      }
    "#);
}

#[test]
fn without_defer_merges_fragment() {
    let schema = parse_schema(DEFAULT_SCHEMA);

    let operation = parse_operation(
        &schema,
        r#"
      {
        a { one }
        either {
          ... on B {
            one
          }
          ... on B @defer {
            two
          }
        }
      }
    "#,
    );

    let without_defer = operation.without_defer().unwrap();

    insta::assert_snapshot!(without_defer, @r#"
      {
        a {
          one
        }
        either {
          ... on B {
            one
            two
          }
        }
      }
    "#);
}

#[test]
fn without_defer_fragment_references() {
    let schema = parse_schema(DEFAULT_SCHEMA);

    let operation = parse_operation(
        &schema,
        r#"
      fragment a on A {
        ... @defer { ...b }
      }
      fragment b on A {
        one
        b {
          ...c @defer
        }
      }
      fragment c on B {
        two
      }
      fragment entry on Query {
        a { ...a }
      }

      { ...entry }
    "#,
    );

    let without_defer = operation.without_defer().unwrap();

    insta::assert_snapshot!(without_defer, @r###"
    fragment c on B {
      two
    }

    fragment b on A {
      one
      b {
        ...c
      }
    }

    fragment a on A {
      ...b
    }

    fragment entry on Query {
      a {
        ...a
      }
    }

    {
      ...entry
    }
    "###);

    let frag_a = without_defer.named_fragments.get("a").unwrap();
    let frag_b = without_defer.named_fragments.get("b").unwrap();

    let (_, Selection::FragmentSpread(frag_a_spread)) =
        frag_a.selection_set.selections.first().unwrap()
    else {
        panic!("first selection from frag a should be ...b");
    };

    assert_eq!(
        frag_a_spread.selection_set, frag_b.selection_set,
        "FragmentSpreadSelection's selection set should be updated"
    );
}
