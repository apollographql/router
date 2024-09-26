use std::borrow::Cow;
use std::hash::BuildHasher;
use std::sync::Arc;

use apollo_compiler::Name;
use hashbrown::DefaultHashBuilder;
use hashbrown::HashTable;
use serde::ser::SerializeSeq;
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

#[derive(Clone)]
struct Bucket {
    index: usize,
    hash: u64,
}

/// A selection map is the underlying representation of a selection set. It contains an ordered
/// list of selections with unique selection keys. Selections with the same key should be merged
/// together by the user of this structure: the selection map API itself will overwrite selections
/// with the same key.
///
/// Because the key depends strictly on the value, we expose the underlying map's API in a
/// read-only capacity, while mutations use an API closer to `IndexSet`. We don't just use an
/// `IndexSet` since key computation is expensive (it involves sorting). This type is in its own
/// module to prevent code from accidentally mutating the underlying map outside the mutation
/// API.
#[derive(Clone)]
pub(crate) struct SelectionMap {
    hash_builder: DefaultHashBuilder,
    table: HashTable<Bucket>,
    selections: Vec<Selection>,
}

impl std::fmt::Debug for SelectionMap {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_set().entries(self.values()).finish()
    }
}

impl PartialEq for SelectionMap {
    /// Compare two selection maps. This is order independent.
    fn eq(&self, other: &Self) -> bool {
        self.len() == other.len()
            && self
                .values()
                .all(|left| other.get(&left.key()).is_some_and(|right| left == right))
    }
}

impl Eq for SelectionMap {}

impl Serialize for SelectionMap {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let mut seq = serializer.serialize_seq(Some(self.len()))?;
        for value in self.values() {
            seq.serialize_element(value)?;
        }
        seq.end()
    }
}

impl Default for SelectionMap {
    fn default() -> Self {
        Self::new()
    }
}

pub(crate) type Values<'a> = std::slice::Iter<'a, Selection>;
pub(crate) type ValuesMut<'a> = std::slice::IterMut<'a, Selection>;
pub(crate) type IntoValues = std::vec::IntoIter<Selection>;

/// Return an equality function taking an index into `selections` and returning if the index
/// matches the given key.
///
/// The returned function panics if the index is out of bounds.
fn key_eq<'a>(selections: &'a [Selection], key: &SelectionKey) -> impl Fn(&Bucket) -> bool + 'a {
    move |bucket| selections[bucket.index].key() == *key
}

impl SelectionMap {
    pub(crate) fn new() -> Self {
        SelectionMap {
            hash_builder: Default::default(),
            table: HashTable::new(),
            selections: Vec::new(),
        }
    }

    /// Returns the number of selections in the map.
    pub(crate) fn len(&self) -> usize {
        self.selections.len()
    }

    /// Returns true if there are no selections in the map.
    pub(crate) fn is_empty(&self) -> bool {
        self.selections.is_empty()
    }

    /// Returns the first selection in the map, or None if the map is empty.
    pub(crate) fn first(&self) -> Option<&Selection> {
        self.selections.first()
    }

    /// Computes the hash of a selection key.
    fn hash(&self, key: SelectionKey) -> u64 {
        self.hash_builder.hash_one(key)
    }

    /// Returns true if the given key exists in the map.
    pub(crate) fn contains_key(&self, key: &SelectionKey) -> bool {
        let hash = self.hash(key);
        self.table
            .find(hash, key_eq(&self.selections, key))
            .is_some()
    }

    /// Returns true if the given key exists in the map.
    pub(crate) fn get(&self, key: &SelectionKey) -> Option<&Selection> {
        let hash = self.hash(key);
        let bucket = self.table.find(hash, key_eq(&self.selections, key))?;
        Some(&self.selections[bucket.index])
    }

