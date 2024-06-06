use std::collections::HashMap;

use apollo_compiler::executable;
use apollo_compiler::Node;

use super::FieldSelection;
use super::FragmentSpreadSelection;
use super::HasSelectionKey;
use super::InlineFragmentSelection;
use super::Selection;
use super::SelectionSet;

/// Compare two input values, with two special cases for objects: assuming no duplicate keys,
/// and order-independence.
///
/// This comes from apollo-rs: https://github.com/apollographql/apollo-rs/blob/6825be88fe13cd0d67b83b0e4eb6e03c8ab2555e/crates/apollo-compiler/src/validation/selection.rs#L160-L188
/// Hopefully we can do this more easily in the future!
fn same_value(left: &executable::Value, right: &executable::Value) -> bool {
    use apollo_compiler::executable::Value;
    match (left, right) {
        (Value::Null, Value::Null) => true,
        (Value::Enum(left), Value::Enum(right)) => left == right,
        (Value::Variable(left), Value::Variable(right)) => left == right,
        (Value::String(left), Value::String(right)) => left == right,
        (Value::Float(left), Value::Float(right)) => left == right,
        (Value::Int(left), Value::Int(right)) => left == right,
        (Value::Boolean(left), Value::Boolean(right)) => left == right,
        (Value::List(left), Value::List(right)) => left
            .iter()
            .zip(right.iter())
            .all(|(left, right)| same_value(left, right)),
        (Value::Object(left), Value::Object(right)) if left.len() == right.len() => {
            left.iter().all(|(key, value)| {
                right
                    .iter()
                    .find(|(other_key, _)| key == other_key)
                    .is_some_and(|(_, other_value)| same_value(value, other_value))
            })
        }
        _ => false,
    }
}

/// Returns true if two argument lists are equivalent.
///
/// The arguments and values must be the same, independent of order.
fn same_arguments(
    left: &[Node<executable::Argument>],
    right: &[Node<executable::Argument>],
) -> bool {
    if left.len() != right.len() {
        return false;
    }

    let right = right
        .iter()
        .map(|arg| (&arg.name, arg))
        .collect::<HashMap<_, _>>();

    left.iter().all(|arg| {
        right
            .get(&arg.name)
            .is_some_and(|right_arg| same_value(&arg.value, &right_arg.value))
    })
}

/// Returns true if two directive lists are equivalent.
fn same_directives(left: &executable::DirectiveList, right: &executable::DirectiveList) -> bool {
    if left.len() != right.len() {
        return false;
    }

    left.iter().all(|left_directive| {
        right.iter().any(|right_directive| {
            left_directive.name == right_directive.name
                && same_arguments(&left_directive.arguments, &right_directive.arguments)
        })
    })
}

/// Options for the `.containment()` family of selection functions.
#[derive(Debug, Clone, Copy)]
pub struct ContainmentOptions {
    /// If the right-hand side has a __typename selection but the left-hand side does not,
    /// still consider the left-hand side to contain the right-hand side.
    pub ignore_missing_typename: bool,
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
pub enum Containment {
    /// The left-hand selection does not fully contain right-hand selection.
    NotContained,
    /// The left-hand selection fully contains the right-hand selection, and more.
    StrictlyContained,
    /// Two selections are equal.
    Equal,
}
impl Containment {
    /// Returns true if the right-hand selection set is strictly contained or equal.
    pub fn is_contained(self) -> bool {
        matches!(self, Containment::StrictlyContained | Containment::Equal)
    }
}

impl Selection {
    pub fn containment(&self, other: &Selection, options: ContainmentOptions) -> Containment {
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
    pub fn contains(&self, other: &Selection) -> bool {
        self.containment(other, Default::default()).is_contained()
    }
}

impl FieldSelection {
    pub fn containment(&self, other: &FieldSelection, options: ContainmentOptions) -> Containment {
        let self_field = self.field.data();
        let other_field = other.field.data();
        if self_field.name() != other_field.name()
            || self_field.alias != other_field.alias
            || !same_arguments(&self_field.arguments, &other_field.arguments)
            || !same_directives(&self_field.directives, &other_field.directives)
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

    /// Returns true if this selection is a superset of the other selection.
    pub fn contains(&self, other: &FieldSelection) -> bool {
        self.containment(other, Default::default()).is_contained()
    }
}

impl FragmentSpreadSelection {
    pub fn containment(&self, other: &Selection, options: ContainmentOptions) -> Containment {
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

    /// Returns true if this selection is a superset of the other selection.
    pub fn contains(&self, other: &Selection) -> bool {
        self.containment(other, Default::default()).is_contained()
    }
}

impl InlineFragmentSelection {
    pub fn containment(&self, other: &Selection, options: ContainmentOptions) -> Containment {
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

    /// Returns true if this selection is a superset of the other selection.
    pub fn contains(&self, other: &Selection) -> bool {
        self.containment(other, Default::default()).is_contained()
    }
}

impl SelectionSet {
    pub fn containment(&self, other: &Self, options: ContainmentOptions) -> Containment {
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

            let Some(self_selection) = self.selections.get(key) else {
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
    pub fn contains(&self, other: &Self) -> bool {
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
