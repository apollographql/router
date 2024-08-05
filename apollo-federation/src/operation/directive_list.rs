use std::fmt;
use std::fmt::Display;
use std::hash::BuildHasher;
use std::hash::Hash;
use std::hash::Hasher;
use std::ops::Deref;
use std::sync::Arc;
use std::sync::OnceLock;

use apollo_compiler::executable;
use apollo_compiler::Node;

use super::compare_sorted_arguments;
use super::sort_arguments;

/// An empty apollo-compiler directive list that we can return a reference to when a
/// [`DirectiveList`] is in the empty state.
static EMPTY_DIRECTIVE_LIST: executable::DirectiveList = executable::DirectiveList(vec![]);

/// Contents for a non-empty directive list.
#[derive(Debug, Clone)]
struct DirectiveListInner {
    // Cached hash: hashing may be expensive with deeply nested values or very many directives,
    // so we only want to do it once.
    // The hash is eagerly precomputed because we expect to, most of the time, hash a DirectiveList
    // at least once (when inserting its selection into a selection map).
    hash: u64,
    // Mutable access to the underlying directive list should not be handed out because `sort_order`
    // may get out of sync.
    directives: executable::DirectiveList,
    sort_order: Vec<usize>,
}

impl PartialEq for DirectiveListInner {
    fn eq(&self, other: &Self) -> bool {
        self.hash == other.hash
            && self
                .iter_sorted()
                .zip(other.iter_sorted())
                .all(|(left, right)| {
                    // We can just use `Eq` because the arguments are sorted recursively
                    left.name == right.name && left.arguments == right.arguments
                })
    }
}

impl Eq for DirectiveListInner {}

impl DirectiveListInner {
    fn rehash(&mut self) {
        static SHARED_RANDOM: OnceLock<std::hash::RandomState> = OnceLock::new();

        let mut state = SHARED_RANDOM.get_or_init(Default::default).build_hasher();
        self.len().hash(&mut state);
        // Hash in sorted order
        for d in self.iter_sorted() {
            d.hash(&mut state);
        }
        self.hash = state.finish();
    }

    fn len(&self) -> usize {
        self.directives.len()
    }

    fn iter_sorted(&self) -> DirectiveIterSorted<'_> {
        DirectiveIterSorted {
            directives: &self.directives.0,
            inner: self.sort_order.iter(),
        }
    }
}

/// A list of directives, with order-independent hashing and equality.
///
/// Original order of directive applications is stored but is not part of hashing,
/// so it may not be maintained exactly when round-tripping several directive lists
/// through a HashSet for example.
///
/// Arguments and input object values provided to directives are all sorted and the
/// original order is not tracked.
///
/// This list is cheaply cloneable, but not intended for frequent mutations.
/// When the list is empty, it does not require an allocation.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct DirectiveList {
    inner: Option<Arc<DirectiveListInner>>,
}

impl Deref for DirectiveList {
    type Target = executable::DirectiveList;
    fn deref(&self) -> &Self::Target {
        self.inner
            .as_ref()
            .map_or(&EMPTY_DIRECTIVE_LIST, |inner| &inner.directives)
    }
}

impl Hash for DirectiveList {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        state.write_u64(self.inner.as_ref().map_or(0, |inner| inner.hash))
    }
}

impl Display for DirectiveList {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(inner) = &self.inner {
            inner.directives.fmt(f)
        } else {
            Ok(())
        }
    }
}

impl From<executable::DirectiveList> for DirectiveList {
    fn from(mut directives: executable::DirectiveList) -> Self {
        if directives.is_empty() {
            return Self::new();
        }

        // Sort directives, which means specifically sorting their arguments, sorting the directives by
        // name, and then breaking directive-name ties by comparing sorted arguments. This is used for
        // hashing arguments in a way consistent with [same_directives()].

        for directive in directives.iter_mut() {
            sort_arguments(&mut directive.make_mut().arguments);
        }

        let mut sort_order = (0usize..directives.len()).collect::<Vec<_>>();
        sort_order.sort_by(|left, right| {
            let left = &directives[*left];
            let right = &directives[*right];
            left.name
                .cmp(&right.name)
                .then_with(|| compare_sorted_arguments(&left.arguments, &right.arguments))
        });

        let mut partially_initialized = DirectiveListInner {
            hash: 0,
            directives,
            sort_order,
        };
        partially_initialized.rehash();
        Self {
            inner: Some(Arc::new(partially_initialized)),
        }
    }
}

impl FromIterator<Node<executable::Directive>> for DirectiveList {
    fn from_iter<T: IntoIterator<Item = Node<executable::Directive>>>(iter: T) -> Self {
        Self::from(executable::DirectiveList::from_iter(iter))
    }
}

impl FromIterator<executable::Directive> for DirectiveList {
    fn from_iter<T: IntoIterator<Item = executable::Directive>>(iter: T) -> Self {
        Self::from(executable::DirectiveList::from_iter(iter))
    }
}

