use std::cmp::Ordering;
use std::hash::BuildHasher;
use std::hash::Hash;
use std::hash::Hasher;
use std::ops::Deref;
use std::sync::OnceLock;

use apollo_compiler::executable;
use apollo_compiler::Node;

use super::compare_sorted_arguments;
use super::sort_arguments;

#[derive(Debug, Clone, Default)]
pub(crate) struct DirectiveList {
    hash: u64,
    // Mutable access should not be handed out because `sort_order` may get out of sync.
    inner: executable::DirectiveList,
    sort_order: Vec<usize>,
}

impl Deref for DirectiveList {
    type Target = executable::DirectiveList;
    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

impl Hash for DirectiveList {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        state.write_u64(self.hash)
    }
}

impl PartialEq for DirectiveList {
    fn eq(&self, other: &Self) -> bool {
        self.hash == other.hash
            && self.iter().zip(other.iter()).all(|(left, right)| {
                left.name == right.name
                    && compare_sorted_arguments(&left.arguments, &right.arguments)
                        == Ordering::Equal
            })
    }
}

impl Eq for DirectiveList {}

impl From<executable::DirectiveList> for DirectiveList {
    fn from(mut directives: executable::DirectiveList) -> Self {
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

        let mut partially_initialized = Self {
            hash: 0,
            inner: directives,
            sort_order,
        };
        partially_initialized.rehash();
        partially_initialized
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
        static SHARED_RANDOM: OnceLock<std::hash::RandomState> = OnceLock::new();
        let mut state = SHARED_RANDOM.get_or_init(Default::default).build_hasher();
        self.len().hash(&mut state);
        // Hash in sorted order
        for d in self.iter() {
            d.hash(&mut state);
        }
        self.hash = state.finish();
    }

    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn iter(&self) -> DirectiveIter<'_> {
        DirectiveIter {
            directives: &self.inner.0,
            inner: self.sort_order.iter(),
        }
    }

    pub(crate) fn iter_original_order(
        &self,
    ) -> impl ExactSizeIterator<Item = &Node<executable::Directive>> {
        self.inner.iter()
    }

    /// Remove one directive application by name.
    ///
    /// To remove a repeatable directive, you may need to call this multiple times.
    pub(crate) fn remove_one(&mut self, name: &str) -> Option<Node<executable::Directive>> {
        let Some(index) = self.inner.iter().position(|dir| dir.name == name) else {
            return None;
        };
        let sort_index = self
            .sort_order
            .iter()
            .position(|sorted| *sorted == index)
            .expect("index must exist in sort order");
        let item = self.inner.remove(index);
        self.sort_order.remove(sort_index);
        for order in &mut self.sort_order {
            if *order > index {
                *order -= 1;
            }
        }
        self.rehash();
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
