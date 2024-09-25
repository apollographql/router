use std::borrow::Cow;
use std::iter::Map;
use std::ops::Deref;
use std::sync::Arc;

use apollo_compiler::collections::IndexMap;
use apollo_compiler::Name;
use serde::Serialize;

use crate::error::FederationError;
use crate::error::SingleFederationError::Internal;
use crate::operation::field_selection::FieldSelection;
use crate::operation::fragment_spread_selection::FragmentSpreadSelection;
use crate::operation::inline_fragment_selection::InlineFragmentSelection;
use crate::operation::DirectiveList;
use crate::operation::Selection;
use crate::operation::SelectionId;
use crate::operation::SelectionSet;
use crate::operation::SiblingTypename;

/// A selection "key" (unrelated to the federation `@key` directive) is an identifier of a selection
/// (field, inline fragment, or fragment spread) that is used to determine whether two selections
/// can be merged.
///
/// In order to merge two selections they need to
/// * reference the same field/inline fragment
/// * specify the same directives
/// * directives have to be applied in the same order
/// * directive arguments order does not matter (they get automatically sorted by their names).
/// * selection cannot specify @defer directive
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
pub(crate) enum SelectionKey {
    Field {
        /// The field alias (if specified) or field name in the resulting selection set.
        response_name: Name,
        /// directives applied on the field
        #[serde(serialize_with = "crate::display_helpers::serialize_as_string")]
        directives: DirectiveList,
    },
    FragmentSpread {
        /// The name of the fragment.
        fragment_name: Name,
        /// Directives applied on the fragment spread (does not contain @defer).
        #[serde(serialize_with = "crate::display_helpers::serialize_as_string")]
        directives: DirectiveList,
    },
    InlineFragment {
        /// The optional type condition of the fragment.
        type_condition: Option<Name>,
        /// Directives applied on the fragment spread (does not contain @defer).
        #[serde(serialize_with = "crate::display_helpers::serialize_as_string")]
        directives: DirectiveList,
    },
    Defer {
        /// Unique selection ID used to distinguish deferred fragment spreads that cannot be merged.
        #[cfg_attr(not(feature = "snapshot_tracing"), serde(skip))]
        deferred_id: SelectionId,
    },
}

impl SelectionKey {
    /// Returns true if the selection key is `__typename` *without directives*.
    #[deprecated = "Use the Selection type instead"]
    pub(crate) fn is_typename_field(&self) -> bool {
        matches!(self, SelectionKey::Field { response_name, directives } if *response_name == super::TYPENAME_FIELD && directives.is_empty())
    }

    /// Create a selection key for a specific field name.
    ///
    /// This is available for tests only as selection keys should not normally be created outside of
    /// `HasSelectionKey::key`.
    #[cfg(test)]
    pub(crate) fn field_name(name: &str) -> Self {
        SelectionKey::Field {
            response_name: Name::new(name).unwrap(),
            directives: Default::default(),
        }
    }
}

pub(crate) trait HasSelectionKey {
    fn key(&self) -> SelectionKey;
}

/// A "normalized" selection map is an optimized representation of a selection set which does
/// not contain selections with the same selection "key". Selections that do have the same key
/// are  merged during the normalization process. By storing a selection set as a map, we can
/// efficiently merge/join multiple selection sets.
///
/// Because the key depends strictly on the value, we expose the underlying map's API in a
/// read-only capacity, while mutations use an API closer to `IndexSet`. We don't just use an
/// `IndexSet` since key computation is expensive (it involves sorting). This type is in its own
/// module to prevent code from accidentally mutating the underlying map outside the mutation
/// API.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize)]
pub(crate) struct SelectionMap(IndexMap<SelectionKey, Selection>);

