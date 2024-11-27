//! # GraphQL subgraph query optimization.
//!
//! This module contains the logic to optimize (or "compress") a subgraph query by using fragments
//! (either reusing existing ones in the original query or generating new ones).
//!
//! ## Add __typename field for abstract types in named fragment definitions
//!
//! ## Selection/SelectionSet intersection/minus operations
//! These set-theoretic operation methods are used to compute the optimized selection set.
//!
//! ## Collect applicable fragments at given type.
//! This is only the first filtering step. Further validation is needed to check if they can merge
//! with other fields and fragment selections.
//!
//! ## Field validation
//! `FieldsConflictMultiBranchValidator` (and `FieldsConflictValidator`) are used to check if
//! modified subgraph GraphQL queries are still valid, since adding fragments can introduce
//! conflicts.
//!
//! ## Matching fragments with selection set
//! `try_apply_fragments` tries to match all applicable fragments one by one.
//! They are expanded into selection sets in order to match against given selection set.
//! Set-intersection/-minus/-containment operations are used to narrow down to fewer number of
//! fragments that can be used to optimize the selection set. If there is a single fragment that
//! covers the full selection set, then that fragment is used. Otherwise, we attempted to reduce
//! the number of fragments applied (but optimality is not guaranteed yet).
//!
//! ## Retain certain fragments in selection sets while expanding the rest
//! Unlike the `expand_all_fragments` method, this methods retains the listed fragments.
//!
//! ## Optimize (or reduce) the named fragments in the query
//! Optimization of named fragment definitions in query documents based on the usage of
//! fragments in (optimized) operations.
//!
//! ## `reuse_fragments` methods (putting everything together)
//! Recursive optimization of selection and selection sets.

use std::sync::Arc;

use apollo_compiler::collections::IndexMap;
use apollo_compiler::executable;
use apollo_compiler::Name;
use apollo_compiler::Node;

use super::Fragment;
use super::FragmentSpreadSelection;
use super::HasSelectionKey;
use super::InlineFragmentSelection;
use super::NamedFragments;
use super::Operation;
use super::Selection;
use super::SelectionMapperReturn;
use super::SelectionOrSet;
use super::SelectionSet;
use crate::error::FederationError;
use crate::operation::FragmentSpread;
use crate::operation::SelectionValue;

//=============================================================================
// Selection/SelectionSet intersection/minus operations

impl Selection {
    // PORT_NOTE: The definition of `minus` and `intersection` functions when either `self` or
    // `other` has no sub-selection seems unintuitive. Why `apple.minus(orange) = None` and
    // `apple.intersection(orange) = apple`?

    /// Computes the set-subtraction (self - other) and returns the result (the difference between
    /// self and other).
    /// If there are respective sub-selections, then we compute their diffs and add them (if not
    /// empty). Otherwise, we have no diff.
    fn minus(&self, other: &Selection) -> Result<Option<Selection>, FederationError> {
        if let (Some(self_sub_selection), Some(other_sub_selection)) =
            (self.selection_set(), other.selection_set())
        {
            let diff = self_sub_selection.minus(other_sub_selection)?;
            if !diff.is_empty() {
                return self
                    .with_updated_selections(self_sub_selection.type_position.clone(), diff)
                    .map(Some);
            }
        }
        Ok(None)
    }

    /// Computes the set-intersection of self and other
    /// - If there are respective sub-selections, then we compute their intersections and add them
    ///   (if not empty).
    /// - Otherwise, the intersection is same as `self`.
    fn intersection(&self, other: &Selection) -> Result<Option<Selection>, FederationError> {
        if let (Some(self_sub_selection), Some(other_sub_selection)) =
            (self.selection_set(), other.selection_set())
        {
            let common = self_sub_selection.intersection(other_sub_selection)?;
            if common.is_empty() {
                return Ok(None);
            } else {
                return self
                    .with_updated_selections(self_sub_selection.type_position.clone(), common)
                    .map(Some);
            }
        }
        Ok(Some(self.clone()))
    }
}

impl SelectionSet {
    /// Performs set-subtraction (self - other) and returns the result (the difference between self
    /// and other).
    pub(crate) fn minus(&self, other: &SelectionSet) -> Result<SelectionSet, FederationError> {
        let iter = self
            .selections
            .values()
            .map(|v| {
                if let Some(other_v) = other.selections.get(v.key()) {
                    v.minus(other_v)
                } else {
                    Ok(Some(v.clone()))
                }
            })
            .collect::<Result<Vec<_>, _>>()? // early break in case of Err
            .into_iter()
            .flatten();
        Ok(SelectionSet::from_raw_selections(
            self.schema.clone(),
            self.type_position.clone(),
            iter,
        ))
    }

    /// Computes the set-intersection of self and other
    fn intersection(&self, other: &SelectionSet) -> Result<SelectionSet, FederationError> {
        if self.is_empty() {
            return Ok(self.clone());
        }
        if other.is_empty() {
            return Ok(other.clone());
        }

        let iter = self
            .selections
            .values()
            .map(|v| {
                if let Some(other_v) = other.selections.get(v.key()) {
                    v.intersection(other_v)
                } else {
                    Ok(None)
                }
            })
            .collect::<Result<Vec<_>, _>>()? // early break in case of Err
            .into_iter()
            .flatten();
        Ok(SelectionSet::from_raw_selections(
            self.schema.clone(),
            self.type_position.clone(),
            iter,
        ))
    }
}

//=============================================================================
// Matching fragments with selection set (`try_optimize_with_fragments`)

/// The return type for `SelectionSet::try_optimize_with_fragments`.
#[derive(derive_more::From)]
enum SelectionSetOrFragment {
    SelectionSet(SelectionSet),
    Fragment(Node<Fragment>),
}

