//! Redis [gateway](https://martinfowler.com/articles/gateway-pattern.html).

mod metrics;
pub(crate) mod redis;

pub(crate) use redis::RedisCacheStorage as Gateway;
pub(crate) use redis::RedisKey as Key;
pub(crate) use redis::RedisValue as Value;
