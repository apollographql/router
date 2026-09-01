use std::borrow::Cow;
use std::hash::BuildHasher;
use std::hash::Hash;
use std::hash::Hasher;
use std::sync::Arc;

use apollo_compiler::Name;
use hashbrown::DefaultHashBuilder;
use hashbrown::HashTable;
use itertools::Itertools;
use serde::Serialize;
use serde::ser::SerializeSeq;

use crate::error::FederationError;
use crate::operation::DirectiveList;
use crate::operation::Selection;
use crate::operation::SelectionId;
use crate::operation::SelectionSet;
use crate::operation::SiblingTypename;
use crate::operation::field_selection::FieldSelection;
use crate::operation::inline_fragment_selection::InlineFragmentSelection;

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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
pub(crate) enum SelectionKey<'a> {
    Field {
        /// The field alias (if specified) or field name in the resulting selection set.
        response_name: &'a Name,
        /// directives applied on the field
        directives: &'a DirectiveList,
    },
    FragmentSpread {
        /// The name of the fragment.
        fragment_name: &'a Name,
        /// Directives applied on the fragment spread (does not contain @defer).
        directives: &'a DirectiveList,
    },
    InlineFragment {
        /// The optional type condition of the fragment.
        type_condition: Option<&'a Name>,
        /// Directives applied on the fragment spread (does not contain @defer).
        directives: &'a DirectiveList,
    },
    Defer {
        /// Unique selection ID used to distinguish deferred fragment spreads that cannot be merged.
        deferred_id: SelectionId,
    },
}

impl SelectionKey<'_> {
    /// Get an owned structure representing the selection key, for use in map keys
    /// that are not a plain selection map.
    pub(crate) fn to_owned_key(self) -> OwnedSelectionKey {
        match self {
            Self::Field {
                response_name,
                directives,
            } => OwnedSelectionKey::Field {
                response_name: response_name.clone(),
                directives: directives.clone(),
            },
            Self::FragmentSpread {
                fragment_name,
                directives,
            } => OwnedSelectionKey::FragmentSpread {
                fragment_name: fragment_name.clone(),
                directives: directives.clone(),
            },
            Self::InlineFragment {
                type_condition,
                directives,
            } => OwnedSelectionKey::InlineFragment {
                type_condition: type_condition.cloned(),
                directives: directives.clone(),
            },
            Self::Defer { deferred_id } => OwnedSelectionKey::Defer { deferred_id },
        }
    }
}

/// An owned structure representing the selection key, for use in map keys
/// that are not a plain selection map.
#[derive(Debug, Clone, Hash, PartialEq, Eq)]
pub(crate) enum OwnedSelectionKey {
    Field {
        response_name: Name,
        directives: DirectiveList,
    },
    FragmentSpread {
        fragment_name: Name,
        directives: DirectiveList,
    },
    InlineFragment {
        type_condition: Option<Name>,
        directives: DirectiveList,
    },
    Defer {
        deferred_id: SelectionId,
    },
}

impl OwnedSelectionKey {
    /// Get a plain, borrowed selection key, that can be used for indexing into a selection map.
    pub(crate) fn as_borrowed_key(&self) -> SelectionKey<'_> {
        match self {
            OwnedSelectionKey::Field {
                response_name,
                directives,
            } => SelectionKey::Field {
                response_name,
                directives,
            },
            OwnedSelectionKey::FragmentSpread {
                fragment_name,
                directives,
            } => SelectionKey::FragmentSpread {
                fragment_name,
                directives,
            },
            OwnedSelectionKey::InlineFragment {
                type_condition,
                directives,
            } => SelectionKey::InlineFragment {
                type_condition: type_condition.as_ref(),
                directives,
            },
            OwnedSelectionKey::Defer { deferred_id } => SelectionKey::Defer {
                deferred_id: *deferred_id,
            },
        }
    }
}

#[cfg(test)]
impl<'a> SelectionKey<'a> {
    /// Create a selection key for a specific field name.
    ///
    /// This is available for tests only as selection keys should not normally be created outside of
    /// `HasSelectionKey::key`.
    pub(crate) fn field_name(name: &'a Name) -> Self {
        static EMPTY_LIST: DirectiveList = DirectiveList::new();
        SelectionKey::Field {
            response_name: name,
            directives: &EMPTY_LIST,
        }
    }
}

