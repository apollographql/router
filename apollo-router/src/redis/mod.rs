//! Redis [gateway](https://martinfowler.com/articles/gateway-pattern.html).

mod error;
mod gateway;
mod key;
mod metrics;
pub(crate) mod options;
mod pool;
mod value;

#[cfg(test)]
pub(crate) mod test;

pub(crate) use error::Error;
pub(crate) use gateway::Gateway;
pub(crate) use key::KeyType;
pub(crate) use value::ValueType;
pub(crate) use value::try_from_redis_value;
pub(crate) use value::try_into_redis_value;

pub(crate) type FredResult<T> = Result<T, fred::error::Error>;
pub(crate) type FredValue = fred::types::Value;
