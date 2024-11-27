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
    use super::*;
    use crate::operation::tests::*;

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
