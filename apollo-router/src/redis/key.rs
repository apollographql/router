use std::fmt;
use std::hash::Hash;

pub(crate) trait KeyType: Clone + fmt::Debug + fmt::Display + Send + Sync {}

// TODO: namespaced vs simple?
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub(super) struct Key<K: KeyType>(pub(super) K);

impl<K: KeyType> From<K> for Key<K> {
    fn from(key: K) -> Self {
        Key(key)
    }
}

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
