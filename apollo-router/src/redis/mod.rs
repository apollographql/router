mod cache;
mod connection_pool;
mod timeout;

type Config = crate::configuration::RedisCache;

enum Key<S: ToString> {
    Namespaced(S),
    Simple(S),
}

// TODO: assumes everything is a simple key unless otherwise noted
impl<S: ToString> From<S> for Key<S> {
    fn from(value: S) -> Self {
        Key::Simple(value)
    }
}
