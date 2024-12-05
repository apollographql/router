use apollo_compiler::executable::FieldSet;
use apollo_compiler::executable::Selection;

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
