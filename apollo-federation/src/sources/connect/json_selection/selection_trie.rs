use std::fmt::Display;
use std::hash::Hash;
use std::hash::Hasher;
use std::ops::Range;

use apollo_compiler::collections::IndexMap;
use apollo_compiler::collections::IndexSet;

use super::Key;
use super::NamedSelection;
use super::PathList;
use super::Ranged;
use super::SubSelection;
use super::helpers::quote_if_necessary;
use super::location::WithRange;

impl PathList {
    pub(crate) fn compute_selection_trie(&self) -> SelectionTrie {
        let mut trie = SelectionTrie::new();
        trie.add_path_list(self);
        trie
    }
}

type Ref<T> = std::sync::Arc<T>;

#[derive(Debug, Eq, Clone)]
pub(crate) struct SelectionTrie {
    /// The top-level sub-selections of this [`SelectionTrie`].
    selections: IndexMap<String, Ref<SelectionTrie>>,

    /// Whether the path terminating at this [`SelectionTrie`] node was
    /// explicitly added to the trie.
    used: bool,

    /// Collected as metadata but ignored by [`PartialEq`] and [`Hash`].
    key_ranges: IndexMap<String, IndexSet<Range<usize>>>,
}

impl Display for SelectionTrie {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut need_space = false;

        for (key, sub) in self.selections.iter() {
            if need_space {
                write!(f, " ")?;
            }

            if sub.is_empty() {
                if sub.is_used() {
                    write!(f, "{}", quote_if_necessary(key))?;
                    need_space = true;
                }
            } else {
                write!(f, "{} {{ {} }}", quote_if_necessary(key), sub)?;
                need_space = true;
            }
        }

        Ok(())
    }
}

impl PartialEq for SelectionTrie {
    fn eq(&self, other: &Self) -> bool {
        self.used == other.used && self.selections == other.selections
    }
}

impl Hash for SelectionTrie {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.selections
            .iter()
            .fold(0, |acc, (key, sub)| {
                let mut hasher = std::hash::DefaultHasher::default();
                (key, sub).hash(&mut hasher);
                acc ^ hasher.finish()
            })
            .hash(state);
    }
}

impl SelectionTrie {
    pub(crate) fn new() -> Self {
        Self {
            used: false,
            selections: IndexMap::default(),
            key_ranges: IndexMap::default(),
        }
    }

    #[allow(dead_code)]
    pub(crate) fn new_used() -> Self {
        Self {
            used: true,
            selections: IndexMap::default(),
            key_ranges: IndexMap::default(),
        }
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.selections.is_empty()
    }

    pub(crate) fn keys(&self) -> impl Iterator<Item = &String> {
        self.selections.keys()
    }

    pub(crate) fn get(&self, key: impl Into<String>) -> Option<&SelectionTrie> {
        self.selections.get(&key.into()).map(|sub| sub.as_ref())
    }

    pub(crate) fn iter(&self) -> impl Iterator<Item = (&str, &SelectionTrie)> {
        self.selections
            .iter()
            .map(|(key, sub)| (key.as_str(), sub.as_ref()))
    }

    pub(crate) fn key_ranges(&self, key: &str) -> impl Iterator<Item = Range<usize>> {
        self.key_ranges
            .get(key)
            .into_iter()
            .flat_map(|ranges| ranges.iter())
            .cloned()
    }

