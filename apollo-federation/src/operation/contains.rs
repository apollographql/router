use apollo_compiler::executable;

use super::FieldSelection;
use super::FragmentSpreadSelection;
use super::HasSelectionKey;
use super::InlineFragmentSelection;
use super::Selection;
use super::SelectionSet;

pub(super) fn is_deferred_selection(directives: &executable::DirectiveList) -> bool {
    directives.has("defer")
}

/// Options for the `.containment()` family of selection functions.
#[derive(Debug, Clone, Copy)]
pub(crate) struct ContainmentOptions {
    /// During query planning, we may add `__typename` selections to sets that did not have it
    /// initially. If the right-hand side has a `__typename` selection but the left-hand side
    /// does not, this option still considers the left-hand side to contain the right-hand side.
    pub(crate) ignore_missing_typename: bool,
}

// Currently Default *can* be derived, but if we add a new option
// here, that might no longer be true.
#[allow(clippy::derivable_impls)]
impl Default for ContainmentOptions {
    fn default() -> Self {
        Self {
            ignore_missing_typename: false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Containment {
    /// The left-hand selection does not fully contain right-hand selection.
    NotContained,
    /// The left-hand selection fully contains the right-hand selection, and more.
    StrictlyContained,
    /// Two selections are equal.
    Equal,
}
impl Containment {
    /// Returns true if the right-hand selection set is strictly contained or equal.
    pub(crate) fn is_contained(self) -> bool {
        matches!(self, Containment::StrictlyContained | Containment::Equal)
    }

    pub(crate) fn is_equal(self) -> bool {
        matches!(self, Containment::Equal)
    }
}

impl Selection {
    pub(crate) fn containment(
        &self,
        other: &Selection,
        options: ContainmentOptions,
    ) -> Containment {
        match (self, other) {
            (Selection::Field(self_field), Selection::Field(other_field)) => {
                self_field.containment(other_field, options)
            }
            (
                Selection::InlineFragment(self_fragment),
                Selection::InlineFragment(_) | Selection::FragmentSpread(_),
            ) => self_fragment.containment(other, options),
            (
                Selection::FragmentSpread(self_fragment),
                Selection::InlineFragment(_) | Selection::FragmentSpread(_),
            ) => self_fragment.containment(other, options),
            _ => Containment::NotContained,
        }
    }

    /// Returns true if this selection is a superset of the other selection.
    pub(crate) fn contains(&self, other: &Selection) -> bool {
        self.containment(other, Default::default()).is_contained()
    }
}

impl FieldSelection {
    pub(crate) fn containment(
        &self,
        other: &FieldSelection,
        options: ContainmentOptions,
    ) -> Containment {
        if self.field.name() != other.field.name()
            || self.field.alias != other.field.alias
            || self.field.arguments != other.field.arguments
            || self.field.directives != other.field.directives
        {
            return Containment::NotContained;
        }

        match (&self.selection_set, &other.selection_set) {
            (None, None) => Containment::Equal,
            (Some(self_selection), Some(other_selection)) => {
                self_selection.containment(other_selection, options)
            }
            (None, Some(_)) | (Some(_), None) => {
                debug_assert!(false, "field selections have the same element, so if one does not have a subselection, neither should the other one");
                Containment::NotContained
            }
        }
    }
}

impl FragmentSpreadSelection {
    pub(crate) fn containment(
        &self,
        other: &Selection,
        options: ContainmentOptions,
    ) -> Containment {
        match other {
            // Using keys here means that @defer fragments never compare equal.
            // This is a bit odd but it is consistent: the selection set data structure would not
            // even try to compare two @defer fragments, because their keys are different.
            Selection::FragmentSpread(other) if self.spread.key() == other.spread.key() => self
                .selection_set
                .containment(&other.selection_set, options),
            _ => Containment::NotContained,
        }
    }
}

impl InlineFragmentSelection {
    pub(crate) fn containment(
        &self,
        other: &Selection,
        options: ContainmentOptions,
    ) -> Containment {
        match other {
            // Using keys here means that @defer fragments never compare equal.
            // This is a bit odd but it is consistent: the selection set data structure would not
            // even try to compare two @defer fragments, because their keys are different.
            Selection::InlineFragment(other)
                if self.inline_fragment.key() == other.inline_fragment.key() =>
            {
                self.selection_set
                    .containment(&other.selection_set, options)
            }
            _ => Containment::NotContained,
        }
    }
}

impl SelectionSet {
    pub(crate) fn containment(&self, other: &Self, options: ContainmentOptions) -> Containment {
        if other.selections.len() > self.selections.len() {
            // If `other` has more selections but we're ignoring missing __typename, then in the case where
            // `other` has a __typename but `self` does not, then we need the length of `other` to be at
            // least 2 more than other of `self` to be able to conclude there is no contains.
            if !options.ignore_missing_typename
                || other.selections.len() > self.selections.len() + 1
                || self.has_top_level_typename_field()
                || !other.has_top_level_typename_field()
            {
                return Containment::NotContained;
            }
        }

        let mut is_equal = true;
        let mut did_ignore_typename = false;

        for (key, other_selection) in other.selections.iter() {
            if key.is_typename_field() && options.ignore_missing_typename {
                if !self.has_top_level_typename_field() {
                    did_ignore_typename = true;
                }
                continue;
            }

            let Some(self_selection) = self.selections.get(&key) else {
                return Containment::NotContained;
            };

            match self_selection.containment(other_selection, options) {
                Containment::NotContained => return Containment::NotContained,
                Containment::StrictlyContained if is_equal => is_equal = false,
                Containment::StrictlyContained | Containment::Equal => {}
            }
        }

        let expected_len = if did_ignore_typename {
            self.selections.len() + 1
        } else {
            self.selections.len()
        };

        if is_equal && other.selections.len() == expected_len {
            Containment::Equal
        } else {
            Containment::StrictlyContained
        }
    }

    /// Returns true if this selection is a superset of the other selection.
    pub(crate) fn contains(&self, other: &Self) -> bool {
        self.containment(other, Default::default()).is_contained()
    }
}

#[cfg(test)]
mod tests {
    use super::Containment;
    use super::ContainmentOptions;
    use crate::operation::Operation;
    use crate::schema::ValidFederationSchema;

    fn containment_custom(left: &str, right: &str, ignore_missing_typename: bool) -> Containment {
        let schema = apollo_compiler::Schema::parse_and_validate(
            r#"
        directive @defer(label: String, if: Boolean! = true) on FRAGMENT_SPREAD | INLINE_FRAGMENT

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
        input Input {
            recur: Input
            f: Boolean
            g: Boolean
            h: Boolean
        }
        type Query {
            a: Int
            b: Int
            c: Int
            object: Nested
            intf: Intf
            arg(a: Int, b: Int, c: Int, d: Input): Int
        }
        "#,
            "schema.graphql",
        )
        .unwrap();
        let schema = ValidFederationSchema::new(schema).unwrap();
        let left = Operation::parse(schema.clone(), left, "left.graphql", None).unwrap();
        let right = Operation::parse(schema.clone(), right, "right.graphql", None).unwrap();

        left.selection_set.containment(
            &right.selection_set,
            ContainmentOptions {
                ignore_missing_typename,
            },
        )
    }

    fn containment(left: &str, right: &str) -> Containment {
        containment_custom(left, right, false)
    }

    #[test]
    fn selection_set_contains() {
        assert_eq!(containment("{ a }", "{ a }"), Containment::Equal);
        assert_eq!(containment("{ a b }", "{ b a }"), Containment::Equal);
        assert_eq!(
            containment("{ arg(a: 1) }", "{ arg(a: 2) }"),
            Containment::NotContained
        );
        assert_eq!(
            containment("{ arg(a: 1) }", "{ arg(b: 1) }"),
            Containment::NotContained
        );
        assert_eq!(
            containment("{ arg(a: 1) }", "{ arg(a: 1) }"),
            Containment::Equal
        );
        assert_eq!(
            containment("{ arg(a: 1, b: 1) }", "{ arg(b: 1 a: 1) }"),
            Containment::Equal
        );
        assert_eq!(
            containment("{ arg(a: 1) }", "{ arg(a: 1) }"),
            Containment::Equal
        );
        assert_eq!(
            containment(
                "{ arg(d: { f: true, g: true }) }",
                "{ arg(d: { f: true }) }"
            ),
            Containment::NotContained
        );
        assert_eq!(
            containment(
                "{ arg(d: { recur: { f: true } g: true h: false }) }",
                "{ arg(d: { h: false recur: {f: true} g: true }) }"
            ),
            Containment::Equal
        );
        assert_eq!(
            containment("{ arg @skip(if: true) }", "{ arg @skip(if: true) }"),
            Containment::Equal
        );
        assert_eq!(
            containment("{ arg @skip(if: true) }", "{ arg @skip(if: false) }"),
            Containment::NotContained
        );
        assert_eq!(
            containment("{ ... @defer { arg } }", "{ ... @defer { arg } }"),
            Containment::NotContained,
            "@defer selections never contain each other"
        );
        assert_eq!(
            containment("{ a b c }", "{ b a }"),
            Containment::StrictlyContained
        );
        assert_eq!(
            containment("{ a b }", "{ b c a }"),
            Containment::NotContained
        );
        assert_eq!(containment("{ a }", "{ b }"), Containment::NotContained);
        assert_eq!(
            containment("{ object { a } }", "{ object { b a } }"),
            Containment::NotContained
        );

        assert_eq!(
            containment("{ ... { a } }", "{ ... { a } }"),
            Containment::Equal
        );
        assert_eq!(
            containment(
                "{ intf { ... on HasA { a } } }",
                "{ intf { ... on HasA { a } } }",
            ),
            Containment::Equal
        );
        // These select the same things, but containment also counts fragment namedness
        assert_eq!(
            containment(
                "{ intf { ... on HasA { a } } }",
                "{ intf { ...named } } fragment named on HasA { a }",
            ),
            Containment::NotContained
        );
        assert_eq!(
            containment(
                "{ intf { ...named } } fragment named on HasA { a intfField }",
                "{ intf { ...named } } fragment named on HasA { a }",
            ),
            Containment::StrictlyContained
        );
        assert_eq!(
            containment(
                "{ intf { ...named } } fragment named on HasA { a }",
                "{ intf { ...named } } fragment named on HasA { a intfField }",
            ),
            Containment::NotContained
        );
    }

    #[test]
    fn selection_set_contains_missing_typename() {
        assert_eq!(
            containment_custom("{ a }", "{ a __typename }", true),
            Containment::Equal
        );
        assert_eq!(
            containment_custom("{ a b }", "{ b a __typename }", true),
            Containment::Equal
        );
        assert_eq!(
            containment_custom("{ a b }", "{ b __typename }", true),
            Containment::StrictlyContained
        );
        assert_eq!(
            containment_custom("{ object { a b } }", "{ object { b __typename } }", true),
            Containment::StrictlyContained
        );
        assert_eq!(
            containment_custom(
                "{ intf { intfField __typename } }",
                "{ intf { intfField } }",
                true
            ),
            Containment::StrictlyContained,
        );
        assert_eq!(
            containment_custom(
                "{ intf { intfField __typename } }",
                "{ intf { intfField __typename } }",
                true
            ),
            Containment::Equal,
        );
    }
}
