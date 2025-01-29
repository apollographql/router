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

use apollo_compiler::collections::HashMap;
use apollo_compiler::collections::IndexMap;
use apollo_compiler::executable;
use apollo_compiler::Name;

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
use crate::schema::position::CompositeTypeDefinitionPosition;
//=============================================================================
// Selection/SelectionSet minus operation

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

    /// Optimize the parsed size of the operation by generating fragments from selection sets that
    /// occur multiple times in the operation.
    pub(crate) fn generate_fragments_v2(&mut self) -> Result<(), FederationError> {
        let mut generator = FragmentGenerator::default();
        generator.collect_selection_usages(&self.selection_set);
        generator.minify(&mut self.selection_set)?;
        self.named_fragments = generator.into_minimized();
        Ok(())
    }
}

#[derive(Debug, Default)]
struct FragmentGenerator {
    fragments: NamedFragments,
    // XXX(@goto-bus-stop): This is temporary to support mismatch testing with JS!
    names: IndexMap<(String, usize), usize>,
    // TODO v2 stuff below - remove v1 after analysis
    selection_counts: HashMap<u64, usize>,
    minimized_fragments: IndexMap<u64, Fragment>,
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

impl FragmentGenerator {
    fn next_name(&self) -> Name {
        fragment_name(self.minimized_fragments.len())
    }

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

    fn hash_key(&self, selection_set: &SelectionSet) -> u64 {
        #[derive(PartialEq, Eq, Hash)]
        struct NamedFragmentCandidateKey<'a> {
            type_position: &'a CompositeTypeDefinitionPosition,
            selection_set: &'a SelectionSet,
        }
        let key = NamedFragmentCandidateKey {
            type_position: &selection_set.type_position,
            selection_set
        };
        self.minimized_fragments.hasher().hash_one(key)
    }

    fn increment_selection_count(&mut self, selection_set: &SelectionSet) {
        let hash = self.hash_key(selection_set);
        *self.selection_counts.entry(hash).or_insert(0) += 1;
    }

    /// Recursively iterate over all selections to capture counts of how many times given selection
    /// occurs within the operation.
    fn collect_selection_usages(&mut self, selection_set: &SelectionSet) {
        for selection in selection_set.selections.values() {
            match selection {
                Selection::Field(field) => {
                    if let Some(field_selection_set) = &field.selection_set {
                        self.increment_selection_count(field_selection_set);
                        self.collect_selection_usages(field_selection_set);
                    }
                }
                Selection::InlineFragment(frag) => {
                    self.increment_selection_count(&frag.selection_set);
                    self.collect_selection_usages(&frag.selection_set);
                }
                Selection::FragmentSpread(_) => {
                    // nothing to here as it is already a fragment spread
                    // NOTE: there shouldn't be any fragment spreads in selections at this time
                    continue;
                }
            }
        }
    }