impl Deref for SelectionMap {
    type Target = IndexMap<SelectionKey, Selection>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl SelectionMap {
    pub(crate) fn new() -> Self {
        SelectionMap(IndexMap::default())
    }

    pub(crate) fn clear(&mut self) {
        self.0.clear();
    }

    pub(crate) fn insert(&mut self, value: Selection) -> Option<Selection> {
        self.0.insert(value.key(), value)
    }

    /// Insert a selection at a specific index.
    pub(crate) fn insert_at(&mut self, index: usize, value: Selection) -> Option<Selection> {
        self.0.shift_insert(index, value.key(), value)
    }

    /// Remove a selection from the map. Returns the selection and its numeric index.
    pub(crate) fn remove(&mut self, key: &SelectionKey) -> Option<(usize, Selection)> {
        // We specifically use shift_remove() instead of swap_remove() to maintain order.
        self.0
            .shift_remove_full(key)
            .map(|(index, _key, selection)| (index, selection))
    }

    pub(crate) fn retain(
        &mut self,
        mut predicate: impl FnMut(&SelectionKey, &Selection) -> bool,
    ) {
        self.0.retain(|k, v| predicate(k, v))
    }

    pub(crate) fn get_mut(&mut self, key: &SelectionKey) -> Option<SelectionValue> {
        self.0.get_mut(key).map(SelectionValue::new)
    }

    pub(crate) fn iter_mut(&mut self) -> IterMut {
        self.0.iter_mut().map(|(k, v)| (k, SelectionValue::new(v)))
    }

    pub(super) fn entry(&mut self, key: SelectionKey) -> Entry {
        match self.0.entry(key) {
            indexmap::map::Entry::Occupied(entry) => Entry::Occupied(OccupiedEntry(entry)),
            indexmap::map::Entry::Vacant(entry) => Entry::Vacant(VacantEntry(entry)),
        }
    }

    pub(crate) fn extend(&mut self, other: SelectionMap) {
        self.0.extend(other.0)
    }

    pub(crate) fn extend_ref(&mut self, other: &SelectionMap) {
        self.0
            .extend(other.iter().map(|(k, v)| (k.clone(), v.clone())))
    }

    /// Returns the selection set resulting from "recursively" filtering any selection
    /// that does not match the provided predicate.
    /// This method calls `predicate` on every selection of the selection set,
    /// not just top-level ones, and apply a "depth-first" strategy:
    /// when the predicate is called on a given selection it is guaranteed that
    /// filtering has happened on all the selections of its sub-selection.
    pub(crate) fn filter_recursive_depth_first(
        &self,
        predicate: &mut dyn FnMut(&Selection) -> Result<bool, FederationError>,
    ) -> Result<Cow<'_, Self>, FederationError> {
        fn recur_sub_selections<'sel>(
            selection: &'sel Selection,
            predicate: &mut dyn FnMut(&Selection) -> Result<bool, FederationError>,
        ) -> Result<Cow<'sel, Selection>, FederationError> {
            Ok(match selection {
                Selection::Field(field) => {
                    if let Some(sub_selections) = &field.selection_set {
                        match sub_selections.filter_recursive_depth_first(predicate)? {
                            Cow::Borrowed(_) => Cow::Borrowed(selection),
                            Cow::Owned(new) => Cow::Owned(Selection::from_field(
                                field.field.clone(),
                                Some(new),
                            )),
                        }
                    } else {
                        Cow::Borrowed(selection)
                    }
                }
                Selection::InlineFragment(fragment) => match fragment
                    .selection_set
                    .filter_recursive_depth_first(predicate)?
                {
                    Cow::Borrowed(_) => Cow::Borrowed(selection),
                    Cow::Owned(selection_set) => Cow::Owned(Selection::InlineFragment(
                        Arc::new(InlineFragmentSelection::new(
                            fragment.inline_fragment.clone(),
                            selection_set,
                        )),
                    )),
                },
                Selection::FragmentSpread(_) => {
                    return Err(FederationError::internal("unexpected fragment spread"))
                }
            })
        }
        let mut iter = self.0.iter();
        let mut enumerated = (&mut iter).enumerate();
        let mut new_map: IndexMap<_, _>;
        loop {
            let Some((index, (key, selection))) = enumerated.next() else {
                return Ok(Cow::Borrowed(self));
            };
            let filtered = recur_sub_selections(selection, predicate)?;
            let keep = predicate(&filtered)?;
            if keep && matches!(filtered, Cow::Borrowed(_)) {
                // Nothing changed so far, continue without cloning
                continue;
            }

            // Clone the map so far
            new_map = self.0.as_slice()[..index]
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect();

            if keep {
                new_map.insert(key.clone(), filtered.into_owned());
            }
            break;
        }
        for (key, selection) in iter {
            let filtered = recur_sub_selections(selection, predicate)?;
            if predicate(&filtered)? {
                new_map.insert(key.clone(), filtered.into_owned());
            }
        }
        Ok(Cow::Owned(Self(new_map)))
    }
}

