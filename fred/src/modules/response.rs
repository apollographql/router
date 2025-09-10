use crate::{
  error::{Error, ErrorKind},
  types::{Key, Value, QUEUED},
};
use bytes::Bytes;
use bytes_utils::Str;
use std::{
  collections::{BTreeMap, BTreeSet, HashMap, HashSet},
  hash::{BuildHasher, Hash},
};

#[cfg(feature = "i-cluster")]
use crate::types::cluster::ClusterInfo;
#[cfg(feature = "i-geo")]
use crate::types::geo::GeoPosition;
#[cfg(feature = "i-slowlog")]
use crate::types::SlowlogEntry;
#[cfg(feature = "i-memory")]
use crate::types::{DatabaseMemoryStats, MemoryStats};

#[allow(unused_imports)]
use std::any::type_name;

macro_rules! debug_type(
  ($($arg:tt)*) => {
    #[cfg(feature="network-logs")]
    log::trace!($($arg)*);
  }
);

macro_rules! check_single_bulk_reply(
  ($v:expr) => {
    if $v.is_single_element_vec() {
      return Self::from_value($v.pop_or_take());
    }
  };
  ($t:ty, $v:expr) => {
    if $v.is_single_element_vec() {
      return $t::from_value($v.pop_or_take());
    }
  }
);

macro_rules! to_signed_number(
  ($t:ty, $v:expr) => {
    match $v {
      Value::Double(f) => Ok(f as $t),
      Value::Integer(i) => Ok(i as $t),
      Value::String(s) => s.parse::<$t>().map_err(|e| e.into()),
      Value::Array(mut a) => if a.len() == 1 {
        match a.pop().unwrap() {
          Value::Integer(i) => Ok(i as $t),
          Value::String(s) => s.parse::<$t>().map_err(|e| e.into()),
          #[cfg(feature = "default-nil-types")]
          Value::Null => Ok(0),
          #[cfg(not(feature = "default-nil-types"))]
          Value::Null => Err(Error::new(ErrorKind::NotFound, "Cannot convert nil to number.")),
          _ => Err(Error::new_parse("Cannot convert to number."))
        }
      }else{
        Err(Error::new_parse("Cannot convert array to number."))
      }
      #[cfg(feature = "default-nil-types")]
      Value::Null => Ok(0),
      #[cfg(not(feature = "default-nil-types"))]
      Value::Null => Err(Error::new(ErrorKind::NotFound, "Cannot convert nil to number.")),
      _ => Err(Error::new_parse("Cannot convert to number.")),
    }
  }
);

macro_rules! to_unsigned_number(
  ($t:ty, $v:expr) => {
    match $v {
      Value::Double(f) => if f.is_sign_negative() {
        Err(Error::new_parse("Cannot convert from negative number."))
      }else{
        Ok(f as $t)
      },
      Value::Integer(i) => if i < 0 {
        Err(Error::new_parse("Cannot convert from negative number."))
      }else{
        Ok(i as $t)
      },
      Value::String(s) => s.parse::<$t>().map_err(|e| e.into()),
      Value::Array(mut a) => if a.len() == 1 {
        match a.pop().unwrap() {
          Value::Integer(i) => if i < 0 {
            Err(Error::new_parse("Cannot convert from negative number."))
          }else{
            Ok(i as $t)
          },
          #[cfg(feature = "default-nil-types")]
          Value::Null => Ok(0),
          #[cfg(not(feature = "default-nil-types"))]
          Value::Null => Err(Error::new(ErrorKind::NotFound, "Cannot convert nil to number.")),
          Value::String(s) => s.parse::<$t>().map_err(|e| e.into()),
          _ => Err(Error::new_parse("Cannot convert to number."))
        }
      }else{
        Err(Error::new_parse("Cannot convert array to number."))
      },
      #[cfg(feature = "default-nil-types")]
      Value::Null => Ok(0),
      #[cfg(not(feature = "default-nil-types"))]
      Value::Null => Err(Error::new(ErrorKind::NotFound, "Cannot convert nil to number.")),
      _ => Err(Error::new_parse("Cannot convert to number.")),
    }
  }
);

macro_rules! impl_signed_number (
  ($t:ty) => {
    impl FromValue for $t {
      fn from_value(value: Value) -> Result<$t, Error> {
        check_single_bulk_reply!(value);
        to_signed_number!($t, value)
      }
    }
  }
);

