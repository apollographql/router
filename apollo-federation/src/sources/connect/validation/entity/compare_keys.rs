use apollo_compiler::collections::IndexMap;
use apollo_compiler::executable::FieldSet;
use apollo_compiler::executable::Selection;
use apollo_compiler::executable::SelectionSet;
use apollo_compiler::validation::Valid;
use apollo_compiler::Name;
use apollo_compiler::Node;

/// Remove repetition from selection sets in a field set
/// Ex: `foo { bar { baz } } foo { bar { quuz } }` -> `foo { bar { baz quux } }`
pub(super) fn optimize_field_set(field_set: &FieldSet) -> Valid<FieldSet> {
    let mut field_set = field_set.clone();

    fn merge_selections(target: &mut SelectionSet, source: &SelectionSet) {
        let mut fields = IndexMap::<Name, Node<apollo_compiler::executable::Field>>::default();
        for selection in source.selections.iter() {
            match selection {
                Selection::Field(field) => {
                    fields
                        .entry(field.name.clone())
                        .and_modify(|new_field| {
                            let new_field = new_field.make_mut();
                            new_field
                                .selection_set
                                .selections
                                .extend(field.selection_set.selections.iter().cloned());
                        })
                        .or_insert_with(|| field.clone());
                }
                Selection::FragmentSpread(_) => {}
                Selection::InlineFragment(_) => {}
            }
        }

        for (_, field) in fields.iter_mut() {
            let source = field.selection_set.clone();
            let mut selection_set = SelectionSet::new(field.selection_set.ty.clone());
            merge_selections(&mut selection_set, &source);

            let field = field.make_mut();
            field.selection_set = selection_set;

            target
                .selections
                .push(Selection::Field(field.clone().into()));
        }
    }

    let mut selection_set = SelectionSet::new(field_set.selection_set.ty.clone());
    merge_selections(&mut selection_set, &field_set.selection_set);

    field_set.selection_set = selection_set;
    Valid::assume_valid(field_set)
}

// --- Semantic comparison of selection sets -----------------------------------
// NOTE: this code is derived from apollo-router's plan diffing code.
// -----------------------------------------------------------------------------

/// Returns true if `inner` is a subset of `outer`.
pub(super) fn field_set_is_subset(inner: &FieldSet, outer: &FieldSet) -> bool {
    inner.selection_set.ty == outer.selection_set.ty
        && vec_includes_as_set(
            &outer.selection_set.selections,
            &inner.selection_set.selections,
            selection_is_subset,
        )
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

// `this` vector includes `other` vector as a set
fn vec_includes_as_set<T>(this: &[T], other: &[T], item_matches: impl Fn(&T, &T) -> bool) -> bool {
    other.iter().all(|other_node| {
        this.iter()
            .any(|this_node| item_matches(this_node, other_node))
    })
}

#[cfg(test)]
mod tests {
    use apollo_compiler::executable::FieldSet;
    use apollo_compiler::name;
    use apollo_compiler::validation::Valid;
    use apollo_compiler::Schema;
    use rstest::rstest;

    use super::field_set_is_subset;
    use super::optimize_field_set;

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

    #[rstest]
    #[case("a b { x x } a b { y } c c", "a b { x y } c")]
    #[case("a b { x y } c", "a b { x y } c")]
    fn test_optimize_field_set(#[case] input: &str, #[case] expected: &str) {
        let schema = schema();
        let input = FieldSet::parse_and_validate(&schema, name!(T), input, "inner").unwrap();
        let expected = FieldSet::parse_and_validate(&schema, name!(T), expected, "outer").unwrap();
        let actual = optimize_field_set(&input);
        assert_eq!(actual.selection_set, expected.selection_set);
    }
}
