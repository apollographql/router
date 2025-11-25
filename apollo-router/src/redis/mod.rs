//! Redis [gateway](https://martinfowler.com/articles/gateway-pattern.html).

mod gateway;
mod key;
mod metrics;
mod value;

pub(crate) use gateway::RedisCacheStorage as Gateway;
pub(self) use key::Key;
pub(crate) use key::KeyType;
pub(self) use value::Value;
pub(crate) use value::ValueType;