macro_rules! impl_unsigned_number (
  ($t:ty) => {
    impl FromValue for $t {
      fn from_value(value: Value) -> Result<$t, Error> {
        check_single_bulk_reply!(value);
        to_unsigned_number!($t, value)
      }
    }
  }
);

/// A trait used to [convert](Value::convert) various forms of [Value](Value) into different types.
///
/// ## Examples
///
/// ```rust
/// # use fred::types::Value;
/// # use std::collections::HashMap;
/// let foo: usize = Value::String("123".into()).convert()?;
/// let foo: i64 = Value::String("123".into()).convert()?;
/// let foo: String = Value::String("123".into()).convert()?;
/// let foo: Vec<u8> = Value::Bytes(vec![102, 111, 111].into()).convert()?;
/// let foo: Vec<u8> = Value::String("foo".into()).convert()?;
/// let foo: Vec<String> = Value::Array(vec!["a".into(), "b".into()]).convert()?;
/// let foo: HashMap<String, u16> =
///   Value::Array(vec!["a".into(), 1.into(), "b".into(), 2.into()]).convert()?;
/// let foo: (String, i64) = Value::Array(vec!["a".into(), 1.into()]).convert()?;
/// let foo: Vec<(String, i64)> =
///   Value::Array(vec!["a".into(), 1.into(), "b".into(), 2.into()]).convert()?;
/// // ...
/// ```
///
/// ## Bulk Values
///
/// This interface can also convert single-element vectors to scalar values in certain scenarios. This is often
/// useful with commands that conditionally return bulk values, or where the number of elements in the response
/// depends on the number of arguments (`MGET`, etc).
///
/// For example:
///
/// ```rust
/// # use fred::types::Value;
/// let _: String = Value::Array(vec![]).convert()?; // error
/// let _: String = Value::Array(vec!["a".into()]).convert()?; // "a"
/// let _: String = Value::Array(vec!["a".into(), "b".into()]).convert()?; // error
/// let _: Option<String> = Value::Array(vec![]).convert()?; // None
/// let _: Option<String> = Value::Array(vec!["a".into()]).convert()?; // Some("a")
/// let _: Option<String> = Value::Array(vec!["a".into(), "b".into()]).convert()?; // error
/// ```
///
/// ## The `default-nil-types` Feature Flag
///
/// By default a `nil` value cannot be converted directly into any of the scalar types (`u8`, `String`, `Bytes`,
/// etc). In practice this often requires callers to use an `Option` or `Vec` container with commands that can return
/// `nil`.
///
/// The `default-nil-types` feature flag can enable some further type conversion branches that treat `nil` values as
/// default values for the relevant type. For `Value::Null` these include:
///
/// * `impl FromValue` for `String` or `Str` returns an empty string.
/// * `impl FromValue` for `Bytes` or `Vec<T>` returns an empty array.
/// * `impl FromValue` for any integer or float type returns `0`
/// * `impl FromValue` for `bool` returns `false`
/// * `impl FromValue` for map or set types return an empty map or set.
pub trait FromValue: Sized {
  fn from_value(value: Value) -> Result<Self, Error>;

  #[doc(hidden)]
  fn from_values(values: Vec<Value>) -> Result<Vec<Self>, Error> {
    values.into_iter().map(|v| Self::from_value(v)).collect()
  }

  #[doc(hidden)]
  // FIXME if/when specialization is stable
  fn from_owned_bytes(_: Vec<u8>) -> Option<Vec<Self>> {
    None
  }
  #[doc(hidden)]
  fn is_tuple() -> bool {
    false
  }
}

impl FromValue for Value {
  fn from_value(value: Value) -> Result<Self, Error> {
    Ok(value)
  }
}

impl FromValue for () {
  fn from_value(_: Value) -> Result<Self, Error> {
    Ok(())
  }
}

impl_signed_number!(i8);
impl_signed_number!(i16);
impl_signed_number!(i32);
impl_signed_number!(i64);
impl_signed_number!(i128);
impl_signed_number!(isize);

impl FromValue for u8 {
  fn from_value(value: Value) -> Result<Self, Error> {
    check_single_bulk_reply!(value);
    to_unsigned_number!(u8, value)
  }

  fn from_owned_bytes(d: Vec<u8>) -> Option<Vec<Self>> {
    Some(d)
  }
}

impl_unsigned_number!(u16);
impl_unsigned_number!(u32);
impl_unsigned_number!(u64);
impl_unsigned_number!(u128);
impl_unsigned_number!(usize);

