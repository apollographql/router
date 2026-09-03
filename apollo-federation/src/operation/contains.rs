use apollo_compiler::executable;

use super::FieldSelection;
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
            (Selection::InlineFragment(self_fragment), Selection::InlineFragment(_)) => {
                self_fragment.containment(other, options)
            }
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
                debug_assert!(
                    false,
                    "field selections have the same element, so if one does not have a subselection, neither should the other one"
                );
                Containment::NotContained
            }
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

        for other_selection in other.selections.values() {
            if other_selection.is_typename_field() && options.ignore_missing_typename {
                if !self.has_top_level_typename_field() {
                    did_ignore_typename = true;
                }
                continue;
            }

            let Some(self_selection) = self.selections.get(other_selection.key()) else {
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
        let left = Operation::parse(schema.clone(), left, "left.graphql")
            .expect("operation is valid and can be parsed");
        let right = Operation::parse(schema, right, "right.graphql")
            .expect("operation is valid and can be parsed");

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

    // =========================================================================================
    // Selection merge/containment semilattice (proptest-graph-notes.md, "Supporting operation
    // properties"): a small, schema-aware selection generator plus an independent recursive
    // model, checking `SelectionSet::containment`/`contains` and `merge_selection_sets` against
    // the algebraic laws of a join-semilattice (merge = least upper bound of containment).
    // Directive-free and fragment-spread-free for this first pass, per the notes ("Add
    // @skip/@include only after the base lattice laws are confirmed").
    // =========================================================================================

    use std::collections::BTreeMap;
    use std::hash::Hash;
    use std::hash::Hasher;

    use proptest::prelude::*;
    use proptest::proptest;

    use super::SelectionSet;
    use crate::operation::merging::merge_selection_sets;

    fn selection_lattice_schema() -> ValidFederationSchema {
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
        ValidFederationSchema::new(schema).unwrap()
    }

    /// A schema-independent, order-independent model of a selection set: no aliases, no
    /// directives, no fragment spreads (only inline fragments with an explicit type condition).
    #[derive(Debug, Clone, PartialEq, Eq)]
    enum ModelSelection {
        /// A scalar field with no sub-selection (`a`, `b`, `c`, `intfField`).
        Leaf,
        /// A composite field (`object`, `intf`).
        Field(ModelSelectionSet),
        /// An inline fragment with a type condition (`... on HasA { ... }`).
        Fragment(&'static str, ModelSelectionSet),
    }
    type ModelSelectionSet = BTreeMap<String, ModelSelection>;

    /// Generate a non-empty subset of `{a, b, c}` under `Nested`.
    fn lattice_nested_selection_strategy() -> impl Strategy<Value = ModelSelectionSet> {
        any::<(bool, bool, bool)>().prop_map(|(mut a, b, c)| {
            a |= !(b || c);
            let mut set = BTreeMap::new();
            if a {
                set.insert("a".to_string(), ModelSelection::Leaf);
            }
            if b {
                set.insert("b".to_string(), ModelSelection::Leaf);
            }
            if c {
                set.insert("c".to_string(), ModelSelection::Leaf);
            }
            set
        })
    }

    /// Generate a selection under `Intf`: `intfField` directly, an inline fragment on `HasA`
    /// selecting some of `{a, intfField}`, or both — always non-empty.
    fn lattice_intf_selection_strategy() -> impl Strategy<Value = ModelSelectionSet> {
        any::<(bool, bool, bool, bool)>().prop_map(
            |(mut has_intf_field, has_fragment, mut frag_a, frag_intf_field)| {
                frag_a |= !(frag_a || frag_intf_field);
                has_intf_field |= !(has_intf_field || has_fragment);
                let mut set = BTreeMap::new();
                if has_intf_field {
                    set.insert("intfField".to_string(), ModelSelection::Leaf);
                }
                if has_fragment {
                    let mut frag = BTreeMap::new();
                    if frag_a {
                        frag.insert("a".to_string(), ModelSelection::Leaf);
                    }
                    if frag_intf_field {
                        frag.insert("intfField".to_string(), ModelSelection::Leaf);
                    }
                    set.insert(
                        "...HasA".to_string(),
                        ModelSelection::Fragment("HasA", frag),
                    );
                }
                set
            },
        )
    }

    /// Generate a full top-level selection set under `Query`: any non-empty combination of the
    /// scalar fields `a`/`b`/`c` and the composite fields `object`/`intf`.
    fn lattice_selection_set_strategy() -> impl Strategy<Value = ModelSelectionSet> {
        (
            any::<(bool, bool, bool)>(),
            prop::option::of(lattice_nested_selection_strategy()),
            prop::option::of(lattice_intf_selection_strategy()),
        )
            .prop_map(|((mut a, b, c), object, intf)| {
                a |= !(a || b || c || object.is_some() || intf.is_some());
                let mut set = BTreeMap::new();
                if a {
                    set.insert("a".to_string(), ModelSelection::Leaf);
                }
                if b {
                    set.insert("b".to_string(), ModelSelection::Leaf);
                }
                if c {
                    set.insert("c".to_string(), ModelSelection::Leaf);
                }
                if let Some(object) = object {
                    set.insert("object".to_string(), ModelSelection::Field(object));
                }
                if let Some(intf) = intf {
                    set.insert("intf".to_string(), ModelSelection::Field(intf));
                }
                set
            })
    }

    /// Print a model selection set to GraphQL source text. `seed` deterministically permutes
    /// entry order at every level, independent of the model's own `BTreeMap` order, so two
    /// prints of the same model with different seeds exercise insertion-order independence.
    fn lattice_print(set: &ModelSelectionSet, seed: u64) -> String {
        let mut entries: Vec<(&String, &ModelSelection)> = set.iter().collect();
        entries.sort_by_key(|(key, _)| {
            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            (seed, *key).hash(&mut hasher);
            hasher.finish()
        });
        entries
            .into_iter()
            .map(|(key, selection)| match selection {
                ModelSelection::Leaf => key.clone(),
                ModelSelection::Field(sub) => format!(
                    "{key} {{ {} }}",
                    lattice_print(sub, seed.wrapping_mul(31).wrapping_add(1))
                ),
                ModelSelection::Fragment(ty, sub) => format!(
                    "... on {ty} {{ {} }}",
                    lattice_print(sub, seed.wrapping_mul(31).wrapping_add(2))
                ),
            })
            .collect::<Vec<_>>()
            .join(" ")
    }

    fn lattice_parse(
        schema: &ValidFederationSchema,
        set: &ModelSelectionSet,
        seed: u64,
    ) -> SelectionSet {
        let source = format!("{{ {} }}", lattice_print(set, seed));
        Operation::parse(schema.clone(), &source, "lattice.graphql")
            .expect("generated selection set must be valid")
            .selection_set
    }

    /// `container` structurally contains `containee`: every key `containee` has, `container` has
    /// too, with a recursively-containing selection.
    fn lattice_contains(container: &ModelSelectionSet, containee: &ModelSelectionSet) -> bool {
        containee.iter().all(|(key, containee_selection)| {
            container.get(key).is_some_and(|container_selection| {
                lattice_selection_contains(container_selection, containee_selection)
            })
        })
    }

    fn lattice_selection_contains(a: &ModelSelection, b: &ModelSelection) -> bool {
        match (a, b) {
            (ModelSelection::Leaf, ModelSelection::Leaf) => true,
            (ModelSelection::Field(a_set), ModelSelection::Field(b_set)) => {
                lattice_contains(a_set, b_set)
            }
            (ModelSelection::Fragment(a_ty, a_set), ModelSelection::Fragment(b_ty, b_set)) => {
                a_ty == b_ty && lattice_contains(a_set, b_set)
            }
            _ => false,
        }
    }

    fn lattice_merge(a: &ModelSelectionSet, b: &ModelSelectionSet) -> ModelSelectionSet {
        let mut result = a.clone();
        for (key, b_selection) in b {
            match result.get(key) {
                None => {
                    result.insert(key.clone(), b_selection.clone());
                }
                Some(a_selection) => {
                    let merged = lattice_selection_merge(a_selection, b_selection);
                    result.insert(key.clone(), merged);
                }
            }
        }
        result
    }

    fn lattice_selection_merge(a: &ModelSelection, b: &ModelSelection) -> ModelSelection {
        match (a, b) {
            (ModelSelection::Field(a_set), ModelSelection::Field(b_set)) => {
                ModelSelection::Field(lattice_merge(a_set, b_set))
            }
            (ModelSelection::Fragment(ty, a_set), ModelSelection::Fragment(_, b_set)) => {
                ModelSelection::Fragment(ty, lattice_merge(a_set, b_set))
            }
            _ => a.clone(),
        }
    }

    /// Remove entries from `set` according to `feed` (`true` = keep), recursing into
    /// composite/fragment sub-selections. Never leaves a composite sub-selection empty (that
    /// would produce invalid, empty-braces GraphQL) — falls back to the original sub-selection
    /// instead — so the result is always valid and non-empty, and by construction
    /// `lattice_contains(set, &result)` holds. Running out of bits defaults to "keep".
    fn lattice_shrink(
        set: &ModelSelectionSet,
        feed: &mut std::vec::IntoIter<bool>,
    ) -> ModelSelectionSet {
        let mut result = BTreeMap::new();
        for (key, selection) in set {
            if !feed.next().unwrap_or(true) {
                continue;
            }
            let shrunk = match selection {
                ModelSelection::Leaf => ModelSelection::Leaf,
                ModelSelection::Field(sub) => {
                    let shrunk_sub = lattice_shrink(sub, feed);
                    ModelSelection::Field(if shrunk_sub.is_empty() {
                        sub.clone()
                    } else {
                        shrunk_sub
                    })
                }
                ModelSelection::Fragment(ty, sub) => {
                    let shrunk_sub = lattice_shrink(sub, feed);
                    ModelSelection::Fragment(
                        ty,
                        if shrunk_sub.is_empty() {
                            sub.clone()
                        } else {
                            shrunk_sub
                        },
                    )
                }
            };
            result.insert(key.clone(), shrunk);
        }
        if result.is_empty() {
            set.clone()
        } else {
            result
        }
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(256))]

        /// `SelectionSet::contains`/`containment` must exactly match the naive structural model,
        /// for two independently generated selection sets (printed with independent orderings,
        /// so this also exercises order-independence).
        #[test]
        fn selection_set_containment_matches_naive_model(
            a in lattice_selection_set_strategy(),
            b in lattice_selection_set_strategy(),
            seed_a in any::<u64>(),
            seed_b in any::<u64>(),
        ) {
            let schema = selection_lattice_schema();
            let production_a = lattice_parse(&schema, &a, seed_a);
            let production_b = lattice_parse(&schema, &b, seed_b);

            prop_assert_eq!(production_a.contains(&production_b), lattice_contains(&a, &b));
            prop_assert_eq!(production_b.contains(&production_a), lattice_contains(&b, &a));

            let expected_equal = lattice_contains(&a, &b) && lattice_contains(&b, &a);
            prop_assert_eq!(
                production_a
                    .containment(&production_b, ContainmentOptions::default())
                    .is_equal(),
                expected_equal
            );
        }

        /// Containment is reflexive (including across a re-printed, differently-ordered copy of
        /// the same selection) and transitive: shrinking a selection twice produces a chain
        /// `a ⊇ b ⊇ c`, and production containment must agree at every link, including `a ⊇ c`.
        #[test]
        fn selection_set_containment_is_reflexive_and_transitive(
            a in lattice_selection_set_strategy(),
            seed_a1 in any::<u64>(),
            seed_a2 in any::<u64>(),
            seed_b in any::<u64>(),
            seed_c in any::<u64>(),
            shrink_bits_1 in prop::collection::vec(any::<bool>(), 24),
            shrink_bits_2 in prop::collection::vec(any::<bool>(), 24),
        ) {
            let schema = selection_lattice_schema();
            let production_a1 = lattice_parse(&schema, &a, seed_a1);
            let production_a2 = lattice_parse(&schema, &a, seed_a2);
            prop_assert!(
                production_a1
                    .containment(&production_a2, ContainmentOptions::default())
                    .is_equal(),
                "reflexive/order-independent equality failed"
            );

            let b = lattice_shrink(&a, &mut shrink_bits_1.into_iter());
            let c = lattice_shrink(&b, &mut shrink_bits_2.into_iter());
            prop_assert!(lattice_contains(&a, &b));
            prop_assert!(lattice_contains(&b, &c));
            prop_assert!(lattice_contains(&a, &c));

            let production_b = lattice_parse(&schema, &b, seed_b);
            let production_c = lattice_parse(&schema, &c, seed_c);
            prop_assert!(production_a1.contains(&production_b), "a must contain its shrink b");
            prop_assert!(production_b.contains(&production_c), "b must contain its shrink c");
            prop_assert!(
                production_a1.contains(&production_c),
                "containment must be transitive: a must contain c"
            );
        }

        /// The named broad property: `merge_selection_sets` is the least upper bound of
        /// containment. Checked against the naive model, plus the standalone algebraic laws
        /// (contains each operand, commutative, associative, idempotent, upper-bound-for-any-
        /// superset) directly on production values.
        #[test]
        fn selection_merge_is_least_upper_bound_of_containment(
            a in lattice_selection_set_strategy(),
            b in lattice_selection_set_strategy(),
            c in lattice_selection_set_strategy(),
            seed_a in any::<u64>(),
            seed_b in any::<u64>(),
            seed_c in any::<u64>(),
            seed_merge_model in any::<u64>(),
        ) {
            let schema = selection_lattice_schema();
            let production_a = lattice_parse(&schema, &a, seed_a);
            let production_b = lattice_parse(&schema, &b, seed_b);
            let production_c = lattice_parse(&schema, &c, seed_c);

            // Oracle: production merge(a, b) must be semantically equal to the naive model's
            // union, independently re-parsed.
            let merge_ab =
                merge_selection_sets(vec![production_a.clone(), production_b.clone()]).unwrap();
            let model_merge_ab = lattice_merge(&a, &b);
            let production_model_merge_ab = lattice_parse(&schema, &model_merge_ab, seed_merge_model);
            prop_assert!(
                merge_ab
                    .containment(&production_model_merge_ab, ContainmentOptions::default())
                    .is_equal(),
                "merge(a, b) did not match the naive union model"
            );

            // Merge contains each operand.
            prop_assert!(merge_ab.contains(&production_a));
            prop_assert!(merge_ab.contains(&production_b));

            // Commutative.
            let merge_ba =
                merge_selection_sets(vec![production_b.clone(), production_a.clone()]).unwrap();
            prop_assert!(
                merge_ab
                    .containment(&merge_ba, ContainmentOptions::default())
                    .is_equal(),
                "merge is not commutative"
            );

            // Associative.
            let merge_ab_c =
                merge_selection_sets(vec![merge_ab.clone(), production_c.clone()]).unwrap();
            let merge_bc =
                merge_selection_sets(vec![production_b.clone(), production_c.clone()]).unwrap();
            let merge_a_bc = merge_selection_sets(vec![production_a.clone(), merge_bc]).unwrap();
            prop_assert!(
                merge_ab_c
                    .containment(&merge_a_bc, ContainmentOptions::default())
                    .is_equal(),
                "merge is not associative"
            );

            // Idempotent.
            let merge_aa =
                merge_selection_sets(vec![production_a.clone(), production_a.clone()]).unwrap();
            prop_assert!(
                merge_aa
                    .containment(&production_a, ContainmentOptions::default())
                    .is_equal(),
                "merge is not idempotent"
            );

            // If C contains A and B, it contains merge(A, B). `merge_ab_c` is a real superset of
            // both a and b (merging in c can only add more), so it must contain their merge too.
            prop_assert!(merge_ab_c.contains(&production_a));
            prop_assert!(merge_ab_c.contains(&production_b));
            prop_assert!(
                merge_ab_c.contains(&merge_ab),
                "a superset of a and b must contain their merge"
            );
        }
    }
}