pub(crate) trait HasSelectionKey {
    fn key(&self) -> SelectionKey<'_>;
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
/// Once a selection is in the selection map, it must not be modified in a way that changes the
/// selection key. Therefore, the selection map only hands out mutable access through the
/// SelectionValue types, which expose the parts of selections that are safe to modify.
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
                .all(|left| other.get(left.key()).is_some_and(|right| left == right))
    }
}

impl Eq for SelectionMap {}

impl Hash for SelectionMap {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.values()
            .sorted()
            .for_each(|hash_key| hash_key.hash(state));
    }
}

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
pub(crate) type ValuesMut<'a> =
    std::iter::Map<std::slice::IterMut<'a, Selection>, fn(&'a mut Selection) -> SelectionValue<'a>>;
pub(crate) type IntoValues = std::vec::IntoIter<Selection>;

/// Return an equality function taking an index into `selections` and returning if the index
/// matches the given key.
///
/// The returned function panics if the index is out of bounds.
fn key_eq(selections: &[Selection], key: SelectionKey<'_>) -> impl Fn(&Bucket) -> bool {
    move |bucket| selections[bucket.index].key() == key
}

impl SelectionMap {
    /// Create an empty selection map.
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

    /// Computes the hash of a selection key.
    fn hash_key(&self, key: SelectionKey<'_>) -> u64 {
        self.hash_builder.hash_one(key)
    }

    /// Returns true if the given key exists in the map.
    pub(crate) fn contains_key(&self, key: SelectionKey<'_>) -> bool {
        let hash = self.hash_key(key);
        self.table
            .find(hash, key_eq(&self.selections, key))
            .is_some()
    }

    /// Returns true if the given key exists in the map.
    pub(crate) fn get(&self, key: SelectionKey<'_>) -> Option<&Selection> {
        let hash = self.hash_key(key);
        let bucket = self.table.find(hash, key_eq(&self.selections, key))?;
        Some(&self.selections[bucket.index])
    }

