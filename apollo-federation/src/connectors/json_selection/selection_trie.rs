use std::fmt::Display;
use std::hash::Hash;
use std::hash::Hasher;
use std::ops::Range;

use apollo_compiler::collections::IndexMap;
use apollo_compiler::collections::IndexSet;

use super::JSONSelection;
use super::Key;
use super::PathList;
use super::PathSelection;
use super::Ranged;
use super::SubSelection;
use super::helpers::quote_if_necessary;
use super::location::WithRange;

impl JSONSelection {
    #[cfg(test)]
    pub(crate) fn compute_selection_trie(&self) -> SelectionTrie {
        let mut trie = SelectionTrie::new();

        // TODO Neither external_var_paths nor the root_trie logic below
        // properly considers "internal" variables like $ and @, even though
        // they could potentially refer to external input data. This state of
        // affairs could be improved by examining the tail of each
        // &PathSelection for those variables, even if we cannot (yet)
        // understand their usage in all cases, such as after an -> method call.
        // Ultimately, getting this completely right will require support from
        // the shape library tracking the names of all shapes.

        use super::VarPaths;
        use crate::connectors::json_selection::TopLevelSelection;
        for path in self.external_var_paths() {
            if let PathList::Var(known_var, tail) = path.path.as_ref() {
                trie.add_str_with_ranges(known_var.as_str(), path.range())
                    .add_path_list(tail);
            } else {
                // The self.external_var_paths() method should only return
                // PathSelection elements whose path starts with PathList::Var.
            }
        }

        let mut root_trie = SelectionTrie::new();
        match &self.inner {
            TopLevelSelection::Path(path) => {
                root_trie.add_path_list(&path.path);
            }
            TopLevelSelection::Named(selection) => {
                root_trie.add_subselection(selection);
            }
        };
        trie.add_str("$root").extend(&root_trie);

        trie
    }
}

impl WithRange<PathList> {
    pub(super) fn compute_selection_trie(&self) -> SelectionTrie {
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

    /// Whether the path terminating with this [`SelectionTrie`] node was
    /// explicitly added to the trie, rather than existing only as a prefix of
    /// other paths that have been added.
    is_leaf: bool,

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
                if sub.is_leaf() {
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
        self.is_leaf == other.is_leaf && self.selections == other.selections
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
            is_leaf: false,
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

    #[cfg(test)]
    pub(crate) fn has_str_path<'a>(&self, path: impl IntoIterator<Item = &'a str>) -> bool {
        let mut current = self;
        for key in path {
            if let Some(sub) = current.get(key) {
                current = sub;
            } else {
                return false;
            }
        }
        current.is_leaf()
    }

    #[cfg(test)]
    pub(crate) fn add_str_path<'a>(
        &mut self,
        path: impl IntoIterator<Item = &'a str>,
    ) -> &mut Self {
        path.into_iter()
            .fold(self, |trie, key| trie.add_str(key))
            .set_leaf()
    }

    pub(crate) fn add_path_selection(&mut self, path: &PathSelection) -> &mut Self {
        self.add_path_list(&path.path)
    }

    fn add_path_list(&mut self, path_list: &WithRange<PathList>) -> &mut Self {
        match path_list.as_ref() {
            PathList::Key(key, tail) => self.add_key(key).add_path_list(tail),
            PathList::Selection(sub) => self.add_subselection(sub),
            // If we get to the end of the PathList, mark the path used.
            PathList::Empty => self.set_leaf(),
            // TODO Support PathList::Method and inputs used within method
            // arguments. For now, assume we use the whole path up to the
            // unhandled PathList element.
            _ => self.set_leaf(),
        }
    }

    pub(crate) fn add_subselection(&mut self, sub: &SubSelection) -> &mut Self {
        for selection in sub.selections_iter() {
            self.add_path_selection(&selection.path);
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
        if self.is_leaf() || other.is_leaf() {
            self.set_leaf()
        } else {
            self
        }
    }

    /// Like [`SelectionTrie::extend`] but producing a new SelectionTrie
    /// instance instead of modifying self.
    #[cfg(test)]
    pub(crate) fn merge(&self, other: &SelectionTrie) -> Self {
        let mut merged = SelectionTrie::new();
        merged.extend(self);
        merged.extend(other);
        merged
    }

    fn add_str(&mut self, key: &str) -> &mut Self {
        Ref::make_mut(
            self.selections
                .entry(key.to_string())
                .or_insert_with(|| Ref::new(SelectionTrie::new())),
        )
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

    fn set_leaf(&mut self) -> &mut Self {
        self.is_leaf = true;
        self
    }

    pub(crate) fn is_leaf(&self) -> bool {
        self.is_leaf
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::selection;

    #[test]
    fn test_empty() {
        let trie = SelectionTrie::new();
        assert!(trie.is_empty());
        assert_eq!(trie.keys().count(), 0);
        assert_eq!(trie.iter().count(), 0);
        assert_eq!(trie.key_ranges("field").count(), 0);
        assert!(!trie.is_leaf());

        let empty_leaf = {
            let mut trie = SelectionTrie::new();
            trie.set_leaf();
            trie
        };
        assert!(empty_leaf.is_empty());
        assert_eq!(empty_leaf.keys().count(), 0);
        assert_eq!(empty_leaf.iter().count(), 0);
        assert_eq!(empty_leaf.key_ranges("saves").count(), 0);
        assert!(empty_leaf.is_leaf());
    }

    #[test]
    fn test_selection_trie_add_key() {
        let mut trie = SelectionTrie::new();
        trie.add_key(&WithRange::new(Key::Field("field".to_string()), Some(0..5)))
            .set_leaf();

        assert!(!trie.is_empty());
        assert_eq!(trie.keys().count(), 1);
        assert_eq!(trie.key_ranges("field").count(), 1);
        assert!(!trie.is_leaf());

        assert!(trie.set_leaf().is_leaf());
        assert!(trie.is_leaf());

        assert_eq!(trie.key_ranges("field").collect::<Vec<_>>(), vec![0..5]);

        trie.add_key(&WithRange::new(
            Key::Field("field".to_string()),
            Some(5..10),
        ))
        .set_leaf();
        assert_eq!(
            trie.key_ranges("field").collect::<Vec<_>>(),
            vec![0..5, 5..10]
        );
        assert_eq!(trie.keys().count(), 1);

        trie.add_key(&WithRange::new(
            Key::Field("other".to_string()),
            Some(15..20),
        ))
        .set_leaf();
        assert_eq!(trie.keys().count(), 2);
        assert_eq!(trie.key_ranges("other").collect::<Vec<_>>(), vec![15..20]);
        assert_eq!(
            trie.key_ranges("field").collect::<Vec<_>>(),
            vec![0..5, 5..10]
        );
        assert!(trie.is_leaf());

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
        assert!(!trie.is_leaf());
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

    #[test]
    fn test_whole_selection_trie() {
        assert_eq!(
            selection!("a { b { c } d { e } }")
                .compute_selection_trie()
                .to_string(),
            "$root { a { b { c } d { e } } }",
        );

        assert_eq!(
            selection!("a { b { c: $args.c } d { e: $this.e } }")
                .compute_selection_trie()
                .to_string(),
            "$args { c } $this { e } $root { a { b d } }",
        );
    }
}
