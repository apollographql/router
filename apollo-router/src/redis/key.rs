use std::fmt;
use std::hash::Hash;

pub(crate) trait KeyType: Clone + fmt::Debug + fmt::Display + Send + Sync {}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub(crate) struct Key<K: KeyType>(pub(crate) K);

impl<K: KeyType> fmt::Display for Key<K> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl<K: KeyType> From<Key<K>> for fred::types::Key {
    fn from(val: Key<K>) -> Self {
        val.to_string().into()
    }
}

// Blanket implementation which satisfies the compiler
impl<K: Clone + fmt::Debug + fmt::Display + Send + Sync> KeyType for K {}
