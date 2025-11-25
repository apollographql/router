//! Redis [gateway](https://martinfowler.com/articles/gateway-pattern.html).

mod gateway;
mod key;
mod metrics;
mod value;

pub(crate) use gateway::RedisCacheStorage as Gateway;
pub(crate) use key::Key;
pub(crate) use key::KeyType;
pub(crate) use value::Value;
pub(crate) use value::ValueType;