    pub(crate) fn get_mut(&mut self, key: &SelectionKey) -> Option<SelectionValue<'_>> {
        let hash = self.hash(key);
        let bucket = self.table.find_mut(hash, key_eq(&self.selections, key))?;
        Some(SelectionValue::new(&mut self.selections[bucket.index]))
    }

    /// Insert a selection into the map.
    fn raw_insert(&mut self, hash: u64, value: Selection) -> &mut Selection {
        let index = self.selections.len();

        // `insert` overwrites a selection without running `Drop`: because
        // we only store an integer which is `Copy`, this works out fine
        self.table.insert_unique(hash, Bucket { index, hash }, |existing| existing.hash);

        self.selections.push(value);
        &mut self.selections[index]
    }

    /// Resets and rebuilds the hash table.
    ///
    /// Preconditions:
    /// - The table must have enough capacity for `self.selections.len()` elements.
    fn rebuild_table_no_grow(&mut self) {
        assert!(self.table.capacity() >= self.selections.len());
        self.table.clear();
        for (index, selection) in self.selections.iter().enumerate() {
            let hash = self.hash(selection.key());
            self.table.insert_unique(hash, Bucket { index, hash }, |existing| existing.hash);
        }
    }

    /// Decrements all the indices in the table starting at `pivot`.
    fn decrement_table(&mut self, pivot: usize) {
        for bucket in self.table.iter_mut() {
            if bucket.index >= pivot {
                bucket.index -= 1;
            }
        }
    }

    pub(crate) fn insert(&mut self, value: Selection) {
        let hash = self.hash(&value.key());
        self.raw_insert(hash, value);
    }

    /// Remove a selection from the map. Returns the selection and its numeric index.
    pub(crate) fn remove(&mut self, key: &SelectionKey) -> Option<(usize, Selection)> {
        let hash = self.hash(key);
        let entry = self
            .table
            .find_entry(hash, key_eq(&self.selections, key))
            .ok()?;
        let (bucket, _) = entry.remove();
        let selection = self.selections.remove(bucket.index);
        self.decrement_table(bucket.index);
        Some((bucket.index, selection))
    }

    pub(crate) fn retain(&mut self, mut predicate: impl FnMut(&SelectionKey, &Selection) -> bool) {
        self.selections.retain(|selection| {
            let key = selection.key();
            predicate(&key, selection)
        });
        if self.selections.len() < self.table.len() {
            // In theory, we could track which keys were removed, and adjust the indices based on
            // that, but it's very tricky and it might not even be faster than just resetting the
            // whole map.
            self.rebuild_table_no_grow();
        }
        assert!(self.selections.len() == self.table.len());
    }

    /// Iterate over all selections.
    pub(crate) fn values(&self) -> std::slice::Iter<'_, Selection> {
        self.selections.iter()
    }

    /// Iterate over all selections.
    pub(crate) fn values_mut(&mut self) -> impl Iterator<Item = SelectionValue<'_>> {
        self.selections.iter_mut().map(SelectionValue::new)
    }

    /// Iterate over all selections.
    pub(crate) fn into_values(self) -> std::vec::IntoIter<Selection> {
        self.selections.into_iter()
    }

    pub(super) fn entry<'a>(&'a mut self, key: &SelectionKey) -> Entry<'a> {
        let hash = self.hash(key);
        let slot = self.table.find_entry(hash, key_eq(&self.selections, key));
        match slot {
            Ok(occupied) => {
                let index = occupied.get().index;
                let selection = &mut self.selections[index];
                Entry::Occupied(OccupiedEntry(selection))
            }
            Err(vacant) => Entry::Vacant(VacantEntry {
                map: self,
                hash,
                key,
            }),
        }
    }

    pub(crate) fn extend(&mut self, other: SelectionMap) {
        for selection in other.into_values() {
            self.insert(selection);
        }
    }

    pub(crate) fn extend_ref(&mut self, other: &SelectionMap) {
        for selection in other.values() {
            self.insert(selection.clone());
        }
    }

    pub(crate) fn as_slice(&self) -> &[Selection] {
        &self.selections
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
                            Cow::Owned(new) => {
                                Cow::Owned(Selection::from_field(field.field.clone(), Some(new)))
                            }
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
                    Cow::Owned(selection_set) => Cow::Owned(Selection::InlineFragment(Arc::new(
                        InlineFragmentSelection::new(
                            fragment.inline_fragment.clone(),
                            selection_set,
                        ),
                    ))),
                },
                Selection::FragmentSpread(_) => {
                    return Err(FederationError::internal("unexpected fragment spread"))
                }
            })
        }
        let mut iter = self.values();
        let mut enumerated = (&mut iter).enumerate();
        let mut new_map: Self;
        loop {
            let Some((index, selection)) = enumerated.next() else {
                return Ok(Cow::Borrowed(self));
            };
            let filtered = recur_sub_selections(selection, predicate)?;
            let keep = predicate(&filtered)?;
            if keep && matches!(filtered, Cow::Borrowed(_)) {
                // Nothing changed so far, continue without cloning
                continue;
            }

            // Clone the map so far
            new_map = self.as_slice()[..index].iter().cloned().collect();

            if keep {
                new_map.insert(filtered.into_owned());
            }
            break;
        }
        for selection in iter {
            let filtered = recur_sub_selections(selection, predicate)?;
            if predicate(&filtered)? {
                new_map.insert(filtered.into_owned());
            }
        }
        Ok(Cow::Owned(new_map))
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
    fn new(selection: &'a mut Selection) -> Self {
        match selection {
            Selection::Field(field_selection) => {
                SelectionValue::Field(FieldSelectionValue::new(field_selection))
            }
            Selection::FragmentSpread(fragment_spread_selection) => SelectionValue::FragmentSpread(
                FragmentSpreadSelectionValue::new(fragment_spread_selection),
            ),
            Selection::InlineFragment(inline_fragment_selection) => SelectionValue::InlineFragment(
                InlineFragmentSelectionValue::new(inline_fragment_selection),
            ),
        }
    }

    pub(super) fn key(&self) -> SelectionKey {
        match self {
            Self::Field(field) => field.get().key(),
            Self::FragmentSpread(frag) => frag.get().key(),
            Self::InlineFragment(frag) => frag.get().key(),
        }
    }

    pub(super) fn get_selection_set_mut(&mut self) -> Option<&'_ mut SelectionSet> {
        match self {
            SelectionValue::Field(field) => field.get_selection_set_mut(),
            SelectionValue::FragmentSpread(frag) => Some(frag.get_selection_set_mut()),
            SelectionValue::InlineFragment(frag) => Some(frag.get_selection_set_mut()),
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

    pub(crate) fn get_selection_set_mut(&mut self) -> Option<&mut SelectionSet> {
        Arc::make_mut(self.0).selection_set.as_mut()
    }
}

