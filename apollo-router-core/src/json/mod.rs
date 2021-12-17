//! custom JSON `Value` implementation, adapted to use `bytes::Bytes` internally.

macro_rules! tri {
    ($e:expr) => {
        match $e {
            Ok(val) => val,
            Err(err) => return Err(err),
        }
    };
    ($e:expr,) => {
        tri!($e)
    };
}

#[macro_export]
macro_rules! bjson {
    ($($json:tt)+) => {
        {
            let value: crate::json::Value = json!($($json)+).into();
            value
        }
    };
}

mod bytestring;
mod map;
mod serialization;
mod value;
pub(crate) use map::{Entry, Map};
pub(crate) use serialization::object_serialize;
pub use value::Value;