impl FromValue for String {
  fn from_value(value: Value) -> Result<Self, Error> {
    debug_type!("FromValue(String): {:?}", value);
    check_single_bulk_reply!(value);

    value
      .into_string()
      .ok_or(Error::new_parse("Could not convert to string."))
  }
}

impl FromValue for Str {
  fn from_value(value: Value) -> Result<Self, Error> {
    debug_type!("FromValue(Str): {:?}", value);
    check_single_bulk_reply!(value);

    value
      .into_bytes_str()
      .ok_or(Error::new_parse("Could not convert to string."))
  }
}

impl FromValue for f64 {
  fn from_value(value: Value) -> Result<Self, Error> {
    debug_type!("FromValue(f64): {:?}", value);
    check_single_bulk_reply!(value);

    value.as_f64().ok_or(Error::new_parse("Could not convert to double."))
  }
}

impl FromValue for f32 {
  fn from_value(value: Value) -> Result<Self, Error> {
    debug_type!("FromValue(f32): {:?}", value);
    check_single_bulk_reply!(value);

    value
      .as_f64()
      .map(|f| f as f32)
      .ok_or(Error::new_parse("Could not convert to float."))
  }
}

impl FromValue for bool {
  fn from_value(value: Value) -> Result<Self, Error> {
    debug_type!("FromValue(bool): {:?}", value);
    check_single_bulk_reply!(value);

    if let Some(val) = value.as_bool() {
      Ok(val)
    } else {
      // it's not obvious how to convert the value to a bool in this block, so we go with a
      // tried and true approach that i'm sure we'll never regret - JS semantics
      Ok(match value {
        Value::String(s) => !s.is_empty(),
        Value::Bytes(b) => !b.is_empty(),
        // everything else should be covered by `as_bool` above
        _ => return Err(Error::new_parse("Could not convert to bool.")),
      })
    }
  }
}

impl<T> FromValue for Option<T>
where
  T: FromValue,
{
  fn from_value(value: Value) -> Result<Option<T>, Error> {
    debug_type!("FromValue(Option<{}>): {:?}", type_name::<T>(), value);

    if let Some(0) = value.array_len() {
      Ok(None)
    } else if value.is_null() {
      Ok(None)
    } else {
      Ok(Some(T::from_value(value)?))
    }
  }
}

impl FromValue for Bytes {
  fn from_value(value: Value) -> Result<Self, Error> {
    debug_type!("FromValue(Bytes): {:?}", value);
    check_single_bulk_reply!(value);

    value.into_bytes().ok_or(Error::new_parse("Cannot parse into bytes."))
  }
}

impl<T> FromValue for Vec<T>
where
  T: FromValue,
{
  fn from_value(value: Value) -> Result<Vec<T>, Error> {
    debug_type!("FromValue(Vec<{}>): {:?}", type_name::<T>(), value);

    match value {
      Value::Bytes(bytes) => T::from_owned_bytes(bytes.to_vec()).ok_or(Error::new_parse("Cannot convert from bytes")),
      Value::String(string) => {
        // hacky way to check if T is bytes without consuming `string`
        if T::from_owned_bytes(Vec::new()).is_some() {
          T::from_owned_bytes(string.into_inner().to_vec())
            .ok_or(Error::new_parse("Could not convert string to bytes."))
        } else {
          Ok(vec![T::from_value(Value::String(string))?])
        }
      },
      Value::Array(values) => {
        if !values.is_empty() {
          if let Value::Array(_) = &values[0] {
            values.into_iter().map(|x| T::from_value(x)).collect()
          } else {
            T::from_values(values)
          }
        } else {
          Ok(vec![])
        }
      },
      Value::Map(map) => {
        // not being able to use collect() here is unfortunate
        let out = Vec::with_capacity(map.len() * 2);
        map.inner().into_iter().try_fold(out, |mut out, (key, value)| {
          if T::is_tuple() {
            // try to convert to a 2-element tuple since that's a common use case from `HGETALL`, etc
            out.push(T::from_value(Value::Array(vec![key.into(), value]))?);
          } else {
            out.push(T::from_value(key.into())?);
            out.push(T::from_value(value)?);
          }

          Ok(out)
        })
      },
      Value::Integer(i) => Ok(vec![T::from_value(Value::Integer(i))?]),
      Value::Double(f) => Ok(vec![T::from_value(Value::Double(f))?]),
      Value::Boolean(b) => Ok(vec![T::from_value(Value::Boolean(b))?]),
      Value::Queued => Ok(vec![T::from_value(Value::from_static_str(QUEUED))?]),
      Value::Null => Ok(Vec::new()),
    }
  }
}