impl<A> FromIterator<A> for SelectionMap
where
    A: Into<Selection>,
{
    fn from_iter<T: IntoIterator<Item = A>>(iter: T) -> Self {
        let mut map = Self::new();
        for selection in iter {
            map.insert(selection.into());
        }
        map
    }
}

type IterMut<'a> = Map<
    indexmap::map::IterMut<'a, SelectionKey, Selection>,
    fn((&'a SelectionKey, &'a mut Selection)) -> (&'a SelectionKey, SelectionValue<'a>),
>;

/// A mutable reference to a `Selection` value in a `SelectionMap`, which
/// also disallows changing key-related data (to maintain the invariant that a value's key is
/// the same as it's map entry's key).
#[derive(Debug)]
pub(crate) enum SelectionValue<'a> {
    Field(FieldSelectionValue<'a>),
    FragmentSpread(FragmentSpreadSelectionValue<'a>),
    InlineFragment(InlineFragmentSelectionValue<'a>),
}

impl<'a> SelectionValue<'a> {
    pub(crate) fn new(selection: &'a mut Selection) -> Self {
        match selection {
            Selection::Field(field_selection) => {
                SelectionValue::Field(FieldSelectionValue::new(field_selection))
            }
            Selection::FragmentSpread(fragment_spread_selection) => {
                SelectionValue::FragmentSpread(FragmentSpreadSelectionValue::new(
                    fragment_spread_selection,
                ))
            }
            Selection::InlineFragment(inline_fragment_selection) => {
                SelectionValue::InlineFragment(InlineFragmentSelectionValue::new(
                    inline_fragment_selection,
                ))
            }
        }
    }

    pub(super) fn directives(&self) -> &'_ DirectiveList {
        match self {
            Self::Field(field) => &field.get().field.directives,
            Self::FragmentSpread(frag) => &frag.get().spread.directives,
            Self::InlineFragment(frag) => &frag.get().inline_fragment.directives,
        }
    }

    pub(super) fn get_selection_set_mut(&mut self) -> Option<&mut SelectionSet> {
        match self {
            Self::Field(field) => field.get_selection_set_mut().as_mut(),
            Self::FragmentSpread(spread) => Some(spread.get_selection_set_mut()),
            Self::InlineFragment(inline) => Some(inline.get_selection_set_mut()),
        }
    }
}

#[derive(Debug)]
pub(crate) struct FieldSelectionValue<'a>(&'a mut Arc<FieldSelection>);

impl<'a> FieldSelectionValue<'a> {
    pub(crate) fn new(field_selection: &'a mut Arc<FieldSelection>) -> Self {
        Self(field_selection)
    }

    pub(crate) fn get(&self) -> &Arc<FieldSelection> {
        self.0
    }

    pub(crate) fn get_sibling_typename_mut(&mut self) -> &mut Option<SiblingTypename> {
        Arc::make_mut(self.0).field.sibling_typename_mut()
    }