// Note: `retain_fragments` methods may return a selection or a selection set.
impl From<SelectionOrSet> for SelectionMapperReturn {
    fn from(value: SelectionOrSet) -> Self {
        match value {
            SelectionOrSet::Selection(selection) => selection.into(),
            SelectionOrSet::SelectionSet(selections) => {
                // The items in a selection set needs to be cloned here, since it's sub-selections
                // are contained in an `Arc`.
                Vec::from_iter(selections.selections.values().cloned()).into()
            }
        }
    }
}

//=============================================================================
// `reuse_fragments` methods (putting everything together)

/// Return type for `InlineFragmentSelection::reuse_fragments`.
#[derive(derive_more::From)]
enum FragmentSelection {
    // Note: Enum variants are named to match those of `Selection`.
    InlineFragment(InlineFragmentSelection),
    FragmentSpread(FragmentSpreadSelection),
}

impl From<FragmentSelection> for Selection {
    fn from(value: FragmentSelection) -> Self {
        match value {
            FragmentSelection::InlineFragment(inline_fragment) => inline_fragment.into(),
            FragmentSelection::FragmentSpread(fragment_spread) => fragment_spread.into(),
        }
    }
}

impl Operation {
    /// Optimize the parsed size of the operation by generating fragments based on the selections
    /// in the operation.
    pub(crate) fn generate_fragments(&mut self) -> Result<(), FederationError> {
        // Currently, this method simply pulls out every inline fragment into a named fragment. If
        // multiple inline fragments are the same, they use the same named fragment.
        //
        // This method can generate named fragments that are only used once. It's not ideal, but it
        // also doesn't seem that bad. Avoiding this is possible but more work, and keeping this
        // as simple as possible is a big benefit for now.
        //
        // When we have more advanced correctness testing, we can add more features to fragment
        // generation, like factoring out partial repeated slices of selection sets or only
        // introducing named fragments for patterns that occur more than once.
        let mut generator = FragmentGenerator::default();
        generator.visit_selection_set(&mut self.selection_set)?;
        self.named_fragments = generator.into_inner();
        Ok(())
    }

    // PORT_NOTE: This mirrors the JS version's `Operation.expandAllFragments`. But this method is
    // mainly for unit tests. The actual port of `expandAllFragments` is in `normalize_operation`.
    #[cfg(test)]
    fn expand_all_fragments_and_normalize(&self) -> Result<Self, FederationError> {
        let selection_set = self
            .selection_set
            .expand_all_fragments()?
            .flatten_unnecessary_fragments(
                &self.selection_set.type_position,
                &self.named_fragments,
                &self.schema,
            )?;
        Ok(Self {
            named_fragments: Default::default(),
            selection_set,
            ..self.clone()
        })
    }
}

#[derive(Debug, Default)]
struct FragmentGenerator {
    fragments: NamedFragments,
    // XXX(@goto-bus-stop): This is temporary to support mismatch testing with JS!
    names: IndexMap<(String, usize), usize>,
}

impl FragmentGenerator {
    // XXX(@goto-bus-stop): This is temporary to support mismatch testing with JS!
    // In the future, we will just use `.next_name()`.
    fn generate_name(&mut self, frag: &InlineFragmentSelection) -> Name {
        use std::fmt::Write as _;

        let type_condition = frag
            .inline_fragment
            .type_condition_position
            .as_ref()
            .map_or_else(
                || "undefined".to_string(),
                |condition| condition.to_string(),
            );
        let selections = frag.selection_set.selections.len();
        let mut name = format!("_generated_on{type_condition}{selections}");

        let key = (type_condition, selections);
        let index = self
            .names
            .entry(key)
            .and_modify(|index| *index += 1)
            .or_default();
        _ = write!(&mut name, "_{index}");

        Name::new_unchecked(&name)
    }

    /// Is a selection set worth using for a newly generated named fragment?
    fn is_worth_using(selection_set: &SelectionSet) -> bool {
        let mut iter = selection_set.iter();
        let Some(first) = iter.next() else {
            // An empty selection is not worth using (and invalid!)
            return false;
        };
        let Selection::Field(field) = first else {
            return true;
        };
        // If there's more than one selection, or one selection with a subselection,
        // it's probably worth using
        iter.next().is_some() || field.selection_set.is_some()
    }