impl<T, const N: usize> FromValue for [T; N]
where
  T: FromValue,
{
  fn from_value(value: Value) -> Result<[T; N], Error> {
    debug_type!("FromValue([{}; {}]): {:?}", type_name::<T>(), N, value);
    // use the `from_value` impl for Vec<T>
    let value: Vec<T> = value.convert()?;
    let len = value.len();

    value
      .try_into()
      .map_err(|_| Error::new_parse(format!("Failed to convert to array. Expected {}, found {}.", N, len)))
  }
}

impl<K, V, S> FromValue for HashMap<K, V, S>
where
  K: FromKey + Eq + Hash,
  V: FromValue,
  S: BuildHasher + Default,
{
  fn from_value(value: Value) -> Result<Self, Error> {
    debug_type!(
      "FromValue(HashMap<{}, {}>): {:?}",
      type_name::<K>(),
      type_name::<V>(),
      value
    );

    let as_map = if value.is_array() || value.is_map() || value.is_null() {
      value
        .into_map()
        .map_err(|_| Error::new_parse("Cannot convert to map."))?
    } else {
      return Err(Error::new_parse("Cannot convert to map."));
    };

    as_map
      .inner()
      .into_iter()
      .map(|(k, v)| Ok((K::from_key(k)?, V::from_value(v)?)))
      .collect()
  }
}

impl<V, S> FromValue for HashSet<V, S>
where
  V: FromValue + Hash + Eq,
  S: BuildHasher + Default,
{
  fn from_value(value: Value) -> Result<Self, Error> {
    debug_type!("FromValue(HashSet<{}>): {:?}", type_name::<V>(), value);
    value.into_set()?.into_iter().map(|v| V::from_value(v)).collect()
  }
}

impl<K, V> FromValue for BTreeMap<K, V>
where
  K: FromKey + Ord,
  V: FromValue,
{
  fn from_value(value: Value) -> Result<Self, Error> {
    debug_type!(
      "FromValue(BTreeMap<{}, {}>): {:?}",
      type_name::<K>(),
      type_name::<V>(),
      value
    );
    let as_map = if value.is_array() || value.is_map() || value.is_null() {
      value
        .into_map()
        .map_err(|_| Error::new_parse("Cannot convert to map."))?
    } else {
      return Err(Error::new_parse("Cannot convert to map."));
    };

    as_map
      .inner()
      .into_iter()
      .map(|(k, v)| Ok((K::from_key(k)?, V::from_value(v)?)))
      .collect()
  }
}

impl<V> FromValue for BTreeSet<V>
where
  V: FromValue + Ord,
{
  fn from_value(value: Value) -> Result<Self, Error> {
    debug_type!("FromValue(BTreeSet<{}>): {:?}", type_name::<V>(), value);
    value.into_set()?.into_iter().map(|v| V::from_value(v)).collect()
  }
}

// adapted from mitsuhiko
macro_rules! impl_from_value_tuple {
  () => ();
  ($($name:ident,)+) => (
    #[doc(hidden)]
    impl<$($name: FromValue),*> FromValue for ($($name,)*) {
      fn is_tuple() -> bool {
        true
      }

      #[allow(non_snake_case, unused_variables)]
      fn from_value(v: Value) -> Result<($($name,)*), Error> {
        if let Value::Array(mut values) = v {
          let mut n = 0;
          $(let $name = (); n += 1;)*
          debug_type!("FromValue({}-tuple): {:?}", n, values);
          if values.len() != n {
            return Err(Error::new_parse(format!("Invalid tuple dimension. Expected {}, found {}.", n, values.len())));
          }

          // since we have ownership over the values we have some freedom in how to implement this
          values.reverse();
          Ok(($({let $name = (); values
            .pop()
            .ok_or(Error::new_parse("Expected value, found none."))?
            .convert()?
          },)*))
        }else{
          Err(Error::new_parse("Could not convert to tuple."))
        }
      }

      #[allow(non_snake_case, unused_variables)]
      fn from_values(mut values: Vec<Value>) -> Result<Vec<($($name,)*)>, Error> {
        let mut n = 0;
        $(let $name = (); n += 1;)*
        debug_type!("FromValue({}-tuple): {:?}", n, values);
        if values.len() % n != 0 {
          return Err(Error::new_parse(format!("Invalid tuple dimension. Expected {}, found {}.", n, values.len())));
        }

        let mut out = Vec::with_capacity(values.len() / n);
        // this would be cleaner if there were an owned `chunks` variant
        for chunk in values.chunks_exact_mut(n) {
          match chunk {
            [$($name),*] => out.push(($($name.take().convert()?),*),),
             _ => unreachable!(),
          }
        }

        Ok(out)
      }
    }
    impl_from_value_peel!($($name,)*);
  )
}

