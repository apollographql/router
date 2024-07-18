use std::cmp::Ordering;
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

static EMPTY_DIRECTIVE_LIST: executable::DirectiveList = executable::DirectiveList(vec![]);

#[derive(Debug, Clone)]
struct DirectiveListInner {
    // The hash is eagerly precomputed because we expect to, most of the time, hash a DirectiveList
    // at least once.
    // hash is 0 if the list is empty.
    hash: u64,
    // Mutable access should not be handed out because `sort_order` may get out of sync.
    directives: executable::DirectiveList,
    sort_order: Vec<usize>,
}

impl PartialEq for DirectiveListInner {
    fn eq(&self, other: &Self) -> bool {
        self.hash == other.hash
            && self.iter().zip(other.iter()).all(|(left, right)| {
                left.name == right.name
                    && compare_sorted_arguments(&left.arguments, &right.arguments)
                        == Ordering::Equal
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
        for d in self.iter() {
            d.hash(&mut state);
        }
        self.hash = state.finish();
    }

    fn len(&self) -> usize {
        self.directives.len()
    }

    fn iter(&self) -> DirectiveIter<'_> {
        DirectiveIter {
            directives: &self.directives.0,
            inner: self.sort_order.iter(),
        }
    }
}

/// A list of directives, with order-independent hashing and equality.
///
/// Original order is stored but is not part of hashing, so it may be lost by certain operations.
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
        self.inner.as_ref().map_or(&EMPTY_DIRECTIVE_LIST, |inner| &inner.directives)
    }
}

impl Hash for DirectiveList {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        state.write_u64(self.inner.as_ref().map_or(0, |inner| inner.hash))
    }
}

impl From<executable::DirectiveList> for DirectiveList {
    fn from(mut directives: executable::DirectiveList) -> Self {
        if directives.is_empty() {
            return Self::default();
        }

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

        if directives.is_empty() {
            EMPTY_DIRECTIVE_LISTS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        } else {
            NONEMPTY_DIRECTIVE_LISTS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }

        let mut partially_initialized = DirectiveListInner {
            hash: 0,
            directives,
            sort_order,
        };
        partially_initialized.rehash();
        Self { inner: Some(Arc::new(partially_initialized)) }
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
    fn rehash(&mut self) {
        if let Some(inner) = self.inner.as_mut().map(Arc::make_mut) {
            inner.rehash();
        }
    }

    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn iter(&self) -> DirectiveIter<'_> {
        self.inner.as_ref().map_or_else(DirectiveIter::empty, |inner| DirectiveIter {
            directives: &inner.directives,
            inner: inner.sort_order.iter(),
        })
    }

    pub(crate) fn iter_original_order(
        &self,
    ) -> impl ExactSizeIterator<Item = &Node<executable::Directive>> {
        self.inner.as_ref().map_or(&EMPTY_DIRECTIVE_LIST, |inner| &inner.directives).iter()
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

        // The directive exists: clone if necessary.
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

pub(crate) struct DirectiveIter<'a> {
    directives: &'a [Node<executable::Directive>],
    inner: std::slice::Iter<'a, usize>,
}
impl<'a> Iterator for DirectiveIter<'a> {
    type Item = &'a Node<executable::Directive>;

    fn next(&mut self) -> Option<Self::Item> {
        self.inner.next().map(|index| &self.directives[*index])
    }
}

impl ExactSizeIterator for DirectiveIter<'_> {
    fn len(&self) -> usize {
        self.inner.len()
    }
}

impl<'a> IntoIterator for &'a DirectiveList {
    type Item = &'a Node<executable::Directive>;
    type IntoIter = DirectiveIter<'a>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

impl DirectiveIter<'_> {
    fn empty() -> Self {
        Self { directives: &[], inner: [].iter() }
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
            "should be order independent"
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