    pub(crate) fn get_selection_set_mut(&mut self) -> &mut Option<SelectionSet> {
        &mut Arc::make_mut(self.0).selection_set
    }
}

#[derive(Debug)]
pub(crate) struct FragmentSpreadSelectionValue<'a>(&'a mut Arc<FragmentSpreadSelection>);

impl<'a> FragmentSpreadSelectionValue<'a> {
    pub(crate) fn new(fragment_spread_selection: &'a mut Arc<FragmentSpreadSelection>) -> Self {
        Self(fragment_spread_selection)
    }

    pub(crate) fn get_selection_set_mut(&mut self) -> &mut SelectionSet {
        &mut Arc::make_mut(self.0).selection_set
    }

    pub(crate) fn get(&self) -> &Arc<FragmentSpreadSelection> {
        self.0
    }
}

#[derive(Debug)]
pub(crate) struct InlineFragmentSelectionValue<'a>(&'a mut Arc<InlineFragmentSelection>);

impl<'a> InlineFragmentSelectionValue<'a> {
    pub(crate) fn new(inline_fragment_selection: &'a mut Arc<InlineFragmentSelection>) -> Self {
        Self(inline_fragment_selection)
    }

    pub(crate) fn get(&self) -> &Arc<InlineFragmentSelection> {
        self.0
    }

    pub(crate) fn get_selection_set_mut(&mut self) -> &mut SelectionSet {
        &mut Arc::make_mut(self.0).selection_set
    }
}

pub(crate) enum Entry<'a> {
    Occupied(OccupiedEntry<'a>),
    Vacant(VacantEntry<'a>),
}

impl<'a> Entry<'a> {
    pub fn or_insert(
        self,
        produce: impl FnOnce() -> Result<Selection, FederationError>,
    ) -> Result<SelectionValue<'a>, FederationError> {
        match self {
            Self::Occupied(entry) => Ok(entry.into_mut()),
            Self::Vacant(entry) => entry.insert(produce()?),
        }
    }
}

pub(crate) struct OccupiedEntry<'a>(indexmap::map::OccupiedEntry<'a, SelectionKey, Selection>);

impl<'a> OccupiedEntry<'a> {
    pub(crate) fn get(&self) -> &Selection {
        self.0.get()
    }

    pub(crate) fn get_mut(&mut self) -> SelectionValue {
        SelectionValue::new(self.0.get_mut())
    }

    pub(crate) fn into_mut(self) -> SelectionValue<'a> {
        SelectionValue::new(self.0.into_mut())
    }

    pub(crate) fn key(&self) -> &SelectionKey {
        self.0.key()
    }

    pub(crate) fn remove(self) -> Selection {
        // We specifically use shift_remove() instead of swap_remove() to maintain order.
        self.0.shift_remove()
    }
}

pub(crate) struct VacantEntry<'a>(indexmap::map::VacantEntry<'a, SelectionKey, Selection>);

impl<'a> VacantEntry<'a> {
    pub(crate) fn key(&self) -> &SelectionKey {
        self.0.key()
    }

    pub(crate) fn insert(
        self,
        value: Selection,
    ) -> Result<SelectionValue<'a>, FederationError> {
        if *self.key() != value.key() {
            return Err(Internal {
                message: format!(
                    "Key mismatch when inserting selection {} into vacant entry ",
                    value
                ),
            }
            .into());
        }
        Ok(SelectionValue::new(self.0.insert(value)))
    }
}

impl IntoIterator for SelectionMap {
    type Item = <IndexMap<SelectionKey, Selection> as IntoIterator>::Item;
    type IntoIter = <IndexMap<SelectionKey, Selection> as IntoIterator>::IntoIter;

    fn into_iter(self) -> Self::IntoIter {
        <IndexMap<SelectionKey, Selection> as IntoIterator>::into_iter(self.0)
    }
}