macro_rules! impl_from_value_peel {
  ($name:ident, $($other:ident,)*) => (impl_from_value_tuple!($($other,)*);)
}

impl_from_value_tuple! { T1, T2, T3, T4, T5, T6, T7, T8, T9, T10, T11, T12, }

macro_rules! impl_from_str_from_key (
  ($t:ty) => {
    impl FromKey for $t {
      fn from_key(value: Key) -> Result<$t, Error> {
        value
          .as_str()
          .and_then(|k| k.parse::<$t>().ok())
          .ok_or(Error::new_parse("Cannot parse key from bytes."))
      }
    }
  }
);

#[cfg(feature = "serde-json")]
#[cfg_attr(docsrs, doc(cfg(feature = "serde-json")))]
impl FromValue for serde_json::Value {
  fn from_value(value: Value) -> Result<Self, Error> {
    let value = match value {
      Value::Null => serde_json::Value::Null,
      Value::Queued => QUEUED.into(),
      Value::String(s) => {
        // check for nested json. this is particularly useful with JSON.GET
        serde_json::from_str(&s).ok().unwrap_or_else(|| s.to_string().into())
      },
      Value::Bytes(b) => {
        let val = Value::String(Str::from_inner(b)?);
        Self::from_value(val)?
      },
      Value::Integer(i) => i.into(),
      Value::Double(f) => f.into(),
      Value::Boolean(b) => b.into(),
      Value::Array(v) => {
        let mut out = Vec::with_capacity(v.len());
        for value in v.into_iter() {
          out.push(Self::from_value(value)?);
        }
        serde_json::Value::Array(out)
      },
      Value::Map(v) => {
        let mut out = serde_json::Map::with_capacity(v.len());
        for (key, value) in v.inner().into_iter() {
          let key = key
            .into_string()
            .ok_or(Error::new_parse("Cannot convert key to string."))?;
          let value = Self::from_value(value)?;

          out.insert(key, value);
        }
        serde_json::Value::Object(out)
      },
    };

    Ok(value)
  }
}

#[cfg(feature = "i-geo")]
#[cfg_attr(docsrs, doc(cfg(feature = "i-geo")))]
impl FromValue for GeoPosition {
  fn from_value(value: Value) -> Result<Self, Error> {
    GeoPosition::try_from(value)
  }
}

#[cfg(feature = "i-slowlog")]
#[cfg_attr(docsrs, doc(cfg(feature = "i-slowlog")))]
impl FromValue for SlowlogEntry {
  fn from_value(value: Value) -> Result<Self, Error> {
    SlowlogEntry::try_from(value)
  }
}

#[cfg(feature = "i-cluster")]
#[cfg_attr(docsrs, doc(cfg(feature = "i-cluster")))]
impl FromValue for ClusterInfo {
  fn from_value(value: Value) -> Result<Self, Error> {
    ClusterInfo::try_from(value)
  }
}

#[cfg(feature = "i-memory")]
#[cfg_attr(docsrs, doc(cfg(feature = "i-memory")))]
impl FromValue for MemoryStats {
  fn from_value(value: Value) -> Result<Self, Error> {
    MemoryStats::try_from(value)
  }
}

#[cfg(feature = "i-memory")]
#[cfg_attr(docsrs, doc(cfg(feature = "i-memory")))]
impl FromValue for DatabaseMemoryStats {
  fn from_value(value: Value) -> Result<Self, Error> {
    DatabaseMemoryStats::try_from(value)
  }
}

impl FromValue for Key {
  fn from_value(value: Value) -> Result<Self, Error> {
    let key = match value {
      Value::Boolean(b) => b.into(),
      Value::Integer(i) => i.into(),
      Value::Double(f) => f.into(),
      Value::String(s) => s.into(),
      Value::Bytes(b) => b.into(),
      Value::Queued => Key::from_static_str(QUEUED),
      Value::Map(_) | Value::Array(_) => return Err(Error::new_parse("Cannot convert aggregate type to key.")),
      Value::Null => return Err(Error::new(ErrorKind::NotFound, "Cannot convert nil to key.")),
    };

    Ok(key)
  }
}

