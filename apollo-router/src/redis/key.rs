use std::fmt;

use crate::cache::storage::KeyType;

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub(crate) struct Key<K>(pub(crate) K)
where
    K: KeyType;

impl<K> fmt::Display for Key<K>
where
    K: KeyType,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl<K> From<Key<K>> for fred::types::Key
where
    K: KeyType,
{
    fn from(val: Key<K>) -> Self {
        val.to_string().into()
    }
}