    #[allow(dead_code)]
    pub(crate) fn has_str_path<'a>(&self, path: impl IntoIterator<Item = &'a str>) -> bool {
        let mut current = self;
        for key in path {
            if let Some(sub) = current.get(key) {
                current = sub;
            } else {
                return false;
            }
        }
        current.is_used()
    }

    #[allow(dead_code)]
    pub(crate) fn add_str_path<'a>(
        &mut self,
        path: impl IntoIterator<Item = &'a str>,
    ) -> &mut Self {
        path.into_iter()
            .fold(self, |trie, key| trie.add_str(key))
            .set_used()
    }

    pub(super) fn add_path_list(&mut self, path_list: &PathList) -> &mut Self {
        match path_list {
            PathList::Key(key, tail) => self.add_key(key).add_path_list(tail.as_ref()),
            PathList::Selection(sub) => self.add_subselection(sub),
            // If we get to the end of the PathList, mark the path used.
            PathList::Empty => self.set_used(),
            // TODO Support PathList::Method and inputs used within method
            // arguments. For now, assume we use the whole path up to the
            // unhandled PathList element.
            _ => self.set_used(),
        }
    }

    pub(crate) fn add_subselection(&mut self, sub: &SubSelection) -> &mut Self {
        for selection in sub.selections_iter() {
            match selection {
                NamedSelection::Field(_, key, nested_selection) => {
                    let result = self.add_key(key);
                    if let Some(nested) = nested_selection {
                        result.add_subselection(nested);
                    } else {
                        result.set_used();
                    }
                }
                NamedSelection::Path { path, .. } => {
                    self.add_path_list(path.path.as_ref());
                }
                NamedSelection::Group(_, sub) => {
                    self.add_subselection(sub);
                }
            }
        }
        self
    }

    pub(crate) fn extend(&mut self, other: &SelectionTrie) -> &mut Self {
        for (key, sub) in other.selections.iter() {
            if let Some(existing) = self.selections.get_mut(key) {
                Ref::make_mut(existing).extend(sub);
            } else {
                // Because sub is an Arc, this clone should be much cheaper than
                // inserting an empty trie and then recursively extending it
                // while traversing sub.
                self.selections.insert(key.clone(), sub.clone());
            }
            // Whether or not the key already existed, we update self.key_ranges
            // the same way:
            self.key_ranges
                .entry(key.clone())
                .or_default()
                .extend(other.key_ranges(key));
        }
        if self.is_used() || other.is_used() {
            self.set_used()
        } else {
            self
        }
    }

    /// Like [`SelectionTrie::extend`] but producing a new SelectionTrie
    /// instance instead of modifying self.
    #[allow(dead_code)]
    pub(crate) fn merge(&self, other: &SelectionTrie) -> Self {
        let mut merged = SelectionTrie::new();
        merged.extend(self);
        merged.extend(other);
        merged
    }

    fn add_str(&mut self, key: &str) -> &mut Self {
        if !self.selections.contains_key(key) {
            self.selections
                .insert(key.to_string(), Ref::new(SelectionTrie::new()));
        }
        Ref::make_mut(self.selections.get_mut(key).expect("should exist"))
    }

    fn add_str_with_ranges(
        &mut self,
        key: &str,
        ranges: impl IntoIterator<Item = Range<usize>>,
    ) -> &mut Self {
        self.key_ranges
            .entry(key.to_string())
            .or_default()
            .extend(ranges);
        self.add_str(key)
    }

    fn add_key(&mut self, key: &WithRange<Key>) -> &mut Self {
        self.add_str_with_ranges(key.as_str(), key.range())
    }

    fn set_used(&mut self) -> &mut Self {
        self.used = true;
        self
    }

    fn is_used(&self) -> bool {
        self.used
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_empty() {
        let trie = SelectionTrie::new();
        assert!(trie.is_empty());
        assert_eq!(trie.keys().count(), 0);
        assert_eq!(trie.iter().count(), 0);
        assert_eq!(trie.key_ranges("field").count(), 0);
        assert!(!trie.is_used());

        let used = SelectionTrie::new_used();
        assert!(used.is_empty());
        assert_eq!(used.keys().count(), 0);
        assert_eq!(used.iter().count(), 0);
        assert_eq!(used.key_ranges("saves").count(), 0);
        assert!(used.is_used());
    }

    #[test]
    fn test_selection_trie_add_key() {
        let mut trie = SelectionTrie::new();
        trie.add_key(&WithRange::new(Key::Field("field".to_string()), Some(0..5)))
            .set_used();

        assert!(!trie.is_empty());
        assert_eq!(trie.keys().count(), 1);
        assert_eq!(trie.key_ranges("field").count(), 1);
        assert!(!trie.is_used());

        assert!(trie.set_used().is_used());
        assert!(trie.is_used());

        assert_eq!(trie.key_ranges("field").collect::<Vec<_>>(), vec![0..5]);

        trie.add_key(&WithRange::new(
            Key::Field("field".to_string()),
            Some(5..10),
        ))
        .set_used();
        assert_eq!(
            trie.key_ranges("field").collect::<Vec<_>>(),
            vec![0..5, 5..10]
        );
        assert_eq!(trie.keys().count(), 1);

        trie.add_key(&WithRange::new(
            Key::Field("other".to_string()),
            Some(15..20),
        ))
        .set_used();
        assert_eq!(trie.keys().count(), 2);
        assert_eq!(trie.key_ranges("other").collect::<Vec<_>>(), vec![15..20]);
        assert_eq!(
            trie.key_ranges("field").collect::<Vec<_>>(),
            vec![0..5, 5..10]
        );
        assert!(trie.is_used());

        assert_eq!(trie.to_string(), "field other");
    }

    #[test]
    fn test_selection_trie_add_path() {
        let mut trie = SelectionTrie::new();
        trie.add_str_path(["a", "b", "c"]);

        assert!(!trie.is_empty());
        assert_eq!(trie.keys().count(), 1);
        assert_eq!(trie.key_ranges("a").count(), 0);
        assert_eq!(trie.key_ranges("b").count(), 0);
        assert_eq!(trie.key_ranges("c").count(), 0);
        assert!(!trie.is_used());
        assert_eq!(trie.to_string(), "a { b { c } }");

        assert!(trie.has_str_path(["a", "b", "c"]));
        assert!(!trie.has_str_path(["a", "b"]));
        assert!(!trie.has_str_path(["a"]));
        assert!(!trie.has_str_path(["b"]));
        assert!(!trie.has_str_path(["c"]));
        assert!(!trie.has_str_path(["a", "b", "c", "d"]));
        assert!(!trie.has_str_path(["a", "b", "c", "d", "e"]));
        assert!(!trie.has_str_path([]));

        trie.add_str_path(["a", "c", "e"]);
        assert!(trie.has_str_path(["a", "c", "e"]));
        assert!(!trie.has_str_path(["a", "c"]));
        assert!(!trie.has_str_path(["a"]));
        assert!(!trie.has_str_path(["c"]));
        assert!(!trie.has_str_path(["e"]));
        assert!(!trie.has_str_path(["a", "c", "e", "f"]));
        assert!(!trie.has_str_path(["a", "c", "e", "f", "g"]));
        assert!(!trie.has_str_path([]));

        trie.add_str_path([]);
        assert!(trie.has_str_path([]));
        assert!(!trie.has_str_path(["a"]));

        assert_eq!(trie.to_string(), "a { b { c } c { e } }");
    }

    #[test]
    fn test_selection_trie_merge() {
        let mut trie1 = SelectionTrie::new();
        trie1.add_str_path(["a", "b", "c"]);
        trie1.add_str_path(["a", "d", "e"]);
        assert_eq!(trie1.to_string(), "a { b { c } d { e } }");

        let mut trie2 = SelectionTrie::new();
        trie2.add_str_path(["a", "b", "f"]);
        trie2.add_str_path(["g", "h"]);
        assert_eq!(trie2.to_string(), "a { b { f } } g { h }");

        let mut merged = trie1.merge(&trie2);
        assert_eq!(merged.to_string(), "a { b { c f } d { e } } g { h }");

        let merged_2_with_1 = trie2.merge(&trie1);
        assert_eq!(
            merged_2_with_1.to_string(),
            "a { b { f c } d { e } } g { h }",
        );

        merged.add_str_path(["a", "b", "x", "y"]);

        assert_eq!(
            merged.to_string(),
            "a { b { c f x { y } } d { e } } g { h }"
        );
        assert_eq!(
            merged_2_with_1.to_string(),
            "a { b { f c } d { e } } g { h }",
        );
        assert_eq!(trie1.to_string(), "a { b { c } d { e } }");
        assert_eq!(trie2.to_string(), "a { b { f } } g { h }");
    }
}