    /// Modify the selection set so that eligible inline fragments are moved to named fragment spreads.
    fn visit_selection_set(
        &mut self,
        selection_set: &mut SelectionSet,
    ) -> Result<(), FederationError> {
        let mut new_selection_set = SelectionSet::empty(
            selection_set.schema.clone(),
            selection_set.type_position.clone(),
        );

        for selection in Arc::make_mut(&mut selection_set.selections).values_mut() {
            match selection {
                SelectionValue::Field(mut field) => {
                    if let Some(selection_set) = field.get_selection_set_mut() {
                        self.visit_selection_set(selection_set)?;
                    }
                    new_selection_set
                        .add_local_selection(&Selection::Field(Arc::clone(field.get())))?;
                }
                SelectionValue::FragmentSpread(frag) => {
                    new_selection_set
                        .add_local_selection(&Selection::FragmentSpread(Arc::clone(frag.get())))?;
                }
                SelectionValue::InlineFragment(frag)
                    if !Self::is_worth_using(&frag.get().selection_set) =>
                {
                    new_selection_set
                        .add_local_selection(&Selection::InlineFragment(Arc::clone(frag.get())))?;
                }
                SelectionValue::InlineFragment(mut candidate) => {
                    self.visit_selection_set(candidate.get_selection_set_mut())?;

                    // XXX(@goto-bus-stop): This is temporary to support mismatch testing with JS!
                    // JS federation does not consider fragments without a type condition.
                    if candidate
                        .get()
                        .inline_fragment
                        .type_condition_position
                        .is_none()
                    {
                        new_selection_set.add_local_selection(&Selection::InlineFragment(
                            Arc::clone(candidate.get()),
                        ))?;
                        continue;
                    }

                    let directives = &candidate.get().inline_fragment.directives;
                    let skip_include = directives
                        .iter()
                        .map(|directive| match directive.name.as_str() {
                            "skip" | "include" => Ok(directive.clone()),
                            _ => Err(()),
                        })
                        .collect::<Result<executable::DirectiveList, _>>();

                    // If there are any directives *other* than @skip and @include,
                    // we can't just transfer them to the generated fragment spread,
                    // so we have to keep this inline fragment.
                    let Ok(skip_include) = skip_include else {
                        new_selection_set.add_local_selection(&Selection::InlineFragment(
                            Arc::clone(candidate.get()),
                        ))?;
                        continue;
                    };

                    // XXX(@goto-bus-stop): This is temporary to support mismatch testing with JS!
                    // JS does not special-case @skip and @include. It never extracts a fragment if
                    // there's any directives on it. This code duplicates the body from the
                    // previous condition so it's very easy to remove when we're ready :)
                    if !skip_include.is_empty() {
                        new_selection_set.add_local_selection(&Selection::InlineFragment(
                            Arc::clone(candidate.get()),
                        ))?;
                        continue;
                    }

                    let existing = self.fragments.iter().find(|existing| {
                        existing.type_condition_position
                            == candidate.get().inline_fragment.casted_type()
                            && existing.selection_set == candidate.get().selection_set
                    });

                    let existing = if let Some(existing) = existing {
                        existing
                    } else {
                        // XXX(@goto-bus-stop): This is temporary to support mismatch testing with JS!
                        // This should be reverted to `self.next_name();` when we're ready.
                        let name = self.generate_name(candidate.get());
                        self.fragments.insert(Fragment {
                            schema: selection_set.schema.clone(),
                            name: name.clone(),
                            type_condition_position: candidate.get().inline_fragment.casted_type(),
                            directives: Default::default(),
                            selection_set: candidate.get().selection_set.clone(),
                        });
                        self.fragments.get(&name).unwrap()
                    };
                    new_selection_set.add_local_selection(&Selection::from(
                        FragmentSpreadSelection {
                            spread: FragmentSpread {
                                schema: selection_set.schema.clone(),
                                fragment_name: existing.name.clone(),
                                type_condition_position: existing.type_condition_position.clone(),
                                directives: skip_include.into(),
                                fragment_directives: existing.directives.clone(),
                                selection_id: crate::operation::SelectionId::new(),
                            },
                            selection_set: existing.selection_set.clone(),
                        },
                    ))?;
                }
            }
        }

        *selection_set = new_selection_set;

        Ok(())
    }

    /// Consumes the generator and returns the fragments it generated.
    fn into_inner(self) -> NamedFragments {
        self.fragments
    }
}

//=============================================================================
// Tests

#[cfg(test)]
mod tests {
    use apollo_compiler::ExecutableDocument;

    use super::*;
    use crate::operation::tests::*;

    macro_rules! assert_without_fragments {
        ($operation: expr, @$expected: literal) => {{
            let without_fragments = $operation.expand_all_fragments_and_normalize().unwrap();
            insta::assert_snapshot!(without_fragments, @$expected);
            without_fragments
        }};
    }

    macro_rules! assert_optimized {
        ($operation: expr, $named_fragments: expr, @$expected: literal) => {{
            let mut optimized = $operation.clone();
            optimized.reuse_fragments(&$named_fragments).unwrap();
            validate_operation(&$operation.schema, &optimized.to_string());
            insta::assert_snapshot!(optimized, @$expected)
        }};
    }

    /// Returns a consistent GraphQL name for the given index.
    fn fragment_name(mut index: usize) -> Name {
        /// https://spec.graphql.org/draft/#NameContinue
        const NAME_CHARS: &str = "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789_";
        /// https://spec.graphql.org/draft/#NameStart
        const NAME_START_CHARS: &str = "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ_";

        if index < NAME_START_CHARS.len() {
            Name::new_static_unchecked(&NAME_START_CHARS[index..index + 1])
        } else {
            let mut s = String::new();

            let i = index % NAME_START_CHARS.len();
            s.push(NAME_START_CHARS.as_bytes()[i].into());
            index /= NAME_START_CHARS.len();

            while index > 0 {
                let i = index % NAME_CHARS.len();
                s.push(NAME_CHARS.as_bytes()[i].into());
                index /= NAME_CHARS.len();
            }

            Name::new_unchecked(&s)
        }
    }

    #[test]
    fn generated_fragment_names() {
        assert_eq!(fragment_name(0), "a");
        assert_eq!(fragment_name(100), "Vb");
        assert_eq!(fragment_name(usize::MAX), "oS5Uz8g3Iqw");
    }