    /// Recursively iterates over all selections to check if their selection sets are used multiple
    /// times within the operation. Every selection set that is used more than once will be extracted
    /// as a named fragment.
    fn minify(&mut self, selection_set: &mut SelectionSet) -> Result<(), FederationError> {
        // iterate over all selections to check if given selection is used multiple times
        // if selection is used multiple times then we extract it as named fragment
        let mut new_selection_set = SelectionSet::empty(
            selection_set.schema.clone(),
            selection_set.type_position.clone(),
        );

        for selection in Arc::make_mut(&mut selection_set.selections).values_mut() {
            match selection {
                SelectionValue::Field(mut field) => {
                    if let Some(field_selection_set) = field.get_selection_set_mut() {
                        let hash = self.hash_key(field_selection_set);
                        if self
                            .selection_counts
                            .get(&hash)
                            .is_some_and(|count| count > &1)
                        {
                            // extract named fragment OR use one that already exists
                            let fragment =
                                if let Some(existing) = self.minimized_fragments.get(&hash) {
                                    existing
                                } else {
                                    // minify current selection set and extract named fragment
                                    self.minify(field_selection_set)?;
                                    self.minimized_fragments.insert(
                                        hash,
                                        Fragment {
                                            schema: field_selection_set.schema.clone(),
                                            name: self.next_name(),
                                            type_condition_position: field_selection_set
                                                .type_position
                                                .clone(),
                                            directives: Default::default(),
                                            selection_set: field_selection_set.clone(),
                                        },
                                    );
                                    self.minimized_fragments.get(&hash).unwrap()
                                };

                            // replace field selection set with fragment spread
                            let schema = &selection_set.schema;
                            *field_selection_set = SelectionSet::empty(
                                schema.clone(),
                                field_selection_set.type_position.clone(),
                            );
                            field_selection_set.add_local_selection(&Selection::from(
                                FragmentSpreadSelection {
                                    spread: FragmentSpread {
                                        schema: fragment.schema.clone(),
                                        fragment_name: fragment.name.clone(),
                                        type_condition_position: fragment
                                            .type_condition_position
                                            .clone(),
                                        directives: Default::default(),
                                        fragment_directives: fragment.directives.clone(),
                                        selection_id: crate::operation::SelectionId::new(),
                                    },
                                    selection_set: fragment.selection_set.clone(),
                                },
                            ))?;
                        } else {
                            // minify current sub selection as it cannot be updated to a fragment reference
                            self.minify(field_selection_set)?;
                        }
                    }
                    new_selection_set
                        .add_local_selection(&Selection::Field(Arc::clone(field.get())))?;
                }
                SelectionValue::FragmentSpread(frag) => {
                    // already fragment spread so just copy it over
                    new_selection_set
                        .add_local_selection(&Selection::FragmentSpread(Arc::clone(frag.get())))?;
                }
                SelectionValue::InlineFragment(mut inline_fragment) => {
                    let hash = self.hash_key(&inline_fragment.get().selection_set);
                    if self
                        .selection_counts
                        .get(&hash)
                        .is_some_and(|count| count > &1)
                    {
                        // extract named fragment OR use one that already exists
                        let fragment = if let Some(existing) = self.minimized_fragments.get(&hash) {
                            existing
                        } else {
                            self.minify(inline_fragment.get_selection_set_mut())?;
                            let name = self.next_name();
                            self.minimized_fragments.insert(
                                hash,
                                Fragment {
                                    schema: selection_set.schema.clone(),
                                    name: name.clone(),
                                    type_condition_position: inline_fragment
                                        .get()
                                        .inline_fragment
                                        .casted_type(),
                                    directives: Default::default(),
                                    selection_set: inline_fragment.get().selection_set.clone(),
                                },
                            );
                            self.minimized_fragments.get(&hash).unwrap()
                        };

                        let directives = &inline_fragment.get().inline_fragment.directives;
                        let skip_include_only = directives
                            .iter()
                            .all(|d| d.name.as_str() == "skip" || d.name.as_str() == "include");

                        if skip_include_only {
                            // convert inline fragment selection to fragment spread
                            let fragment_spread_selection =
                                Selection::from(FragmentSpreadSelection {
                                    spread: FragmentSpread {
                                        schema: selection_set.schema.clone(),
                                        fragment_name: fragment.name.clone(),
                                        type_condition_position: fragment
                                            .type_condition_position
                                            .clone(),
                                        directives: directives.clone(),
                                        fragment_directives: fragment.directives.clone(),
                                        selection_id: crate::operation::SelectionId::new(),
                                    },
                                    selection_set: fragment.selection_set.clone(),
                                });

                            new_selection_set.add_local_selection(&fragment_spread_selection)?;
                        } else {
                            // cannot lift out inline selection directly as it has directives
                            // extract named fragment from inline fragment selections
                            let fragment_spread_selection =
                                Selection::from(FragmentSpreadSelection {
                                    spread: FragmentSpread {
                                        schema: selection_set.schema.clone(),
                                        fragment_name: fragment.name.clone(),
                                        type_condition_position: fragment
                                            .type_condition_position
                                            .clone(),
                                        directives: Default::default(),
                                        fragment_directives: fragment.directives.clone(),
                                        selection_id: crate::operation::SelectionId::new(),
                                    },
                                    selection_set: fragment.selection_set.clone(),
                                });

                            let mut new_inline_selection_set = SelectionSet::empty(
                                fragment.schema.clone(),
                                fragment.type_condition_position.clone(),
                            );
                            new_inline_selection_set
                                .add_local_selection(&fragment_spread_selection)?;
                            *inline_fragment.get_selection_set_mut() = new_inline_selection_set;
                            new_selection_set.add_local_selection(&Selection::InlineFragment(
                                Arc::clone(inline_fragment.get()),
                            ))?;
                        }
                    } else {
                        self.minify(inline_fragment.get_selection_set_mut())?;
                        new_selection_set.add_local_selection(&Selection::InlineFragment(
                            Arc::clone(inline_fragment.get()),
                        ))?;
                    }
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

    fn into_minimized(self) -> NamedFragments {
        let mut named_fragments = NamedFragments::default();
        for (_, fragment) in &self.minimized_fragments {
            named_fragments.insert(fragment.clone());
        }
        named_fragments
    }
}

//=============================================================================
// Tests

#[cfg(test)]
mod tests {
    use super::*;
    use crate::operation::tests::*;

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

    mod fragment_generation {
        use crate::operation::tests::parse_and_expand;
        use crate::operation::tests::parse_schema;

        #[test]
        fn extracts_common_selections() {
            let schema_doc = r#"
              type Query {
                t1: T
                t2: T
              }

              type T {
                a: String
                b: String
                c: Int
              }
            "#;
            let schema = parse_schema(schema_doc);
            let mut query = parse_and_expand(
                &schema,
                r#"
                query {
                  t1 {
                    a
                    b
                    c
                  }
                  t2 {
                    a
                    b
                    c
                  }
                }
                "#,
            )
            .expect("query is valid");

            query
                .generate_fragments_v2()
                .expect("successfully generated fragments");
            insta::assert_snapshot!(query, @r###"
            fragment a on T {
              a
              b
              c
            }

            {
              t1 {
                ...a
              }
              t2 {
                ...a
              }
            }
            "###);
        }

        #[test]
        fn extracts_common_order_independent_selections() {
            let schema_doc = r#"
              type Query {
                t1: T
                t2: T
              }

              type T {
                a: String
                b: String
                c: Int
              }
            "#;
            let schema = parse_schema(schema_doc);
            let mut query = parse_and_expand(
                &schema,
                r#"
                query {
                  t1 {
                    a
                    b
                    c
                  }
                  t2 {
                    c
                    b
                    a
                  }
                }
                "#,
            )
            .expect("query is valid");

            query
                .generate_fragments_v2()
                .expect("successfully generated fragments");
            insta::assert_snapshot!(query, @r###"
            fragment a on T {
              a
              b
              c
            }

            {
              t1 {
                ...a
              }
              t2 {
                ...a
              }
            }
            "###);
        }

        #[test]
        fn does_not_extract_different_sub_selections() {
            let schema_doc = r#"
              type Query {
                t1: T
                t2: T
              }

              type T {
                a: String
                b: String
                c: Int
              }
            "#;
            let schema = parse_schema(schema_doc);
            let mut query = parse_and_expand(
                &schema,
                r#"
                query {
                  t1 {
                    a
                    b
                  }
                  t2 {
                    a
                    b
                    c
                  }
                }
                "#,
            )
            .expect("query is valid");

            query
                .generate_fragments_v2()
                .expect("no fragments were generated");
            insta::assert_snapshot!(query, @r###"
            {
              t1 {
                a
                b
              }
              t2 {
                a
                b
                c
              }
            }
            "###);
        }

        #[test]
        fn does_not_extract_selections_on_different_types() {
            let schema_doc = r#"
              type Query {
                t1: T1
                t2: T2
              }

              type T1 {
                a: String
                b: String
                c: Int
              }

              type T2 {
                a: String
                b: String
                c: Int
              }
            "#;
            let schema = parse_schema(schema_doc);
            let mut query = parse_and_expand(
                &schema,
                r#"
                query {
                  t1 {
                    a
                    b
                    c
                  }
                  t2 {
                    a
                    b
                    c
                  }
                }
                "#,
            )
            .expect("query is valid");

            query
                .generate_fragments_v2()
                .expect("no fragments were generated");
            insta::assert_snapshot!(query, @r###"
            {
              t1 {
                a
                b
                c
              }
              t2 {
                a
                b
                c
              }
            }
            "###);
        }

        #[test]
        fn extracts_common_inline_fragment_selections() {
            let schema_doc = r#"
              type Query {
                i1: I
                i2: I
              }

              interface I {
                a: String
              }

              type T implements I {
                a: String
                b: String
                c: Int
              }
            "#;
            let schema = parse_schema(schema_doc);
            let mut query = parse_and_expand(
                &schema,
                r#"
                query {
                  i1 {
                    ... on T {
                      a
                      b
                      c
                    }
                  }
                  i2 {
                    ... on T {
                      a
                      b
                      c
                    }
                  }
                }
                "#,
            )
            .expect("query is valid");

            query
                .generate_fragments_v2()
                .expect("successfully generated fragments");
            insta::assert_snapshot!(query, @r###"
            fragment a on T {
              a
              b
              c
            }

            fragment b on I {
              ...a
            }

            {
              i1 {
                ...b
              }
              i2 {
                ...b
              }
            }
            "###);
        }

        #[test]
        fn extracts_common_field_and_inline_fragment_selections() {
            let schema_doc = r#"
              type Query {
                i: I
                t: T
              }

              interface I {
                a: String
              }

              type T implements I {
                a: String
                b: String
                c: Int
              }
            "#;
            let schema = parse_schema(schema_doc);
            let mut query = parse_and_expand(
                &schema,
                r#"
                query {
                  i {
                    ... on T {
                      a
                      b
                      c
                    }
                  }
                  t {
                    a
                    b
                    c
                  }
                }
                "#,
            )
            .expect("query is valid");

            query
                .generate_fragments_v2()
                .expect("successfully generated fragments");
            insta::assert_snapshot!(query, @r###"
            fragment a on T {
              a
              b
              c
            }

            {
              i {
                ...a
              }
              t {
                ...a
              }
            }
            "###);
        }

        #[test]
        fn extracts_common_sub_selections() {
            let schema_doc = r#"
              type Query {
                t1: T
                t2: T
              }

              type T {
                a: String
                b: String
                c: Int
                v: V
              }

              type V {
                x: String
                y: String
              }
            "#;
            let schema = parse_schema(schema_doc);
            let mut query = parse_and_expand(
                &schema,
                r#"
                query {
                  t1 {
                    a
                    b
                    v {
                      x
                      y
                    }
                  }
                  t2 {
                    a
                    b
                    c
                    v {
                      x
                      y
                    }
                  }
                }
                "#,
            )
            .expect("query is valid");

            query
                .generate_fragments_v2()
                .expect("successfully generated fragments");
            insta::assert_snapshot!(query, @r###"
            fragment a on V {
              x
              y
            }

            {
              t1 {
                a
                b
                v {
                  ...a
                }
              }
              t2 {
                a
                b
                c
                v {
                  ...a
                }
              }
            }
            "###);
        }

        #[test]
        fn extracts_common_complex_selections() {
            let schema_doc = r#"
              type Query {
                t1: T
                t2: T
              }

              type T {
                a: String
                b: String
                c: Int
                v: V
              }

              type V {
                x: String
                y: String
              }
            "#;
            let schema = parse_schema(schema_doc);
            let mut query = parse_and_expand(
                &schema,
                r#"
                query {
                  t1 {
                    a
                    b
                    c
                    v {
                      x
                      y
                    }
                  }
                  t2 {
                    a
                    b
                    c
                    v {
                      ...FragmentV
                    }
                  }
                }

                fragment FragmentV on V {
                  x
                  y
                }
                "#,
            )
            .expect("query is valid");

            query
                .generate_fragments_v2()
                .expect("successfully generated fragments");
            insta::assert_snapshot!(query, @r###"
            fragment a on V {
              x
              y
            }

            fragment b on T {
              a
              b
              c
              v {
                ...a
              }
            }

            {
              t1 {
                ...b
              }
              t2 {
                ...b
              }
            }
            "###);
        }

        #[test]
        fn handles_include_skip() {
            let schema_doc = r#"
              type Query {
                t1: T
                t2: T
              }

              type T {
                a: String
                b: String
                c: Int
                v: V
              }

              type V {
                x: String
                y: String
              }
            "#;
            let schema = parse_schema(schema_doc);
            let mut query = parse_and_expand(
                &schema,
                r#"
                query {
                  t1 {
                    a
                    b
                    c
                    v @include(if: true) {
                      x
                      y
                    }
                  }
                  t2 {
                    a
                    b
                    c
                    v {
                      ...FragmentV @skip(if: false)
                    }
                  }
                }

                fragment FragmentV on V {
                  x
                  y
                }
                "#,
            )
            .expect("query is valid");

            query
                .generate_fragments_v2()
                .expect("successfully generated fragments");
            insta::assert_snapshot!(query, @r###"
            fragment a on V {
              x
              y
            }

            {
              t1 {
                a
                b
                c
                v @include(if: true) {
                  ...a
                }
              }
              t2 {
                a
                b
                c
                v {
                  ...a @skip(if: false)
                }
              }
            }
            "###);
        }

        #[test]
        fn handles_skip_on_inline_fragments() {
            let schema_doc = r#"
              type Query {
                i1: I
                i2: I
              }

              interface I {
                a: String
              }

              type T implements I {
                a: String
                b: String
                c: Int
              }
            "#;
            let schema = parse_schema(schema_doc);
            let mut query = parse_and_expand(
                &schema,
                r#"
                query {
                  i1 {
                    ... on T @skip(if: false) {
                      a
                      b
                      c
                    }
                  }
                  i2 {
                    ... on T {
                      a
                      b
                      c
                    }
                  }
                }
                "#,
            )
            .expect("query is valid");

            query
                .generate_fragments_v2()
                .expect("successfully generated fragments");
            insta::assert_snapshot!(query, @r###"
            fragment a on T {
              a
              b
              c
            }

            {
              i1 {
                ...a @skip(if: false)
              }
              i2 {
                ...a
              }
            }
            "###);
        }
    }
}