/// A trait used to convert [Key](crate::types::Key) values to various types.
///
/// See the [convert](crate::types::Key::convert) documentation for more information.
pub trait FromKey: Sized {
  fn from_key(value: Key) -> Result<Self, Error>;
}

impl_from_str_from_key!(u8);
impl_from_str_from_key!(u16);
impl_from_str_from_key!(u32);
impl_from_str_from_key!(u64);
impl_from_str_from_key!(u128);
impl_from_str_from_key!(usize);
impl_from_str_from_key!(i8);
impl_from_str_from_key!(i16);
impl_from_str_from_key!(i32);
impl_from_str_from_key!(i64);
impl_from_str_from_key!(i128);
impl_from_str_from_key!(isize);
impl_from_str_from_key!(f32);
impl_from_str_from_key!(f64);

impl FromKey for () {
  fn from_key(_: Key) -> Result<Self, Error> {
    Ok(())
  }
}

impl FromKey for Value {
  fn from_key(value: Key) -> Result<Self, Error> {
    Ok(Value::Bytes(value.into_bytes()))
  }
}

impl FromKey for Key {
  fn from_key(value: Key) -> Result<Self, Error> {
    Ok(value)
  }
}

impl FromKey for String {
  fn from_key(value: Key) -> Result<Self, Error> {
    value
      .into_string()
      .ok_or(Error::new_parse("Cannot parse key as string."))
  }
}

impl FromKey for Str {
  fn from_key(value: Key) -> Result<Self, Error> {
    Ok(Str::from_inner(value.into_bytes())?)
  }
}

impl FromKey for Vec<u8> {
  fn from_key(value: Key) -> Result<Self, Error> {
    Ok(value.into_bytes().to_vec())
  }
}

impl FromKey for Bytes {
  fn from_key(value: Key) -> Result<Self, Error> {
    Ok(value.into_bytes())
  }
}

#[cfg(test)]
mod tests {
  use crate::types::Value;
  use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

  #[cfg(not(feature = "default-nil-types"))]
  use crate::error::Error;

  #[test]
  fn should_convert_signed_numeric_types() {
    let _foo: i8 = Value::String("123".into()).convert().unwrap();
    assert_eq!(_foo, 123);
    let _foo: i8 = Value::Integer(123).convert().unwrap();
    assert_eq!(_foo, 123);
    let _foo: i16 = Value::String("123".into()).convert().unwrap();
    assert_eq!(_foo, 123);
    let _foo: i16 = Value::Integer(123).convert().unwrap();
    assert_eq!(_foo, 123);
    let _foo: i32 = Value::String("123".into()).convert().unwrap();
    assert_eq!(_foo, 123);
    let _foo: i32 = Value::Integer(123).convert().unwrap();
    assert_eq!(_foo, 123);
    let _foo: i64 = Value::String("123".into()).convert().unwrap();
    assert_eq!(_foo, 123);
    let _foo: i64 = Value::Integer(123).convert().unwrap();
    assert_eq!(_foo, 123);
    let _foo: i128 = Value::String("123".into()).convert().unwrap();
    assert_eq!(_foo, 123);
    let _foo: i128 = Value::Integer(123).convert().unwrap();
    assert_eq!(_foo, 123);
    let _foo: isize = Value::String("123".into()).convert().unwrap();
    assert_eq!(_foo, 123);
    let _foo: isize = Value::Integer(123).convert().unwrap();
    assert_eq!(_foo, 123);
    let _foo: f32 = Value::String("123.5".into()).convert().unwrap();
    assert_eq!(_foo, 123.5);
    let _foo: f64 = Value::String("123.5".into()).convert().unwrap();
    assert_eq!(_foo, 123.5);
  }