#[derive(Debug)]
pub(crate) struct FragmentSpreadSelectionValue<'a>(&'a mut Arc<FragmentSpreadSelection>);

impl<'a> FragmentSpreadSelectionValue<'a> {
    pub(crate) fn new(fragment_spread_selection: &'a mut Arc<FragmentSpreadSelection>) -> Self {
        Self(fragment_spread_selection)
    }

    pub(crate) fn get(&self) -> &Arc<FragmentSpreadSelection> {
        self.0
    }

    pub(crate) fn get_selection_set_mut(&mut self) -> &mut SelectionSet {
        &mut Arc::make_mut(self.0).selection_set
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
    pub(crate) fn or_insert(
        self,
        produce: impl FnOnce() -> Result<Selection, FederationError>,
    ) -> Result<SelectionValue<'a>, FederationError> {
        match self {
            Self::Occupied(entry) => Ok(entry.into_mut()),
            Self::Vacant(entry) => entry.insert(produce()?),
        }
    }
}

pub(crate) struct OccupiedEntry<'a>(&'a mut Selection);

impl<'a> OccupiedEntry<'a> {
    pub(crate) fn get(&self) -> &Selection {
        self.0
    }

    pub(crate) fn into_mut(self) -> SelectionValue<'a> {
        SelectionValue::new(self.0)
    }
}

pub(crate) struct VacantEntry<'a> {
    map: &'a mut SelectionMap,
    hash: u64,
    key: SelectionKey,
}

impl<'a> VacantEntry<'a> {
    pub(crate) fn key(&self) -> &SelectionKey {
        &self.key
    }

    pub(crate) fn insert(self, value: Selection) -> Result<SelectionValue<'a>, FederationError> {
        if *self.key() != value.key() {
            return Err(Internal {
                message: format!(
                    "Key mismatch when inserting selection {} into vacant entry ",
                    value
                ),
            }
            .into());
        };
        Ok(SelectionValue::new(self.map.raw_insert(self.hash, value)))
    }
}
