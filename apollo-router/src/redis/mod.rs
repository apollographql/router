//! Redis [gateway](https://martinfowler.com/articles/gateway-pattern.html).

mod gateway;
mod key;
mod metrics;
mod value;

pub(crate) use gateway::RedisCacheStorage as Gateway;
use key::Key;
pub(crate) use key::KeyType;
use value::Value;
pub(crate) use value::ValueType;