  #[test]
  fn should_convert_unsigned_numeric_types() {
    let _foo: u8 = Value::String("123".into()).convert().unwrap();
    assert_eq!(_foo, 123);
    let _foo: u8 = Value::Integer(123).convert().unwrap();
    assert_eq!(_foo, 123);
    let _foo: u16 = Value::String("123".into()).convert().unwrap();
    assert_eq!(_foo, 123);
    let _foo: u16 = Value::Integer(123).convert().unwrap();
    assert_eq!(_foo, 123);
    let _foo: u32 = Value::String("123".into()).convert().unwrap();
    assert_eq!(_foo, 123);
    let _foo: u32 = Value::Integer(123).convert().unwrap();
    assert_eq!(_foo, 123);
    let _foo: u64 = Value::String("123".into()).convert().unwrap();
    assert_eq!(_foo, 123);
    let _foo: u64 = Value::Integer(123).convert().unwrap();
    assert_eq!(_foo, 123);
    let _foo: u128 = Value::String("123".into()).convert().unwrap();
    assert_eq!(_foo, 123);
    let _foo: u128 = Value::Integer(123).convert().unwrap();
    assert_eq!(_foo, 123);
    let _foo: usize = Value::String("123".into()).convert().unwrap();
    assert_eq!(_foo, 123);
    let _foo: usize = Value::Integer(123).convert().unwrap();
    assert_eq!(_foo, 123);
  }

  #[test]
  #[cfg(not(feature = "default-nil-types"))]
  fn should_return_not_found_with_null_number_types() {
    let result: Result<u8, Error> = Value::Null.convert();
    assert!(result.unwrap_err().is_not_found());
    let result: Result<u16, Error> = Value::Null.convert();
    assert!(result.unwrap_err().is_not_found());
    let result: Result<u32, Error> = Value::Null.convert();
    assert!(result.unwrap_err().is_not_found());
    let result: Result<u64, Error> = Value::Null.convert();
    assert!(result.unwrap_err().is_not_found());
    let result: Result<u128, Error> = Value::Null.convert();
    assert!(result.unwrap_err().is_not_found());
    let result: Result<usize, Error> = Value::Null.convert();
    assert!(result.unwrap_err().is_not_found());
    let result: Result<i8, Error> = Value::Null.convert();
    assert!(result.unwrap_err().is_not_found());
    let result: Result<i16, Error> = Value::Null.convert();
    assert!(result.unwrap_err().is_not_found());
    let result: Result<i32, Error> = Value::Null.convert();
    assert!(result.unwrap_err().is_not_found());
    let result: Result<i64, Error> = Value::Null.convert();
    assert!(result.unwrap_err().is_not_found());
    let result: Result<i128, Error> = Value::Null.convert();
    assert!(result.unwrap_err().is_not_found());
    let result: Result<isize, Error> = Value::Null.convert();
    assert!(result.unwrap_err().is_not_found());
  }

  #[test]
  #[cfg(feature = "default-nil-types")]
  fn should_return_zero_with_null_number_types() {
    assert_eq!(0, Value::Null.convert::<u8>().unwrap());
    assert_eq!(0, Value::Null.convert::<u16>().unwrap());
    assert_eq!(0, Value::Null.convert::<u32>().unwrap());
    assert_eq!(0, Value::Null.convert::<u64>().unwrap());
    assert_eq!(0, Value::Null.convert::<u128>().unwrap());
    assert_eq!(0, Value::Null.convert::<usize>().unwrap());
    assert_eq!(0, Value::Null.convert::<i8>().unwrap());
    assert_eq!(0, Value::Null.convert::<i16>().unwrap());
    assert_eq!(0, Value::Null.convert::<i32>().unwrap());
    assert_eq!(0, Value::Null.convert::<i64>().unwrap());
    assert_eq!(0, Value::Null.convert::<i128>().unwrap());
    assert_eq!(0, Value::Null.convert::<isize>().unwrap());
    assert_eq!(0.0, Value::Null.convert::<f32>().unwrap());
    assert_eq!(0.0, Value::Null.convert::<f64>().unwrap());
  }

  #[test]
  #[cfg(feature = "default-nil-types")]
  fn should_convert_null_to_false() {
    assert!(!Value::Null.convert::<bool>().unwrap());
  }

  #[test]
  #[should_panic]
  #[cfg(not(feature = "default-nil-types"))]
  fn should_not_convert_null_to_false() {
    assert!(!Value::Null.convert::<bool>().unwrap());
  }

  #[test]
  fn should_convert_strings() {
    let _foo: String = Value::String("foo".into()).convert().unwrap();
    assert_eq!(_foo, "foo".to_owned());
  }

  #[test]
  fn should_convert_numbers_to_bools() {
    let foo: bool = Value::Integer(0).convert().unwrap();
    assert!(!foo);
    let foo: bool = Value::Integer(1).convert().unwrap();
    assert!(foo);
    let foo: bool = Value::String("0".into()).convert().unwrap();
    assert!(!foo);
    let foo: bool = Value::String("1".into()).convert().unwrap();
    assert!(foo);
  }

