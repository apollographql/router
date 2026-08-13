//! Building standalone values from Rust data.

use crate::arena::Arena;
use crate::builder::{NewValue, ValueBuilder};
use crate::document::Value;
use crate::node::Node;

impl Value {
    /// A `null` value.
    pub fn null() -> Value {
        scalar(|_| Node::Null)
    }

    /// An array holding `items`, in order.
    ///
    /// A [`Value`] item is adopted by reference, so the array pins the arena
    /// that item came from; [`Value::compact`] severs that before retention.
    /// Non-finite floats land as `null`.
    ///
    /// # Example
    /// ```
    /// use apollo_json::Value;
    ///
    /// assert_eq!(Value::array(["a", "b"]).to_string(), r#"["a","b"]"#);
    /// assert_eq!(Value::array(Vec::<Value>::new()).to_string(), "[]");
    /// ```
    pub fn array<'a, I>(items: I) -> Value
    where
        I: IntoIterator,
        I::Item: Into<NewValue<'a>>,
    {
        let mut builder = ValueBuilder::new_array().coercing_non_finite();
        for item in items {
            builder.push(item).expect(
                "an array root accepts elements, and the builder coerces non-finite floats",
            );
        }
        builder.seal()
    }

    /// An object holding `entries`, in iteration order. A repeated key keeps
    /// its first position and its last value, matching what parsing
    /// `{"a":1,"a":2}` produces.
    ///
    /// A [`Value`] entry is adopted by reference, so the object pins the arena
    /// that value came from; [`Value::compact`] severs that before retention.
    /// Non-finite floats land as `null`.
    ///
    /// # Example
    /// ```
    /// use apollo_json::Value;
    ///
    /// let object = Value::object([("id", 7i64)]);
    /// assert_eq!(object.to_string(), r#"{"id":7}"#);
    /// ```
    pub fn object<'a, I, K, V>(entries: I) -> Value
    where
        I: IntoIterator<Item = (K, V)>,
        K: AsRef<str>,
        V: Into<NewValue<'a>>,
    {
        let mut builder = ValueBuilder::new().coercing_non_finite();
        for (key, value) in entries {
            builder.set(key.as_ref(), value).expect(
                "an object root accepts any key, and the builder coerces non-finite floats",
            );
        }
        builder.seal()
    }
}

impl From<NewValue<'_>> for Value {
    /// Writes a pending tree into one fresh arena in a single pass — the
    /// conversion behind [`json!`](crate::json). [`NewValue::Node`] hands
    /// back the handle it wraps, and any handle nested deeper is adopted by
    /// reference. Non-finite floats land as `null`, matching [`Value::array`]
    /// and [`Value::object`].
    fn from(value: NewValue<'_>) -> Value {
        match value {
            NewValue::Node(handle) => handle,
            pending => {
                let mut builder = ValueBuilder::new().coercing_non_finite();
                builder
                    .set_path(&[], pending)
                    .expect("the builder coerces non-finite floats");
                builder.seal()
            }
        }
    }
}

impl From<&str> for Value {
    fn from(value: &str) -> Value {
        scalar(|arena| Node::OwnedString(arena.alloc_text(value)))
    }
}

impl From<String> for Value {
    fn from(value: String) -> Value {
        Value::from(value.as_str())
    }
}

impl From<bool> for Value {
    fn from(value: bool) -> Value {
        scalar(|_| Node::Bool(value))
    }
}

impl From<i64> for Value {
    fn from(value: i64) -> Value {
        number(&value.to_string())
    }
}

impl From<u64> for Value {
    /// Keeps every digit of values above [`i64::MAX`] — numbers are stored as
    /// their literal text.
    fn from(value: u64) -> Value {
        number(&value.to_string())
    }
}

impl From<f64> for Value {
    /// Formats the float the way `serde_json` does. Infinity and NaN have no
    /// JSON form and become `null`, again as `serde_json` writes them.
    fn from(value: f64) -> Value {
        match serde_json::Number::from_f64(value) {
            Some(number_value) => number(&number_value.to_string()),
            None => Value::null(),
        }
    }
}

/// Widens the smaller integer types into the `i64` and `u64` impls above, so
/// callers holding a `usize` count or a `u32` id do not have to cast at every
/// call site.
macro_rules! from_integer {
    ($($signed:ty),* ; $($unsigned:ty),*) => {
        $(
            impl From<$signed> for Value {
                fn from(value: $signed) -> Value {
                    Value::from(i64::from(value))
                }
            }
            impl From<$signed> for crate::NewValue<'_> {
                fn from(value: $signed) -> crate::NewValue<'static> {
                    crate::NewValue::Int(i64::from(value))
                }
            }
        )*
        $(
            impl From<$unsigned> for Value {
                fn from(value: $unsigned) -> Value {
                    Value::from(u64::from(value))
                }
            }
            impl From<$unsigned> for crate::NewValue<'_> {
                fn from(value: $unsigned) -> crate::NewValue<'static> {
                    crate::NewValue::from(u64::from(value))
                }
            }
        )*
    };
}

from_integer!(i8, i16, i32 ; u8, u16, u32);

impl From<u64> for crate::NewValue<'_> {
    /// Values above [`i64::MAX`] keep every digit: they are written as their
    /// decimal literal rather than being narrowed into the `Int` variant.
    fn from(value: u64) -> crate::NewValue<'static> {
        match i64::try_from(value) {
            Ok(narrowed) => crate::NewValue::Int(narrowed),
            Err(_) => crate::NewValue::Node(Value::from(value)),
        }
    }
}

impl From<usize> for Value {
    fn from(value: usize) -> Value {
        Value::from(value as u64)
    }
}

impl From<usize> for crate::NewValue<'_> {
    fn from(value: usize) -> crate::NewValue<'static> {
        crate::NewValue::from(value as u64)
    }
}

/// A value holding the number written as `literal`, which the caller has
/// already formatted as JSON.
fn number(literal: &str) -> Value {
    scalar(|arena| Node::OwnedNumber(arena.alloc_text(literal)))
}

/// A value whose root is one scalar node, in an arena of its own.
fn scalar(build: impl FnOnce(&mut Arena) -> Node) -> Value {
    let mut arena = Arena::new(1);
    let node = build(&mut arena);
    let root = arena.push_node(node);
    Value::rooted(arena, root)
}