impl DirectiveList {
    /// Create an empty directive list.
    pub(crate) const fn new() -> Self {
        Self { inner: None }
    }

    /// Create a directive list with a single directive.
    ///
    /// This sorts arguments and input object values provided to the directive.
    pub(crate) fn one(directive: impl Into<Node<executable::Directive>>) -> Self {
        std::iter::once(directive.into()).collect()
    }

    /// Iterate the directives in their original order.
    pub(crate) fn iter(&self) -> impl ExactSizeIterator<Item = &Node<executable::Directive>> {
        self.inner
            .as_ref()
            .map_or(&EMPTY_DIRECTIVE_LIST, |inner| &inner.directives)
            .iter()
    }

    /// Iterate the directives in a consistent sort order.
    pub(crate) fn iter_sorted(&self) -> DirectiveIterSorted<'_> {
        self.inner
            .as_ref()
            .map_or_else(DirectiveIterSorted::empty, |inner| inner.iter_sorted())
    }

    /// Remove one directive application by name.
    ///
    /// To remove a repeatable directive, you may need to call this multiple times.
    pub(crate) fn remove_one(&mut self, name: &str) -> Option<Node<executable::Directive>> {
        let Some(inner) = self.inner.as_mut() else {
            // Nothing to do on an empty list
            return None;
        };
        let Some(index) = inner.directives.iter().position(|dir| dir.name == name) else {
            return None;
        };

        // The directive exists and is the only directive: switch to the empty representation
        if inner.len() == 1 {
            // The index is guaranteed to exist so we can safely use the panicky [] syntax.
            let item = inner.directives[index].clone();
            self.inner = None;
            return Some(item);
        }

        // The directive exists: clone the inner structure if necessary.
        let inner = Arc::make_mut(inner);
        let sort_index = inner
            .sort_order
            .iter()
            .position(|sorted| *sorted == index)
            .expect("index must exist in sort order");
        let item = inner.directives.remove(index);
        inner.sort_order.remove(sort_index);

        for order in &mut inner.sort_order {
            if *order > index {
                *order -= 1;
            }
        }
        inner.rehash();
        Some(item)
    }
}

/// Iterate over a [`DirectiveList`] in a consistent sort order.
pub(crate) struct DirectiveIterSorted<'a> {
    directives: &'a [Node<executable::Directive>],
    inner: std::slice::Iter<'a, usize>,
}
impl<'a> Iterator for DirectiveIterSorted<'a> {
    type Item = &'a Node<executable::Directive>;

    fn next(&mut self) -> Option<Self::Item> {
        self.inner.next().map(|index| &self.directives[*index])
    }
}

impl ExactSizeIterator for DirectiveIterSorted<'_> {
    fn len(&self) -> usize {
        self.inner.len()
    }
}

impl DirectiveIterSorted<'_> {
    fn empty() -> Self {
        Self {
            directives: &[],
            inner: [].iter(),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use apollo_compiler::name;
    use apollo_compiler::Name;
    use apollo_compiler::Node;

    use super::*;

    fn directive(
        name: &str,
        arguments: Vec<Node<executable::Argument>>,
    ) -> Node<executable::Directive> {
        executable::Directive {
            name: Name::new_unchecked(name),
            arguments,
        }
        .into()
    }

    #[test]
    fn consistent_hash() {
        let mut set = HashSet::new();

        assert!(set.insert(DirectiveList::new()));
        assert!(!set.insert(DirectiveList::new()));

        assert!(set.insert(DirectiveList::from_iter([
            directive("a", Default::default()),
            directive("b", Default::default()),
        ])));
        assert!(!set.insert(DirectiveList::from_iter([
            directive("b", Default::default()),
            directive("a", Default::default()),
        ])));
    }

    #[test]
    fn order_independent_equality() {
        assert_eq!(DirectiveList::new(), DirectiveList::new());
        assert_eq!(
            DirectiveList::from_iter([
                directive("a", Default::default()),
                directive("b", Default::default()),
            ]),
            DirectiveList::from_iter([
                directive("b", Default::default()),
                directive("a", Default::default()),
            ]),
            "equality should be order independent"
        );

        assert_eq!(
            DirectiveList::from_iter([
                directive(
                    "a",
                    vec![(name!("arg1"), true).into(), (name!("arg2"), false).into()]
                ),
                directive(
                    "b",
                    vec![(name!("arg2"), false).into(), (name!("arg1"), true).into()]
                ),
            ]),
            DirectiveList::from_iter([
                directive(
                    "b",
                    vec![(name!("arg1"), true).into(), (name!("arg2"), false).into()]
                ),
                directive(
                    "a",
                    vec![(name!("arg1"), true).into(), (name!("arg2"), false).into()]
                ),
            ]),
            "arguments should be order independent"
        );
    }
}