  #[test]
  fn should_convert_bytes() {
    let foo: Vec<u8> = Value::Bytes("foo".as_bytes().to_vec().into()).convert().unwrap();
    assert_eq!(foo, "foo".as_bytes().to_vec());
    let foo: Vec<u8> = Value::String("foo".into()).convert().unwrap();
    assert_eq!(foo, "foo".as_bytes().to_vec());
    let foo: Vec<u8> = Value::Array(vec![102.into(), 111.into(), 111.into()])
      .convert()
      .unwrap();
    assert_eq!(foo, "foo".as_bytes().to_vec());
  }

  #[test]
  fn should_convert_arrays() {
    let foo: Vec<String> = Value::Array(vec!["a".into(), "b".into()]).convert().unwrap();
    assert_eq!(foo, vec!["a".to_owned(), "b".to_owned()]);
  }

  #[test]
  fn should_convert_hash_maps() {
    let foo: HashMap<String, u16> = Value::Array(vec!["a".into(), 1.into(), "b".into(), 2.into()])
      .convert()
      .unwrap();

    let mut expected = HashMap::new();
    expected.insert("a".to_owned(), 1);
    expected.insert("b".to_owned(), 2);
    assert_eq!(foo, expected);
  }

  #[test]
  fn should_convert_hash_sets() {
    let foo: HashSet<String> = Value::Array(vec!["a".into(), "b".into()]).convert().unwrap();

    let mut expected = HashSet::new();
    expected.insert("a".to_owned());
    expected.insert("b".to_owned());
    assert_eq!(foo, expected);
  }

  #[test]
  fn should_convert_btree_maps() {
    let foo: BTreeMap<String, u16> = Value::Array(vec!["a".into(), 1.into(), "b".into(), 2.into()])
      .convert()
      .unwrap();

    let mut expected = BTreeMap::new();
    expected.insert("a".to_owned(), 1);
    expected.insert("b".to_owned(), 2);
    assert_eq!(foo, expected);
  }

  #[test]
  fn should_convert_btree_sets() {
    let foo: BTreeSet<String> = Value::Array(vec!["a".into(), "b".into()]).convert().unwrap();

    let mut expected = BTreeSet::new();
    expected.insert("a".to_owned());
    expected.insert("b".to_owned());
    assert_eq!(foo, expected);
  }

  #[test]
  fn should_convert_tuples() {
    let foo: (String, i64) = Value::Array(vec!["a".into(), 1.into()]).convert().unwrap();
    assert_eq!(foo, ("a".to_owned(), 1));
  }

  #[test]
  fn should_convert_array_tuples() {
    let foo: Vec<(String, i64)> = Value::Array(vec!["a".into(), 1.into(), "b".into(), 2.into()])
      .convert()
      .unwrap();
    assert_eq!(foo, vec![("a".to_owned(), 1), ("b".to_owned(), 2)]);
  }

  #[test]
  fn should_handle_single_element_vector_to_scalar() {
    assert!(Value::Array(vec![]).convert::<String>().is_err());
    assert_eq!(Value::Array(vec!["foo".into()]).convert::<String>(), Ok("foo".into()));
    assert!(Value::Array(vec!["foo".into(), "bar".into()])
      .convert::<String>()
      .is_err());

    assert_eq!(Value::Array(vec![]).convert::<Option<String>>(), Ok(None));
    assert_eq!(
      Value::Array(vec!["foo".into()]).convert::<Option<String>>(),
      Ok(Some("foo".into()))
    );
    assert!(Value::Array(vec!["foo".into(), "bar".into()])
      .convert::<Option<String>>()
      .is_err());
  }

  #[test]
  fn should_convert_null_to_empty_array() {
    assert_eq!(Vec::<String>::new(), Value::Null.convert::<Vec<String>>().unwrap());
    assert_eq!(Vec::<u8>::new(), Value::Null.convert::<Vec<u8>>().unwrap());
  }

  #[test]
  fn should_convert_to_fixed_arrays() {
    let foo: [i64; 2] = Value::Array(vec![1.into(), 2.into()]).convert().unwrap();
    assert_eq!(foo, [1, 2]);

    assert!(Value::Array(vec![1.into(), 2.into()]).convert::<[i64; 3]>().is_err());
    assert!(Value::Array(vec![]).convert::<[i64; 3]>().is_err());
  }
}