    #[test]
    fn duplicate_fragment_spreads_after_fragment_expansion() {
        // This is a regression test for FED-290, making sure `make_select` method can handle
        // duplicate fragment spreads.
        // During optimization, `make_selection` may merge multiple fragment spreads with the same
        // key. This can happen in the case below where `F1` and `F2` are expanded and generating
        // two duplicate `F_shared` spreads in the definition of `fragment F_target`.
        let schema_doc = r#"
            type Query {
                t: T
                t2: T
            }

            type T {
                id: ID!
                a: Int!
                b: Int!
                c: Int!
            }
        "#;

        let query = r#"
            fragment F_shared on T {
                id
                a
            }
            fragment F1 on T {
                ...F_shared
                b
            }

            fragment F2 on T {
                ...F_shared
                c
            }

            fragment F_target on T {
                ...F1
                ...F2
            }

            query {
                t {
                    ...F_target
                }
                t2 {
                    ...F_target
                }
            }
        "#;

        let operation = parse_operation(&parse_schema(schema_doc), query);
        let expanded = operation.expand_all_fragments_and_normalize().unwrap();
        assert_optimized!(expanded, operation.named_fragments, @r###"
        fragment F_target on T {
          id
          a
          b
          c
        }

        {
          t {
            ...F_target
          }
          t2 {
            ...F_target
          }
        }
        "###);
    }

    #[test]
    fn optimize_fragments_using_other_fragments_when_possible() {
        let schema = r#"
              type Query {
                t: I
              }

              interface I {
                b: Int
                u: U
              }

              type T1 implements I {
                a: Int
                b: Int
                u: U
              }

              type T2 implements I {
                x: String
                y: String
                b: Int
                u: U
              }

              union U = T1 | T2
        "#;

        let query = r#"
              fragment OnT1 on T1 {
                a
                b
              }

              fragment OnT2 on T2 {
                x
                y
              }

              fragment OnI on I {
                b
              }

              fragment OnU on U {
                ...OnI
                ...OnT1
                ...OnT2
              }

              query {
                t {
                  ...OnT1
                  ...OnT2
                  ...OnI
                  u {
                    ...OnU
                  }
                }
              }
        "#;

        let operation = parse_operation(&parse_schema(schema), query);

        let expanded = assert_without_fragments!(
            operation,
            @r###"
        {
          t {
            ... on T1 {
              a
              b
            }
            ... on T2 {
              x
              y
            }
            b
            u {
              ... on I {
                b
              }
              ... on T1 {
                a
                b
              }
              ... on T2 {
                x
                y
              }
            }
          }
        }
        "###
        );

        assert_optimized!(expanded, operation.named_fragments, @r###"
              fragment OnU on U {
                ... on I {
                  b
                }
                ... on T1 {
                  a
                  b
                }
                ... on T2 {
                  x
                  y
                }
              }

              {
                t {
                  ...OnU
                  u {
                    ...OnU
                  }
                }
              }
        "###);
    }

    #[test]
    fn handles_fragments_using_other_fragments() {
        let schema = r#"
              type Query {
                t: I
              }

              interface I {
                b: Int
                c: Int
                u1: U
                u2: U
              }

              type T1 implements I {
                a: Int
                b: Int
                c: Int
                me: T1
                u1: U
                u2: U
              }

              type T2 implements I {
                x: String
                y: String
                b: Int
                c: Int
                u1: U
                u2: U
              }

              union U = T1 | T2
        "#;

        let query = r#"
              fragment OnT1 on T1 {
                a
                b
              }

              fragment OnT2 on T2 {
                x
                y
              }

              fragment OnI on I {
                b
                c
              }

              fragment OnU on U {
                ...OnI
                ...OnT1
                ...OnT2
              }

              query {
                t {
                  ...OnT1
                  ...OnT2
                  u1 {
                    ...OnU
                  }
                  u2 {
                    ...OnU
                  }
                  ... on T1 {
                    me {
                      ...OnI
                    }
                  }
                }
              }
        "#;

        let operation = parse_operation(&parse_schema(schema), query);

        let expanded = assert_without_fragments!(
            &operation,
            @r###"
              {
                t {
                  ... on T1 {
                    a
                    b
                    me {
                      b
                      c
                    }
                  }
                  ... on T2 {
                    x
                    y
                  }
                  u1 {
                    ... on I {
                      b
                      c
                    }
                    ... on T1 {
                      a
                      b
                    }
                    ... on T2 {
                      x
                      y
                    }
                  }
                  u2 {
                    ... on I {
                      b
                      c
                    }
                    ... on T1 {
                      a
                      b
                    }
                    ... on T2 {
                      x
                      y
                    }
                  }
                }
              }
        "###);

        // We should reuse and keep all fragments, because 1) onU is used twice and 2)
        // all the other ones are used once in the query, and once in onU definition.
        assert_optimized!(expanded, operation.named_fragments, @r###"
              fragment OnT1 on T1 {
                a
                b
              }

              fragment OnT2 on T2 {
                x
                y
              }

              fragment OnI on I {
                b
                c
              }

              fragment OnU on U {
                ...OnI
                ...OnT1
                ...OnT2
              }

              {
                t {
                  ... on T1 {
                    ...OnT1
                    me {
                      ...OnI
                    }
                  }
                  ...OnT2
                  u1 {
                    ...OnU
                  }
                  u2 {
                    ...OnU
                  }
                }
              }
        "###);
    }

    macro_rules! test_fragments_roundtrip {
        ($schema_doc: expr, $query: expr, @$expanded: literal) => {{
            let schema = parse_schema($schema_doc);
            let operation = parse_operation(&schema, $query);
            let without_fragments = operation.expand_all_fragments_and_normalize().unwrap();
            insta::assert_snapshot!(without_fragments, @$expanded);

            let mut optimized = without_fragments;
            optimized.reuse_fragments(&operation.named_fragments).unwrap();
            validate_operation(&operation.schema, &optimized.to_string());
            assert_eq!(optimized.to_string(), operation.to_string());
        }};
    }

    /// Tests ported from JS codebase rely on special behavior of
    /// `Operation::reuse_fragments_for_roundtrip_test` that is specific for testing, since it makes it
    /// easier to write tests.
    macro_rules! test_fragments_roundtrip_legacy {
        ($schema_doc: expr, $query: expr, @$expanded: literal) => {{
            let schema = parse_schema($schema_doc);
            let operation = parse_operation(&schema, $query);
            let without_fragments = operation.expand_all_fragments_and_normalize().unwrap();
            insta::assert_snapshot!(without_fragments, @$expanded);

            let mut optimized = without_fragments;
            optimized.reuse_fragments_for_roundtrip_test(&operation.named_fragments).unwrap();
            validate_operation(&operation.schema, &optimized.to_string());
            assert_eq!(optimized.to_string(), operation.to_string());
        }};
    }

    #[test]
    fn handles_fragments_with_nested_selections() {
        let schema_doc = r#"
              type Query {
                t1a: T1
                t2a: T1
              }

              type T1 {
                t2: T2
              }

              type T2 {
                x: String
                y: String
              }
        "#;

        let query = r#"
                fragment OnT1 on T1 {
                  t2 {
                    x
                  }
                }

                query {
                  t1a {
                    ...OnT1
                    t2 {
                      y
                    }
                  }
                  t2a {
                    ...OnT1
                  }
                }
        "#;

        test_fragments_roundtrip!(schema_doc, query, @r###"
                {
                  t1a {
                    t2 {
                      x
                      y
                    }
                  }
                  t2a {
                    t2 {
                      x
                    }
                  }
                }
        "###);
    }

    #[test]
    fn handles_nested_fragments_with_field_intersection() {
        let schema_doc = r#"
            type Query {
                t: T
            }

            type T {
                a: A
                b: Int
            }

            type A {
                x: String
                y: String
                z: String
            }
        "#;

        // The subtlety here is that `FA` contains `__typename` and so after we're reused it, the
        // selection will look like:
        // {
        //   t {
        //     a {
        //       ...FA
        //     }
        //   }
        // }
        // But to recognize that `FT` can be reused from there, we need to be able to see that
        // the `__typename` that `FT` wants is inside `FA` (and since FA applies on the parent type `A`
        // directly, it is fine to reuse).
        let query = r#"
            fragment FA on A {
                __typename
                x
                y
            }

            fragment FT on T {
                a {
                __typename
                ...FA
                }
            }

            query {
                t {
                ...FT
                }
            }
        "#;

        test_fragments_roundtrip_legacy!(schema_doc, query, @r###"
        {
          t {
            a {
              __typename
              x
              y
            }
          }
        }
        "###);
    }

    #[test]
    fn handles_fragment_matching_subset_of_field_selection() {
        let schema_doc = r#"
              type Query {
                t: T
              }

              type T {
                a: String
                b: B
                c: Int
                d: D
              }

              type B {
                x: String
                y: String
              }

              type D {
                m: String
                n: String
              }
        "#;

        let query = r#"
                fragment FragT on T {
                  b {
                    __typename
                    x
                  }
                  c
                  d {
                    m
                  }
                }

                {
                  t {
                    ...FragT
                    d {
                      n
                    }
                    a
                  }
                }
        "#;

        test_fragments_roundtrip_legacy!(schema_doc, query, @r###"
                {
                  t {
                    b {
                      __typename
                      x
                    }
                    c
                    d {
                      m
                      n
                    }
                    a
                  }
                }
        "###);
    }

    #[test]
    fn handles_fragment_matching_subset_of_inline_fragment_selection() {
        // Pretty much the same test than the previous one, but matching inside a fragment selection inside
        // of inside a field selection.
        // PORT_NOTE: ` implements I` was added in the definition of `type T`, so that validation can pass.
        let schema_doc = r#"
          type Query {
            i: I
          }

          interface I {
            a: String
          }

          type T implements I {
            a: String
            b: B
            c: Int
            d: D
          }

          type B {
            x: String
            y: String
          }

          type D {
            m: String
            n: String
          }
        "#;

        let query = r#"
            fragment FragT on T {
              b {
                __typename
                x
              }
              c
              d {
                m
              }
            }

            {
              i {
                ... on T {
                  ...FragT
                  d {
                    n
                  }
                  a
                }
              }
            }
        "#;

        test_fragments_roundtrip_legacy!(schema_doc, query, @r###"
            {
              i {
                ... on T {
                  b {
                    __typename
                    x
                  }
                  c
                  d {
                    m
                    n
                  }
                  a
                }
              }
            }
        "###);
    }

    #[test]
    fn intersecting_fragments() {
        let schema_doc = r#"
              type Query {
                t: T
              }

              type T {
                a: String
                b: B
                c: Int
                d: D
              }

              type B {
                x: String
                y: String
              }

              type D {
                m: String
                n: String
              }
        "#;

        // Note: the code that reuse fragments iterates on fragments in the order they are defined
        // in the document, but when it reuse a fragment, it puts it at the beginning of the
        // selection (somewhat random, it just feel often easier to read), so the net effect on
        // this example is that `Frag2`, which will be reused after `Frag1` will appear first in
        // the re-optimized selection. So we put it first in the input too so that input and output
        // actually match (the `testFragmentsRoundtrip` compares strings, so it is sensible to
        // ordering; we could theoretically use `Operation.equals` instead of string equality,
        // which wouldn't really on ordering, but `Operation.equals` is not entirely trivial and
        // comparing strings make problem a bit more obvious).
        let query = r#"
                fragment Frag1 on T {
                  b {
                    x
                  }
                  c
                  d {
                    m
                  }
                }

                fragment Frag2 on T {
                  a
                  b {
                    __typename
                    x
                  }
                  d {
                    m
                    n
                  }
                }

                {
                  t {
                    ...Frag1
                    ...Frag2
                  }
                }
        "#;

        // PORT_NOTE: `__typename` and `x`'s placements are switched in Rust.
        test_fragments_roundtrip_legacy!(schema_doc, query, @r###"
                {
                  t {
                    b {
                      __typename
                      x
                    }
                    c
                    d {
                      m
                      n
                    }
                    a
                  }
                }
        "###);
    }

    #[test]
    fn fragments_application_makes_type_condition_trivial() {
        let schema_doc = r#"
              type Query {
                t: T
              }

              interface I {
                x: String
              }

              type T implements I {
                x: String
                a: String
              }
        "#;

        let query = r#"
                fragment FragI on I {
                  x
                  ... on T {
                    a
                  }
                }

                {
                  t {
                    ...FragI
                  }
                }
        "#;

        test_fragments_roundtrip_legacy!(schema_doc, query, @r###"
                {
                  t {
                    x
                    a
                  }
                }
        "###);
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

        test_fragments_roundtrip_legacy!(schema_doc, query, @r###"
                {
                  t {
                    u {
                      x
                      y
                    }
                    a
                  }
                }
        "###);
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

        test_fragments_roundtrip_legacy!(schema_doc, query, @r###"
                {
                  t1 {
                    y
                  }
                }
        "###);
    }

    #[test]
    fn handles_fragments_used_in_the_context_of_non_intersecting_abstract_types() {
        let schema_doc = r#"
              type Query {
                i2: I2
              }

              interface I1 {
                x: Int
              }

              interface I2 {
                y: Int
              }

              interface I3 {
                z: Int
              }

              type T1 implements I1 & I2 {
                x: Int
                y: Int
              }

              type T2 implements I1 & I3 {
                x: Int
                z: Int
              }
        "#;

        let query = r#"
                fragment FragOnI1 on I1 {
                  ... on I2 {
                    y
                  }
                  ... on I3 {
                    z
                  }
                }

                {
                  i2 {
                    ...FragOnI1
                  }
                }
        "#;

        test_fragments_roundtrip_legacy!(schema_doc, query, @r###"
                {
                  i2 {
                    ... on I1 {
                      ... on I2 {
                        y
                      }
                      ... on I3 {
                        z
                      }
                    }
                  }
                }
        "###);
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

        test_fragments_roundtrip_legacy!(schema_doc, query, @r###"
                {
                  t1 {
                    x
                  }
                }
        "###);
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

        let operation = parse_operation(&parse_schema(schema), query);

        let expanded = assert_without_fragments!(
            operation,
            @r###"
              {
                t {
                  __typename
                  id
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
            "###
        );

        assert_optimized!(expanded, operation.named_fragments, @r###"
        fragment TFrag on T {
          __typename
          id
        }

        {
          t {
            ...TFrag
            v {
              t {
                ...TFrag
                a {
                  __typename
                  id
                }
              }
            }
          }
        }
        "###);
    }

    #[test]
    fn removes_all_unused_fragments() {
        let schema = r#"
              type Query {
                t1: T1
              }

              union U1 = T1 | T2 | T3
              union U2 =      T2 | T3

              type T1 {
                x: Int
              }

              type T2 {
                y: Int
              }

              type T3 {
                z: Int
              }
        "#;

        let query = r#"
              query {
                t1 {
                  ...Outer
                }
              }

              fragment Outer on U1 {
                ... on T1 {
                  x
                }
                ... on T2 {
                  ... Inner
                }
                ... on T3 {
                  ... Inner
                }
              }

              fragment Inner on U2 {
                ... on T2 {
                  y
                }
              }
        "#;

        let operation = parse_operation(&parse_schema(schema), query);

        let expanded = assert_without_fragments!(
            operation,
            @r###"
              {
                t1 {
                  x
                }
              }
            "###
        );

        // This is a bit of contrived example, but the reusing code will be able
        // to figure out that the `Outer` fragment can be reused and will initially
        // do so, but it's only use once, so it will expand it, which yields:
        // {
        //   t1 {
        //     ... on T1 {
        //       x
        //     }
        //     ... on T2 {
        //       ... Inner
        //     }
        //     ... on T3 {
        //       ... Inner
        //     }
        //   }
        // }
        // and so `Inner` will not be expanded (it's used twice). Except that
        // the `flatten_unnecessary_fragments` code is apply then and will _remove_ both instances
        // of `.... Inner`. Which is ok, but we must make sure the fragment
        // itself is removed since it is not used now, which this test ensures.
        assert_optimized!(expanded, operation.named_fragments, @r###"
              {
                t1 {
                  x
                }
              }
        "###);
    }

    #[test]
    fn removes_fragments_only_used_by_unused_fragments() {
        // Similar to the previous test, but we artificially add a
        // fragment that is only used by the fragment that is finally
        // unused.
        let schema = r#"
              type Query {
                t1: T1
              }

              union U1 = T1 | T2 | T3
              union U2 =      T2 | T3

              type T1 {
                x: Int
              }

              type T2 {
                y1: Y
                y2: Y
              }

              type T3 {
                z: Int
              }

              type Y {
                v: Int
              }
        "#;

        let query = r#"
              query {
                t1 {
                  ...Outer
                }
              }

              fragment Outer on U1 {
                ... on T1 {
                  x
                }
                ... on T2 {
                  ... Inner
                }
                ... on T3 {
                  ... Inner
                }
              }

              fragment Inner on U2 {
                ... on T2 {
                  y1 {
                    ...WillBeUnused
                  }
                  y2 {
                    ...WillBeUnused
                  }
                }
              }

              fragment WillBeUnused on Y {
                v
              }
        "#;

        let operation = parse_operation(&parse_schema(schema), query);

        let expanded = assert_without_fragments!(
            operation,
            @r###"
              {
                t1 {
                  x
                }
              }
            "###
        );

        assert_optimized!(expanded, operation.named_fragments, @r###"
              {
                t1 {
                  x
                }
              }
        "###);
    }

    #[test]
    fn keeps_fragments_used_by_other_fragments() {
        let schema = r#"
              type Query {
                t1: T
                t2: T
              }

              type T {
                a1: Int
                a2: Int
                b1: B
                b2: B
              }

              type B {
                x: Int
                y: Int
              }
        "#;

        let query = r#"
              query {
                t1 {
                  ...TFields
                }
                t2 {
                  ...TFields
                }
              }

              fragment TFields on T {
                ...DirectFieldsOfT
                b1 {
                  ...BFields
                }
                b2 {
                  ...BFields
                }
              }

              fragment DirectFieldsOfT on T {
                a1
                a2
              }

              fragment BFields on B {
                x
                y
              }
        "#;

        let operation = parse_operation(&parse_schema(schema), query);

        let expanded = assert_without_fragments!(
            operation,
            @r###"
              {
                t1 {
                  a1
                  a2
                  b1 {
                    x
                    y
                  }
                  b2 {
                    x
                    y
                  }
                }
                t2 {
                  a1
                  a2
                  b1 {
                    x
                    y
                  }
                  b2 {
                    x
                    y
                  }
                }
              }
            "###
        );

        // The `DirectFieldsOfT` fragments should not be kept as it is used only once within `TFields`,
        // but the `BFields` one should be kept.
        assert_optimized!(expanded, operation.named_fragments, @r###"
        fragment BFields on B {
          x
          y
        }

        fragment TFields on T {
          a1
          a2
          b1 {
            ...BFields
          }
          b2 {
            ...BFields
          }
        }

        {
          t1 {
            ...TFields
          }
          t2 {
            ...TFields
          }
        }
        "###);
    }

    ///
    /// applied directives
    ///

    #[test]
    fn reuse_fragments_with_same_directive_in_the_fragment_selection() {
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

                  query myQuery($cond1: Boolean!, $cond2: Boolean!) {
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

        test_fragments_roundtrip_legacy!(schema_doc, query, @r###"
                  query myQuery($cond1: Boolean!, $cond2: Boolean!) {
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
        "###);
    }

    #[test]
    fn reuse_fragments_with_directives_on_inline_fragments() {
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
                  fragment NoDirectiveDef on T {
                    a
                  }

                  query myQuery($cond1: Boolean!) {
                    t1 {
                      ...NoDirectiveDef
                    }
                    t2 {
                      ...NoDirectiveDef @include(if: $cond1)
                    }
                  }
        "#;

        test_fragments_roundtrip!(schema_doc, query, @r###"
                  query myQuery($cond1: Boolean!) {
                    t1 {
                      a
                    }
                    t2 {
                      ... on T @include(if: $cond1) {
                        a
                      }
                    }
                  }
        "###);
    }

    #[test]
    fn reuse_fragments_with_directive_on_typename() {
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
            query A ($if: Boolean!) {
              t1 { b a ...x }
              t2 { ...x }
            }
            query B {
              # Because this inline fragment is exactly the same shape as `x`,
              # except for a `__typename` field, it may be tempting to reuse it.
              # But `x.__typename` has a directive with a variable, and this query
              # does not have that variable declared, so it can't be used.
              t3 { ... on T { a c } }
            }
            fragment x on T {
                __typename @include(if: $if)
                a
                c
            }
        "#;
        let schema = parse_schema(schema);
        let query = ExecutableDocument::parse_and_validate(schema.schema(), query, "query.graphql")
            .unwrap();

        let operation_a =
            Operation::from_operation_document(schema.clone(), &query, Some("A")).unwrap();
        let operation_b =
            Operation::from_operation_document(schema.clone(), &query, Some("B")).unwrap();
        let expanded_b = operation_b.expand_all_fragments_and_normalize().unwrap();

        assert_optimized!(expanded_b, operation_a.named_fragments, @r###"
        query B {
          t3 {
            a
            c
          }
        }
        "###);
    }

    #[test]
    fn reuse_fragments_with_non_intersecting_types() {
        let schema = r#"
            type Query {
              t: T
              s: S
              s2: S
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
            query A ($if: Boolean!) {
              t { ...x }
              s { ...x }
              i { ...x }
            }
            query B {
              s {
                # this matches fragment x once it is flattened,
                # because the `...on T` condition does not intersect with our
                # current type `S`
                __typename
                a b
              }
              s2 {
                # same snippet to get it to use the fragment
                __typename
                a b
              }
            }
            fragment x on I {
                __typename
                a
                b
                ... on T { c d @include(if: $if) }
            }
        "#;
        let schema = parse_schema(schema);
        let query = ExecutableDocument::parse_and_validate(schema.schema(), query, "query.graphql")
            .unwrap();

        let operation_a =
            Operation::from_operation_document(schema.clone(), &query, Some("A")).unwrap();
        let operation_b =
            Operation::from_operation_document(schema.clone(), &query, Some("B")).unwrap();
        let expanded_b = operation_b.expand_all_fragments_and_normalize().unwrap();

        assert_optimized!(expanded_b, operation_a.named_fragments, @r###"
        query B {
          s {
            __typename
            a
            b
          }
          s2 {
            __typename
            a
            b
          }
        }
        "###);
    }

    ///
    /// empty branches removal
    ///

    mod test_empty_branch_removal {
        use apollo_compiler::name;

        use super::*;
        use crate::operation::SelectionKey;

        const TEST_SCHEMA_FOR_EMPTY_BRANCH_REMOVAL: &str = r#"
            type Query {
                t: T
                u: Int
            }

            type T {
                a: Int
                b: Int
                c: C
            }

            type C {
                x: String
                y: String
            }
        "#;

        fn operation_without_empty_branches(operation: &Operation) -> Option<String> {
            operation
                .selection_set
                .without_empty_branches()
                .unwrap()
                .map(|s| s.to_string())
        }

        fn without_empty_branches(query: &str) -> Option<String> {
            let operation =
                parse_operation(&parse_schema(TEST_SCHEMA_FOR_EMPTY_BRANCH_REMOVAL), query);
            operation_without_empty_branches(&operation)
        }

        // To test `without_empty_branches` method, we need to test operations with empty selection
        // sets. However, such operations can't be constructed from strings, since the parser will
        // reject them. Thus, we first create a valid query with non-empty selection sets and then
        // clear some of them.
        // PORT_NOTE: The JS tests use `astSSet` function to construct queries with
        // empty selection sets using graphql-js's SelectionSetNode API. In Rust version,
        // instead of re-creating such API, we will selectively clear selection sets.

        fn clear_selection_set_at_path(
            ss: &mut SelectionSet,
            path: &[Name],
        ) -> Result<(), FederationError> {
            match path.split_first() {
                None => {
                    // Base case
                    ss.selections = Default::default();
                    Ok(())
                }

                Some((first, rest)) => {
                    let result = Arc::make_mut(&mut ss.selections).get_mut(SelectionKey::Field {
                        response_name: first,
                        directives: &Default::default(),
                    });
                    let Some(mut value) = result else {
                        return Err(FederationError::internal("No matching field found"));
                    };
                    match value.get_selection_set_mut() {
                        None => Err(FederationError::internal(
                            "Sub-selection expected, but not found.",
                        )),
                        Some(sub_selection_set) => {
                            // Recursive case
                            clear_selection_set_at_path(sub_selection_set, rest)?;
                            Ok(())
                        }
                    }
                }
            }
        }

        #[test]
        fn operation_not_modified_if_no_empty_branches() {
            let test_vec = vec!["{ t { a } }", "{ t { a b } }", "{ t { a c { x y } } }"];
            for query in test_vec {
                assert_eq!(without_empty_branches(query).unwrap(), query);
            }
        }

        #[test]
        fn removes_simple_empty_branches() {
            {
                // query to test: "{ t { a c { } } }"
                let expected = "{ t { a } }";

                // Since the parser won't accept empty selection set, we first create
                // a valid query and then clear the selection set.
                let valid_query = r#"{ t { a c { x } } }"#;
                let mut operation = parse_operation(
                    &parse_schema(TEST_SCHEMA_FOR_EMPTY_BRANCH_REMOVAL),
                    valid_query,
                );
                clear_selection_set_at_path(
                    &mut operation.selection_set,
                    &[name!("t"), name!("c")],
                )
                .unwrap();
                // Note: Unfortunately, this assertion won't work since SelectionSet.to_string() can't
                // display empty selection set.
                // assert_eq!(operation.selection_set.to_string(), "{ t { a c { } } }");
                assert_eq!(
                    operation_without_empty_branches(&operation).unwrap(),
                    expected
                );
            }

            {
                // query to test: "{ t { c { } a } }"
                let expected = "{ t { a } }";

                let valid_query = r#"{ t { c { x } a } }"#;
                let mut operation = parse_operation(
                    &parse_schema(TEST_SCHEMA_FOR_EMPTY_BRANCH_REMOVAL),
                    valid_query,
                );
                clear_selection_set_at_path(
                    &mut operation.selection_set,
                    &[name!("t"), name!("c")],
                )
                .unwrap();
                assert_eq!(
                    operation_without_empty_branches(&operation).unwrap(),
                    expected
                );
            }

            {
                // query to test: "{ t { } }"
                let expected = None;

                let valid_query = r#"{ t { a } }"#;
                let mut operation = parse_operation(
                    &parse_schema(TEST_SCHEMA_FOR_EMPTY_BRANCH_REMOVAL),
                    valid_query,
                );
                clear_selection_set_at_path(&mut operation.selection_set, &[name!("t")]).unwrap();
                assert_eq!(operation_without_empty_branches(&operation), expected);
            }
        }

        #[test]
        fn removes_cascading_empty_branches() {
            {
                // query to test: "{ t { c { } } }"
                let expected = None;

                let valid_query = r#"{ t { c { x } } }"#;
                let mut operation = parse_operation(
                    &parse_schema(TEST_SCHEMA_FOR_EMPTY_BRANCH_REMOVAL),
                    valid_query,
                );
                clear_selection_set_at_path(
                    &mut operation.selection_set,
                    &[name!("t"), name!("c")],
                )
                .unwrap();
                assert_eq!(operation_without_empty_branches(&operation), expected);
            }

            {
                // query to test: "{ u t { c { } } }"
                let expected = "{ u }";

                let valid_query = r#"{ u t { c { x } } }"#;
                let mut operation = parse_operation(
                    &parse_schema(TEST_SCHEMA_FOR_EMPTY_BRANCH_REMOVAL),
                    valid_query,
                );
                clear_selection_set_at_path(
                    &mut operation.selection_set,
                    &[name!("t"), name!("c")],
                )
                .unwrap();
                assert_eq!(
                    operation_without_empty_branches(&operation).unwrap(),
                    expected
                );
            }

            {
                // query to test: "{ t { c { } } u }"
                let expected = "{ u }";

                let valid_query = r#"{ t { c { x } } u }"#;
                let mut operation = parse_operation(
                    &parse_schema(TEST_SCHEMA_FOR_EMPTY_BRANCH_REMOVAL),
                    valid_query,
                );
                clear_selection_set_at_path(
                    &mut operation.selection_set,
                    &[name!("t"), name!("c")],
                )
                .unwrap();
                assert_eq!(
                    operation_without_empty_branches(&operation).unwrap(),
                    expected
                );
            }
        }
    }
}
