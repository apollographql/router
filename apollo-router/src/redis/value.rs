use std::fmt;

use fred::error::Error as RedisError;
use fred::error::ErrorKind as RedisErrorKind;
use serde::Serialize;
use serde::de::DeserializeOwned;

pub(crate) trait ValueType:
    Clone + fmt::Debug + Send + Sync + Serialize + DeserializeOwned
{
}

#[derive(Clone, Debug)]
pub(crate) struct Value<V: ValueType>(pub(crate) V);

impl<V: ValueType> From<V> for Value<V> {
    fn from(value: V) -> Self {
        Value(value)
    }
}

impl<V: ValueType> fmt::Display for Value<V> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}|{:?}", get_type_of(&self.0), self.0)
    }
}

impl<V: ValueType> ValueType for Option<V> {}

impl<V: ValueType> fred::prelude::FromValue for Value<Option<V>> {
    fn from_value(value: fred::types::Value) -> Result<Self, RedisError> {
        match value {
            fred::types::Value::Bytes(data) => serde_json::from_slice(&data)
                .map(|v| Value(Some(v)))
                .map_err(|e| {
                    RedisError::new(
                        RedisErrorKind::Parse,
                        format!("can't deserialize from JSON: {e}"),
                    )
                }),
            fred::types::Value::String(s) => serde_json::from_str(&s)
                .map(|v| Value(Some(v)))
                .map_err(|e| {
                    RedisError::new(
                        RedisErrorKind::Parse,
                        format!("can't deserialize from JSON: {e}"),
                    )
                }),
            fred::types::Value::Null => Ok(Value(None)),
            _res => Err(RedisError::new(
                RedisErrorKind::Parse,
                "the data is the wrong type",
            )),
        }
    }
}

impl<V: ValueType> TryInto<fred::types::Value> for Value<V> {
    type Error = RedisError;

    fn try_into(self) -> Result<fred::types::Value, Self::Error> {
        let v = serde_json::to_vec(&self.0).map_err(|e| {
            tracing::error!("couldn't serialize value to redis {}. This is a bug in the router, please file an issue: https://github.com/apollographql/router/issues/new", e);
            RedisError::new(
                RedisErrorKind::Parse,
                format!("couldn't serialize value to redis {e}"),
            )
        })?;

        Ok(fred::types::Value::Bytes(v.into()))
    }
}

fn get_type_of<T>(_: &T) -> &'static str {
    std::any::type_name::<T>()
}

impl ValueType for String {}
impl ValueType for usize {}
