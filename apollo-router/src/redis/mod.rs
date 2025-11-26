//! Redis [gateway](https://martinfowler.com/articles/gateway-pattern.html).

mod error;
mod gateway;
mod key;
mod metrics;
mod value;

pub(crate) use error::Error;
pub(crate) use gateway::Gateway;
pub(crate) use key::KeyType;
pub(crate) use value::ValueType;