    pub(crate) fn get_mut(&mut self, key: SelectionKey<'_>) -> Option<SelectionValue<'_>> {
        let hash = self.hash_key(key);
        let bucket = self.table.find_mut(hash, key_eq(&self.selections, key))?;
        Some(SelectionValue::new(&mut self.selections[bucket.index]))
    }

    /// Insert a selection into the map.
    fn raw_insert(&mut self, hash: u64, value: Selection) -> &mut Selection {
        let index = self.selections.len();

        self.table
            .insert_unique(hash, Bucket { index, hash }, |existing| existing.hash);

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
            let hash = self.hash_key(selection.key());
            self.table
                .insert_unique(hash, Bucket { index, hash }, |existing| existing.hash);
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

    /// Insert a selection into the map. If a selection with an equal key is already present, it
    /// is overwritten in place (keeping its original position); otherwise the new selection is
    /// appended.
    pub(crate) fn insert(&mut self, value: Selection) {
        let key = value.key();
        let hash = self.hash_key(key);
        if let Some(bucket) = self.table.find_mut(hash, key_eq(&self.selections, key)) {
            self.selections[bucket.index] = value;
        } else {
            self.raw_insert(hash, value);
        }
    }

    /// Remove a selection from the map. Returns the selection and its numeric index.
    pub(crate) fn remove(&mut self, key: SelectionKey<'_>) -> Option<(usize, Selection)> {
        let hash = self.hash_key(key);
        let entry = self
            .table
            .find_entry(hash, key_eq(&self.selections, key))
            .ok()?;
        let (bucket, _) = entry.remove();
        let selection = self.selections.remove(bucket.index);
        self.decrement_table(bucket.index);
        Some((bucket.index, selection))
    }

    pub(crate) fn retain(
        &mut self,
        mut predicate: impl FnMut(SelectionKey<'_>, &Selection) -> bool,
    ) {
        self.selections.retain(|selection| {
            let key = selection.key();
            predicate(key, selection)
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
    pub(crate) fn values(&self) -> Values<'_> {
        self.selections.iter()
    }

    /// Iterate over all selections.
    pub(crate) fn values_mut(&mut self) -> ValuesMut<'_> {
        self.selections.iter_mut().map(SelectionValue::new)
    }

    /// Iterate over all selections.
    pub(crate) fn into_values(self) -> IntoValues {
        self.selections.into_iter()
    }

    /// Provides mutable access to a selection key. A new selection can be inserted or an existing
    /// selection modified.
    pub(super) fn entry<'a>(&'a mut self, key: SelectionKey<'a>) -> Entry<'a> {
        let hash = self.hash_key(key);
        let slot = self.table.find_entry(hash, key_eq(&self.selections, key));
        match slot {
            Ok(occupied) => {
                let index = occupied.get().index;
                let selection = &mut self.selections[index];
                Entry::Occupied(OccupiedEntry(selection))
            }
            // We're not using `hashbrown`'s VacantEntry API here, because we have some custom
            // insertion logic, it's easier to use `SelectionMap::raw_insert` to implement
            // `VacantEntry::or_insert`.
            Err(_) => Entry::Vacant(VacantEntry {
                map: self,
                hash,
                key,
            }),
        }
    }

    /// Add selections from another selection map to this one. If there are key collisions, the
    /// selections are *overwritten*.
    pub(crate) fn extend(&mut self, other: SelectionMap) {
        for selection in other.into_values() {
            self.insert(selection);
        }
    }

    /// Add selections from another selection map to this one. If there are key collisions, the
    /// selections are *overwritten*.
    pub(crate) fn extend_ref(&mut self, other: &SelectionMap) {
        for selection in other.values() {
            self.insert(selection.clone());
        }
    }

    /// Returns the selection set resulting from "recursively" filtering any selection
    /// that does not match the provided predicate.
    /// This method calls `predicate` on every selection of the selection set,
    /// not just top-level ones, and apply a "depth-first" strategy:
    /// when the predicate is called on a given selection it is guaranteed that
    /// filtering has happened on all the selections of its sub-selection.
    pub(crate) fn filter_recursive_depth_first(
        &self,
        predicate: &mut dyn FnMut(&Selection) -> bool,
    ) -> Cow<'_, Self> {
        fn recur_sub_selections<'sel>(
            selection: &'sel Selection,
            predicate: &mut dyn FnMut(&Selection) -> bool,
        ) -> Cow<'sel, Selection> {
            match selection {
                Selection::Field(field) => {
                    if let Some(sub_selections) = &field.selection_set {
                        match sub_selections.filter_recursive_depth_first(predicate) {
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
                    .filter_recursive_depth_first(predicate)
                {
                    Cow::Borrowed(_) => Cow::Borrowed(selection),
                    Cow::Owned(selection_set) => Cow::Owned(Selection::InlineFragment(Arc::new(
                        InlineFragmentSelection::new(
                            fragment.inline_fragment.clone(),
                            selection_set,
                        ),
                    ))),
                },
            }
        }
        let mut iter = self.values();
        let mut enumerated = (&mut iter).enumerate();
        let mut new_map: Self;
        loop {
            let Some((index, selection)) = enumerated.next() else {
                return Cow::Borrowed(self);
            };
            let filtered = recur_sub_selections(selection, predicate);
            let keep = predicate(&filtered);
            if keep && matches!(filtered, Cow::Borrowed(_)) {
                // Nothing changed so far, continue without cloning
                continue;
            }

            // Clone the map so far
            new_map = self.selections[..index].iter().cloned().collect();

            if keep {
                new_map.insert(filtered.into_owned());
            }
            break;
        }
        for selection in iter {
            let filtered = recur_sub_selections(selection, predicate);
            if predicate(&filtered) {
                new_map.insert(filtered.into_owned());
            }
        }
        Cow::Owned(new_map)
    }
}

impl<A> FromIterator<A> for SelectionMap
where
    A: Into<Selection>,
{
    /// Create a selection map from an iterator of selections. On key collisions, *only the later
    /// selection is used*.
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
    InlineFragment(InlineFragmentSelectionValue<'a>),
}

impl<'a> SelectionValue<'a> {
    fn new(selection: &'a mut Selection) -> Self {
        match selection {
            Selection::Field(field_selection) => {
                SelectionValue::Field(FieldSelectionValue::new(field_selection))
            }
            Selection::InlineFragment(inline_fragment_selection) => SelectionValue::InlineFragment(
                InlineFragmentSelectionValue::new(inline_fragment_selection),
            ),
        }
    }

    pub(super) fn key(&self) -> SelectionKey<'_> {
        match self {
            Self::Field(field) => field.get().key(),
            Self::InlineFragment(frag) => frag.get().key(),
        }
    }

    // This is used in operation::optimize tests
    #[cfg(test)]
    pub(super) fn get_selection_set_mut(&mut self) -> Option<&mut SelectionSet> {
        match self {
            SelectionValue::Field(field) => field.get_selection_set_mut(),
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
    key: SelectionKey<'a>,
}

impl<'a> VacantEntry<'a> {
    pub(crate) fn key(&self) -> SelectionKey<'a> {
        self.key
    }

    pub(crate) fn insert(self, value: Selection) -> Result<SelectionValue<'a>, FederationError> {
        if self.key() != value.key() {
            return Err(FederationError::internal(format!(
                "Key mismatch when inserting selection {value} into vacant entry "
            )));
        };
        Ok(SelectionValue::new(self.map.raw_insert(self.hash, value)))
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use proptest::prelude::*;

    use super::*;
    use crate::operation::tests::parse_operation;
    use crate::operation::tests::parse_schema;

    const SCHEMA: &str = r#"
        type Query {
            a: T
            b: T
            c: T
        }

        type T {
            x: Int
            y: Int
        }
    "#;

    fn selection_from_source(
        schema: &crate::schema::ValidFederationSchema,
        source: &str,
    ) -> Selection {
        let query = format!("{{ {source} }}");
        let operation = parse_operation(schema, &query);
        operation
            .selection_set
            .selections
            .values()
            .next()
            .expect("selection source must produce exactly one top-level selection")
            .clone()
    }

    /// Minimized regression for the bug found by `selection_map_matches_last_write_wins_model`:
    /// inserting a selection under a key that's already present used to append a second entry
    /// instead of overwriting the first, in violation of the documented last-write-wins contract.
    #[test]
    fn insert_overwrites_equal_key_without_changing_position() {
        let schema = parse_schema(SCHEMA);
        let mut map = SelectionMap::new();

        map.insert(selection_from_source(&schema, "a { x }"));
        map.insert(selection_from_source(&schema, "b { x }"));
        map.insert(selection_from_source(&schema, "b { y }"));

        assert_eq!(
            map.len(),
            2,
            "the second `b` insertion should overwrite the first"
        );
        let values: Vec<&Selection> = map.values().collect();
        assert_eq!(values[0], &selection_from_source(&schema, "a { x }"));
        assert_eq!(
            values[1],
            &selection_from_source(&schema, "b { y }"),
            "the surviving `b` entry should hold the last-written value, in its original position"
        );
    }

    /// A slow, obviously-correct model of `SelectionMap`'s documented last-write-wins overwrite
    /// semantics: a key that already exists keeps its position but gets the new value.
    fn model_insert(model: &mut Vec<(OwnedSelectionKey, Selection)>, value: Selection) {
        let key = value.key().to_owned_key();
        if let Some(existing) = model.iter_mut().find(|(k, _)| *k == key) {
            existing.1 = value;
        } else {
            model.push((key, value));
        }
    }

    fn response_name() -> impl Strategy<Value = &'static str> {
        prop_oneof![Just("a"), Just("b"), Just("c")]
    }

    fn key_name() -> impl Strategy<Value = &'static str> {
        prop_oneof![Just("a"), Just("b"), Just("c"), Just("z")]
    }

    fn sub_selection() -> impl Strategy<Value = &'static str> {
        prop_oneof![Just("x"), Just("y"), Just("x y")]
    }

    fn selection_source() -> impl Strategy<Value = String> {
        (response_name(), sub_selection()).prop_map(|(name, sub)| format!("{name} {{ {sub} }}"))
    }

    #[derive(Debug, Clone)]
    enum Command {
        Insert(String),
        Get(String),
        Remove(String),
        Retain(Vec<String>),
        Extend(Vec<String>),
    }

    fn command() -> impl Strategy<Value = Command> {
        prop_oneof![
            3 => selection_source().prop_map(Command::Insert),
            2 => key_name().prop_map(|n| Command::Get(n.to_string())),
            2 => key_name().prop_map(|n| Command::Remove(n.to_string())),
            1 => prop::collection::vec(key_name(), 0..3)
                .prop_map(|names| Command::Retain(names.into_iter().map(String::from).collect())),
            2 => prop::collection::vec(selection_source(), 0..3).prop_map(Command::Extend),
        ]
    }

    /// Compares production state against the model: length, iteration order and content,
    /// structural (order-independent) equality against a freshly reconstructed map, and
    /// lookups for every name in the small generated key universe.
    fn assert_map_matches_model(map: &SelectionMap, model: &[(OwnedSelectionKey, Selection)]) {
        let actual: Vec<&Selection> = map.values().collect();
        let expected: Vec<&Selection> = model.iter().map(|(_, value)| value).collect();
        assert_eq!(actual, expected, "iteration order/content mismatch");
        assert_eq!(map.len(), model.len());
        assert_eq!(map.is_empty(), model.is_empty());

        let rebuilt: SelectionMap = model.iter().map(|(_, value)| value.clone()).collect();
        assert_eq!(
            map, &rebuilt,
            "map should equal a freshly reconstructed map"
        );

        for name in ["a", "b", "c", "z"] {
            let name = Name::new(name).unwrap();
            let key = SelectionKey::field_name(&name);
            let owned_key = key.to_owned_key();
            let expected = model
                .iter()
                .find(|(k, _)| *k == owned_key)
                .map(|(_, value)| value);
            assert_eq!(map.get(key), expected, "lookup mismatch for {name}");
            assert_eq!(map.contains_key(key), expected.is_some());
        }
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(256))]

        #[test]
        fn selection_map_matches_last_write_wins_model(commands in prop::collection::vec(command(), 0..40)) {
            let schema = parse_schema(SCHEMA);
            let mut map = SelectionMap::new();
            let mut model: Vec<(OwnedSelectionKey, Selection)> = Vec::new();

            for command in commands {
                match command {
                    Command::Insert(source) => {
                        let selection = selection_from_source(&schema, &source);
                        map.insert(selection.clone());
                        model_insert(&mut model, selection);
                    }
                    Command::Get(name) => {
                        let name = Name::new(&name).unwrap();
                        let key = SelectionKey::field_name(&name);
                        let owned_key = key.to_owned_key();
                        let expected = model
                            .iter()
                            .find(|(k, _)| *k == owned_key)
                            .map(|(_, value)| value.clone());
                        prop_assert_eq!(map.get(key).cloned(), expected);
                    }
                    Command::Remove(name) => {
                        let name = Name::new(&name).unwrap();
                        let key = SelectionKey::field_name(&name);
                        let owned_key = key.to_owned_key();
                        let model_index = model.iter().position(|(k, _)| *k == owned_key);
                        let removed = map.remove(key);
                        match (removed, model_index) {
                            (Some((index, selection)), Some(model_index)) => {
                                prop_assert_eq!(index, model_index);
                                let (_, model_selection) = model.remove(model_index);
                                prop_assert_eq!(selection, model_selection);
                            }
                            (None, None) => {}
                            (actual, expected_index) => {
                                prop_assert!(
                                    false,
                                    "remove presence mismatch: actual={:?} expected_index={:?}",
                                    actual,
                                    expected_index
                                );
                            }
                        }
                    }
                    Command::Retain(names) => {
                        let keep: HashSet<String> = names.into_iter().collect();
                        map.retain(|key, _| match key {
                            SelectionKey::Field { response_name, .. } => {
                                keep.contains(response_name.as_str())
                            }
                            _ => false,
                        });
                        model.retain(|(key, _)| match key {
                            OwnedSelectionKey::Field { response_name, .. } => {
                                keep.contains(response_name.as_str())
                            }
                            _ => false,
                        });
                    }
                    Command::Extend(sources) => {
                        let selections: Vec<Selection> = sources
                            .iter()
                            .map(|source| selection_from_source(&schema, source))
                            .collect();
                        let other: SelectionMap = selections.iter().cloned().collect();
                        map.extend(other);
                        for selection in selections {
                            model_insert(&mut model, selection);
                        }
                    }
                }

                assert_map_matches_model(&map, &model);
            }
        }
    }
}
