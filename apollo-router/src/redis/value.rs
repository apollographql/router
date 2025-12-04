use std::fmt;

use fred::error::Error as RedisError;
use fred::error::ErrorKind as RedisErrorKind;
use serde::Serialize;
use serde::de::DeserializeOwned;

use super::FredResult;
use super::FredValue;

fn unexpected_type(value: &FredValue) -> RedisError {
    tracing::debug!("unable to parse: {:?}", value);
    RedisError::new(
        RedisErrorKind::Parse,
        format!("can't parse unexpected type: {:?}", value.kind()),
    )
}

fn unable_to_deserialize(error: serde_json::Error) -> RedisError {
    RedisError::new(
        RedisErrorKind::Parse,
        format!("can't deserialize from JSON: {error}"),
    )
}

fn unable_to_serialize(error: serde_json::Error) -> RedisError {
    tracing::error!(
        "couldn't serialize value to redis {}. This is a bug in the router, please file an issue: https://github.com/apollographql/router/issues/new",
        error
    );
    RedisError::new(
        RedisErrorKind::Parse,
        format!("can't serialize to JSON: {error}"),
    )
}

pub(crate) fn try_from_redis_value<S: DeserializeOwned>(value: FredValue) -> FredResult<S> {
    match value.as_bytes_str() {
        Some(byte_str) => serde_json::from_str(&byte_str).map_err(unable_to_deserialize),
        None => Err(unexpected_type(&value)),
    }
}

pub(crate) fn try_into_redis_value<S: Serialize>(value: S) -> FredResult<FredValue> {
    let v = serde_json::to_vec(&value).map_err(unable_to_serialize)?;
    Ok(FredValue::Bytes(v.into()))
}

// TODO: explain that serializable things should just use try_from_redis_value and try_into_redis_value
pub(crate) trait ValueType: Clone + fmt::Debug + Send + Sync {
    fn try_from_redis_value(value: FredValue) -> FredResult<Self>;
    fn try_into_redis_value(self) -> FredResult<FredValue>;
}

#[derive(Clone, Debug)]
pub(super) struct Value<V: ValueType>(pub(super) V);

impl<V: ValueType> From<V> for Value<V> {
    fn from(value: V) -> Self {
        Value(value)
    }
}

impl<V: ValueType> ValueType for Value<V> {
    fn try_from_redis_value(value: FredValue) -> FredResult<Self> {
        V::try_from_redis_value(value).map(Value)
    }

    fn try_into_redis_value(self) -> FredResult<FredValue> {
        self.0.try_into_redis_value()
    }
}

impl<V: ValueType> ValueType for Option<V> {
    fn try_from_redis_value(value: FredValue) -> FredResult<Self> {
        V::try_from_redis_value(value).map(Some)
    }

    fn try_into_redis_value(self) -> FredResult<FredValue> {
        self.map(|v| v.try_into_redis_value())
            .unwrap_or(Ok(FredValue::Null))
    }
}

impl<V: ValueType> fmt::Display for Value<V> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}|{:?}", get_type_of(&self.0), self.0)
    }
}

impl<V: ValueType> fred::prelude::FromValue for Value<Option<V>> {
    fn from_value(value: fred::types::Value) -> Result<Self, RedisError> {
        if value.is_null() {
            Ok(Value(None))
        } else {
            V::try_from_redis_value(value).map(|v| Value(Some(v)))
        }
    }
}

impl<V: ValueType> TryInto<fred::types::Value> for Value<V> {
    type Error = RedisError;

    fn try_into(self) -> Result<fred::types::Value, Self::Error> {
        self.0.try_into_redis_value()
    }
}

fn get_type_of<T>(_: &T) -> &'static str {
    std::any::type_name::<T>()
}

impl ValueType for () {
    fn try_from_redis_value(value: FredValue) -> FredResult<Self> {
        fred::prelude::FromValue::from_value(value)
    }

    fn try_into_redis_value(self) -> FredResult<FredValue> {
        FredValue::try_from(self)
            .map_err(|_| RedisError::new(RedisErrorKind::Unknown, "infallible"))
    }
}

impl ValueType for String {
    fn try_from_redis_value(value: FredValue) -> FredResult<Self> {
        fred::prelude::FromValue::from_value(value)
    }

    fn try_into_redis_value(self) -> FredResult<FredValue> {
        FredValue::try_from(self)
            .map_err(|_| RedisError::new(RedisErrorKind::Unknown, "infallible"))
    }
}

impl ValueType for usize {
    fn try_from_redis_value(value: FredValue) -> FredResult<Self> {
        fred::prelude::FromValue::from_value(value)
    }

    fn try_into_redis_value(self) -> FredResult<FredValue> {
        FredValue::try_from(self)
            .map_err(|_| RedisError::new(RedisErrorKind::Unknown, "infallible"))
    }
}
