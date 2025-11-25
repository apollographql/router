//! Redis [gateway](https://martinfowler.com/articles/gateway-pattern.html).

mod key;
mod metrics;
pub(crate) mod redis;
mod value;

pub(crate) use key::Key;
pub(crate) use redis::RedisCacheStorage as Gateway;
pub(crate) use value::Value;
