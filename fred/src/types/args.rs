use crate::{
  error::{Error, ErrorKind},
  interfaces::{ClientLike, Resp3Frame},
  protocol::{connection::OK, utils as protocol_utils},
  types::{config::Server, FromKey, FromValue, QUEUED},
  utils,
};
use bytes::Bytes;
use bytes_utils::Str;
use float_cmp::approx_eq;
use redis_protocol::resp2::types::NULL;
use std::{
  borrow::Cow,
  collections::{BTreeMap, HashMap, HashSet, VecDeque},
  convert::{TryFrom, TryInto},
  fmt,
  hash::{Hash, Hasher},
  iter::FromIterator,
  mem,
  ops::{Deref, DerefMut},
  str,
};

#[cfg(feature = "i-geo")]
use crate::types::geo::{GeoPosition, GeoRadiusInfo};
#[cfg(feature = "i-scripts")]
use crate::types::scripts::Function;
#[cfg(feature = "i-streams")]
use crate::types::streams::{XReadResponse, XReadValue};

static TRUE_STR: Str = utils::static_str("true");
static FALSE_STR: Str = utils::static_str("false");

macro_rules! impl_string_or_number(
    ($t:ty) => {
        impl From<$t> for StringOrNumber {
            fn from(val: $t) -> Self {
                StringOrNumber::Number(val as i64)
            }
        }
    }
);

macro_rules! impl_from_str_for_key(
    ($t:ty) => {
        impl From<$t> for Key {
            fn from(val: $t) -> Self {
                Key { key: val.to_string().into() }
            }
        }
    }
);

/// An argument representing a string or number.
#[derive(Clone, Debug)]
pub enum StringOrNumber {
  String(Str),
  Number(i64),
  Double(f64),
}

impl PartialEq for StringOrNumber {
  fn eq(&self, other: &Self) -> bool {
    match *self {
      StringOrNumber::String(ref s) => match *other {
        StringOrNumber::String(ref _s) => s == _s,
        _ => false,
      },
      StringOrNumber::Number(ref i) => match *other {
        StringOrNumber::Number(ref _i) => *i == *_i,
        _ => false,
      },
      StringOrNumber::Double(ref d) => match *other {
        StringOrNumber::Double(ref _d) => utils::f64_eq(*d, *_d),
        _ => false,
      },
    }
  }
}

impl Eq for StringOrNumber {}

impl StringOrNumber {
  /// An optimized way to convert from `&'static str` that avoids copying or moving the underlying bytes.
  pub fn from_static_str(s: &'static str) -> Self {
    StringOrNumber::String(utils::static_str(s))
  }

  #[cfg(feature = "i-streams")]
  pub(crate) fn into_arg(self) -> Value {
    match self {
      StringOrNumber::String(s) => Value::String(s),
      StringOrNumber::Number(n) => Value::Integer(n),
      StringOrNumber::Double(f) => Value::Double(f),
    }
  }
}

impl TryFrom<Value> for StringOrNumber {
  type Error = Error;

  fn try_from(value: Value) -> Result<Self, Self::Error> {
    let val = match value {
      Value::String(s) => StringOrNumber::String(s),
      Value::Integer(i) => StringOrNumber::Number(i),
      Value::Double(f) => StringOrNumber::Double(f),
      Value::Bytes(b) => StringOrNumber::String(Str::from_inner(b)?),
      _ => return Err(Error::new(ErrorKind::InvalidArgument, "")),
    };

    Ok(val)
  }
}

impl<'a> From<&'a str> for StringOrNumber {
  fn from(s: &'a str) -> Self {
    StringOrNumber::String(s.into())
  }
}

impl From<String> for StringOrNumber {
  fn from(s: String) -> Self {
    StringOrNumber::String(s.into())
  }
}

impl From<Str> for StringOrNumber {
  fn from(s: Str) -> Self {
    StringOrNumber::String(s)
  }
}

impl_string_or_number!(i8);
impl_string_or_number!(i16);
impl_string_or_number!(i32);
impl_string_or_number!(i64);
impl_string_or_number!(isize);
impl_string_or_number!(u8);
impl_string_or_number!(u16);
impl_string_or_number!(u32);
impl_string_or_number!(u64);
impl_string_or_number!(usize);

impl From<f32> for StringOrNumber {
  fn from(f: f32) -> Self {
    StringOrNumber::Double(f as f64)
  }
}

impl From<f64> for StringOrNumber {
  fn from(f: f64) -> Self {
    StringOrNumber::Double(f)
  }
}

/// A key identifying a [Value](crate::types::Value).
#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct Key {
  key: Bytes,
}

impl Key {
  /// Create a new `Key` from static bytes without copying.
  pub const fn from_static(b: &'static [u8]) -> Self {
    Key {
      key: Bytes::from_static(b),
    }
  }

  /// Create a new `Key` from a `&'static str` without copying.
  pub const fn from_static_str(b: &'static str) -> Self {
    Key {
      key: Bytes::from_static(b.as_bytes()),
    }
  }

  /// Read the key as a str slice if it can be parsed as a UTF8 string.
  pub fn as_str(&self) -> Option<&str> {
    str::from_utf8(&self.key).ok()
  }

  /// Read the key as a byte slice.
  pub fn as_bytes(&self) -> &[u8] {
    &self.key
  }

  /// Read the inner `Bytes` struct.
  pub fn inner(&self) -> &Bytes {
    &self.key
  }

  /// Read the key as a lossy UTF8 string with `String::from_utf8_lossy`.
  pub fn as_str_lossy(&self) -> Cow<str> {
    String::from_utf8_lossy(&self.key)
  }

  /// Convert the key to a UTF8 string, if possible.
  pub fn into_string(self) -> Option<String> {
    String::from_utf8(self.key.to_vec()).ok()
  }

  /// Read the inner bytes making up the key.
  pub fn into_bytes(self) -> Bytes {
    self.key
  }

  /// Parse and return the key as a `Str` without copying the inner contents.
  pub fn as_bytes_str(&self) -> Option<Str> {
    Str::from_inner(self.key.clone()).ok()
  }

  /// Hash the key to find the associated cluster [hash slot](https://redis.io/topics/cluster-spec#keys-distribution-model).
  pub fn cluster_hash(&self) -> u16 {
    redis_protocol::redis_keyslot(&self.key)
  }

  /// Read the `host:port` of the cluster node that owns the key if the client is clustered and the cluster state is
  /// known.
  pub fn cluster_owner<C>(&self, client: &C) -> Option<Server>
  where
    C: ClientLike,
  {
    if client.is_clustered() {
      let hash_slot = self.cluster_hash();
      client
        .inner()
        .with_cluster_state(|state| Ok(state.get_server(hash_slot).cloned()))
        .ok()
        .and_then(|server| server)
    } else {
      None
    }
  }

  /// Replace this key with an empty byte array, returning the bytes from the original key.
  pub fn take(&mut self) -> Bytes {
    self.key.split_to(self.key.len())
  }

  /// Attempt to convert the key to any type that implements [FromKey](crate::types::FromKey).
  ///
  /// See the [Value::convert](crate::types::Value::convert) documentation for more information.
  pub fn convert<K>(self) -> Result<K, Error>
  where
    K: FromKey,
  {
    K::from_key(self)
  }
}

impl TryFrom<Value> for Key {
  type Error = Error;

  fn try_from(value: Value) -> Result<Self, Self::Error> {
    let val = match value {
      Value::String(s) => Key { key: s.into_inner() },
      Value::Integer(i) => Key {
        key: i.to_string().into(),
      },
      Value::Double(f) => Key {
        key: f.to_string().into(),
      },
      Value::Bytes(b) => Key { key: b },
      Value::Boolean(b) => match b {
        true => Key {
          key: TRUE_STR.clone().into_inner(),
        },
        false => Key {
          key: FALSE_STR.clone().into_inner(),
        },
      },
      Value::Queued => utils::static_str(QUEUED).into(),
      _ => return Err(Error::new(ErrorKind::InvalidArgument, "Cannot convert to key.")),
    };

    Ok(val)
  }
}

impl From<Bytes> for Key {
  fn from(b: Bytes) -> Self {
    Key { key: b }
  }
}

impl From<Box<[u8]>> for Key {
  fn from(b: Box<[u8]>) -> Self {
    Key { key: b.into() }
  }
}

impl<'a> From<&'a [u8]> for Key {
  fn from(b: &'a [u8]) -> Self {
    Key { key: b.to_vec().into() }
  }
}

impl From<String> for Key {
  fn from(s: String) -> Self {
    Key { key: s.into() }
  }
}

impl From<&str> for Key {
  fn from(s: &str) -> Self {
    Key {
      key: s.as_bytes().to_vec().into(),
    }
  }
}

impl From<&String> for Key {
  fn from(s: &String) -> Self {
    Key { key: s.clone().into() }
  }
}

impl From<Str> for Key {
  fn from(s: Str) -> Self {
    Key { key: s.into_inner() }
  }
}

impl From<&Str> for Key {
  fn from(s: &Str) -> Self {
    Key { key: s.inner().clone() }
  }
}

impl From<&Key> for Key {
  fn from(k: &Key) -> Key {
    k.clone()
  }
}

impl From<bool> for Key {
  fn from(b: bool) -> Self {
    match b {
      true => Key::from_static_str("true"),
      false => Key::from_static_str("false"),
    }
  }
}

impl_from_str_for_key!(u8);
impl_from_str_for_key!(u16);
impl_from_str_for_key!(u32);
impl_from_str_for_key!(u64);
impl_from_str_for_key!(u128);
impl_from_str_for_key!(usize);
impl_from_str_for_key!(i8);
impl_from_str_for_key!(i16);
impl_from_str_for_key!(i32);
impl_from_str_for_key!(i64);
impl_from_str_for_key!(i128);
impl_from_str_for_key!(isize);
impl_from_str_for_key!(f32);
impl_from_str_for_key!(f64);

/// A map of `(Key, Value)` pairs.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Map {
  pub(crate) inner: HashMap<Key, Value>,
}

impl Map {
  /// Create a new empty map.
  pub fn new() -> Self {
    Map { inner: HashMap::new() }
  }

  /// Replace the value an empty map, returning the original value.
  pub fn take(&mut self) -> Self {
    Map {
      inner: mem::take(&mut self.inner),
    }
  }

  /// Read the number of (key, value) pairs in the map.
  pub fn len(&self) -> usize {
    self.inner.len()
  }

  /// Take the inner `HashMap`.
  pub fn inner(self) -> HashMap<Key, Value> {
    self.inner
  }
}

impl Deref for Map {
  type Target = HashMap<Key, Value>;

  fn deref(&self) -> &Self::Target {
    &self.inner
  }
}

impl DerefMut for Map {
  fn deref_mut(&mut self) -> &mut Self::Target {
    &mut self.inner
  }
}

impl<'a> From<&'a Map> for Map {
  fn from(vals: &'a Map) -> Self {
    vals.clone()
  }
}

impl<K, V> TryFrom<HashMap<K, V>> for Map
where
  K: TryInto<Key>,
  K::Error: Into<Error>,
  V: TryInto<Value>,
  V::Error: Into<Error>,
{
  type Error = Error;

  fn try_from(value: HashMap<K, V>) -> Result<Self, Self::Error> {
    Ok(Map {
      inner: utils::into_map(value.into_iter())?,
    })
  }
}

impl<K, V> TryFrom<BTreeMap<K, V>> for Map
where
  K: TryInto<Key>,
  K::Error: Into<Error>,
  V: TryInto<Value>,
  V::Error: Into<Error>,
{
  type Error = Error;

  fn try_from(value: BTreeMap<K, V>) -> Result<Self, Self::Error> {
    Ok(Map {
      inner: utils::into_map(value.into_iter())?,
    })
  }
}

impl From<()> for Map {
  fn from(_: ()) -> Self {
    Map::new()
  }
}

impl<K, V> TryFrom<(K, V)> for Map
where
  K: TryInto<Key>,
  K::Error: Into<Error>,
  V: TryInto<Value>,
  V::Error: Into<Error>,
{
  type Error = Error;

  fn try_from((key, value): (K, V)) -> Result<Self, Self::Error> {
    let mut inner = HashMap::with_capacity(1);
    inner.insert(to!(key)?, to!(value)?);
    Ok(Map { inner })
  }
}

impl<K, V> TryFrom<Vec<(K, V)>> for Map
where
  K: TryInto<Key>,
  K::Error: Into<Error>,
  V: TryInto<Value>,
  V::Error: Into<Error>,
{
  type Error = Error;

  fn try_from(values: Vec<(K, V)>) -> Result<Self, Self::Error> {
    let mut inner = HashMap::with_capacity(values.len());
    for (key, value) in values.into_iter() {
      inner.insert(to!(key)?, to!(value)?);
    }
    Ok(Map { inner })
  }
}

impl<K, V, const N: usize> TryFrom<[(K, V); N]> for Map
where
  K: TryInto<Key>,
  K::Error: Into<Error>,
  V: TryInto<Value>,
  V::Error: Into<Error>,
{
  type Error = Error;

  fn try_from(value: [(K, V); N]) -> Result<Self, Self::Error> {
    let mut inner = HashMap::with_capacity(value.len());
    for (key, value) in value.into_iter() {
      inner.insert(to!(key)?, to!(value)?);
    }

    Ok(Map { inner })
  }
}
impl<'a, K, V, const N: usize> TryFrom<&'a [(K, V); N]> for Map
where
  K: TryInto<Key> + Clone,
  K::Error: Into<Error>,
  V: TryInto<Value> + Clone,
  V::Error: Into<Error>,
{
  type Error = Error;

  fn try_from(value: &'a [(K, V); N]) -> Result<Self, Self::Error> {
    let mut inner = HashMap::with_capacity(value.len());
    for (key, value) in value.iter() {
      let (key, value) = (key.clone(), value.clone());
      inner.insert(to!(key)?, to!(value)?);
    }

    Ok(Map { inner })
  }
}

impl<K, V> TryFrom<VecDeque<(K, V)>> for Map
where
  K: TryInto<Key>,
  K::Error: Into<Error>,
  V: TryInto<Value>,
  V::Error: Into<Error>,
{
  type Error = Error;

  fn try_from(values: VecDeque<(K, V)>) -> Result<Self, Self::Error> {
    let mut inner = HashMap::with_capacity(values.len());
    for (key, value) in values.into_iter() {
      inner.insert(to!(key)?, to!(value)?);
    }
    Ok(Map { inner })
  }
}

impl<K, V> FromIterator<(K, V)> for Map
where
  K: Into<Key>,
  V: Into<Value>,
{
  fn from_iter<T: IntoIterator<Item = (K, V)>>(iter: T) -> Self {
    Self {
      inner: HashMap::from_iter(iter.into_iter().map(|(k, v)| (k.into(), v.into()))),
    }
  }
}

/// The kind of value from the server.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ValueKind {
  Boolean,
  Integer,
  Double,
  String,
  Bytes,
  Null,
  Queued,
  Map,
  Array,
}

impl fmt::Display for ValueKind {
  fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
    let s = match *self {
      ValueKind::Boolean => "Boolean",
      ValueKind::Integer => "Integer",
      ValueKind::Double => "Double",
      ValueKind::String => "String",
      ValueKind::Bytes => "Bytes",
      ValueKind::Null => "nil",
      ValueKind::Queued => "Queued",
      ValueKind::Map => "Map",
      ValueKind::Array => "Array",
    };

    write!(f, "{}", s)
  }
}

/// A value used in arguments or response types.
#[derive(Clone, Debug)]
pub enum Value {
  /// A boolean value.
  Boolean(bool),
  /// An integer value.
  Integer(i64),
  /// A double floating point number.
  Double(f64),
  /// A string value.
  String(Str),
  /// A byte array value.
  Bytes(Bytes),
  /// A `nil` value.
  Null,
  /// A special value used to indicate a MULTI block command was received by the server.
  Queued,
  /// A map of key/value pairs, primarily used in RESP3 mode.
  Map(Map),
  /// An ordered list of values.
  ///
  /// In RESP2 mode the server usually sends map structures as an array of key/value pairs.
  Array(Vec<Value>),
}

#[allow(clippy::match_like_matches_macro)]
impl PartialEq for Value {
  fn eq(&self, other: &Self) -> bool {
    use Value::*;

    match self {
      Boolean(ref s) => match other {
        Boolean(ref o) => *s == *o,
        _ => false,
      },
      Integer(ref s) => match other {
        Integer(ref o) => *s == *o,
        _ => false,
      },
      Double(ref s) => match other {
        Double(ref o) => approx_eq!(f64, *s, *o, ulps = 2),
        _ => false,
      },
      String(ref s) => match other {
        String(ref o) => s == o,
        _ => false,
      },
      Bytes(ref s) => match other {
        Bytes(ref o) => s == o,
        _ => false,
      },
      Null => match other {
        Null => true,
        _ => false,
      },
      Queued => match other {
        Queued => true,
        _ => false,
      },
      Map(ref s) => match other {
        Map(ref o) => s == o,
        _ => false,
      },
      Array(ref s) => match other {
        Array(ref o) => s == o,
        _ => false,
      },
    }
  }
}

impl Eq for Value {}

impl Value {
  /// Create a new `Value::Bytes` from a static byte slice without copying.
  pub fn from_static(b: &'static [u8]) -> Self {
    Value::Bytes(Bytes::from_static(b))
  }

  /// Create a new `Value::String` from a static `str` without copying.
  pub fn from_static_str(s: &'static str) -> Self {
    Value::String(utils::static_str(s))
  }

  /// Create a new `Value` with the `OK` status.
  pub fn new_ok() -> Self {
    Self::from_static_str(OK)
  }

  /// Whether the value is a simple string OK value.
  pub fn is_ok(&self) -> bool {
    match *self {
      Value::String(ref s) => *s == OK,
      _ => false,
    }
  }

  /// Attempt to convert the value into an integer, returning the original string as an error if the parsing fails.
  pub fn into_integer(self) -> Result<Value, Value> {
    match self {
      Value::String(s) => match s.parse::<i64>() {
        Ok(i) => Ok(Value::Integer(i)),
        Err(_) => Err(Value::String(s)),
      },
      Value::Integer(i) => Ok(Value::Integer(i)),
      _ => Err(self),
    }
  }

  /// Read the type of the value without any associated data.
  pub fn kind(&self) -> ValueKind {
    match *self {
      Value::Boolean(_) => ValueKind::Boolean,
      Value::Integer(_) => ValueKind::Integer,
      Value::Double(_) => ValueKind::Double,
      Value::String(_) => ValueKind::String,
      Value::Bytes(_) => ValueKind::Bytes,
      Value::Null => ValueKind::Null,
      Value::Queued => ValueKind::Queued,
      Value::Map(_) => ValueKind::Map,
      Value::Array(_) => ValueKind::Array,
    }
  }

  /// Check if the value is null.
  pub fn is_null(&self) -> bool {
    matches!(*self, Value::Null)
  }

  /// Check if the value is an integer.
  pub fn is_integer(&self) -> bool {
    matches!(self, Value::Integer(_))
  }

  /// Check if the value is a string.
  pub fn is_string(&self) -> bool {
    matches!(*self, Value::String(_))
  }

  /// Check if the value is an array of bytes.
  pub fn is_bytes(&self) -> bool {
    matches!(*self, Value::Bytes(_))
  }

  /// Whether the value is a boolean value or can be parsed as a boolean value.
  #[allow(clippy::match_like_matches_macro)]
  pub fn is_boolean(&self) -> bool {
    match *self {
      Value::Boolean(_) => true,
      Value::Integer(0 | 1) => true,
      Value::Integer(_) => false,
      Value::String(ref s) => match s.as_bytes() {
        b"true" | b"false" | b"t" | b"f" | b"TRUE" | b"FALSE" | b"T" | b"F" | b"1" | b"0" => true,
        _ => false,
      },
      _ => false,
    }
  }

  /// Whether the inner value is a double or can be parsed as a double.
  pub fn is_double(&self) -> bool {
    match *self {
      Value::Double(_) => true,
      Value::String(ref s) => utils::string_to_f64(s).is_ok(),
      _ => false,
    }
  }

  /// Check if the value is a `QUEUED` response.
  pub fn is_queued(&self) -> bool {
    matches!(*self, Value::Queued)
  }

  /// Whether the value is an array or map.
  pub fn is_aggregate_type(&self) -> bool {
    matches!(*self, Value::Array(_) | Value::Map(_))
  }

  /// Whether the value is a `Map`.
  ///
  /// See [is_maybe_map](Self::is_maybe_map) for a function that also checks for arrays that likely represent a map in
  /// RESP2 mode.
  pub fn is_map(&self) -> bool {
    matches!(*self, Value::Map(_))
  }

  /// Whether the value is a `Map` or an array with an even number of elements where each even-numbered
  /// element is not an aggregate type.
  ///
  /// RESP2 and RESP3 encode maps differently, and this function can be used to duck-type maps across protocol
  /// versions.
  pub fn is_maybe_map(&self) -> bool {
    match *self {
      Value::Map(_) => true,
      Value::Array(ref arr) => utils::is_maybe_array_map(arr),
      _ => false,
    }
  }

  /// Whether the value is an array.
  pub fn is_array(&self) -> bool {
    matches!(*self, Value::Array(_))
  }

  /// Read and return the inner value as a `u64`, if possible.
  pub fn as_u64(&self) -> Option<u64> {
    match self {
      Value::Integer(ref i) => {
        if *i >= 0 {
          Some(*i as u64)
        } else {
          None
        }
      },
      Value::String(ref s) => s.parse::<u64>().ok(),
      Value::Array(ref inner) => {
        if inner.len() == 1 {
          inner.first().and_then(|v| v.as_u64())
        } else {
          None
        }
      },
      #[cfg(feature = "default-nil-types")]
      Value::Null => Some(0),
      #[cfg(not(feature = "default-nil-types"))]
      Value::Null => None,
      _ => None,
    }
  }

  ///  Read and return the inner value as a `i64`, if possible.
  pub fn as_i64(&self) -> Option<i64> {
    match self {
      Value::Integer(ref i) => Some(*i),
      Value::String(ref s) => s.parse::<i64>().ok(),
      Value::Array(ref inner) => {
        if inner.len() == 1 {
          inner.first().and_then(|v| v.as_i64())
        } else {
          None
        }
      },
      #[cfg(feature = "default-nil-types")]
      Value::Null => Some(0),
      #[cfg(not(feature = "default-nil-types"))]
      Value::Null => None,
      _ => None,
    }
  }

  ///  Read and return the inner value as a `usize`, if possible.
  pub fn as_usize(&self) -> Option<usize> {
    match self {
      Value::Integer(i) => {
        if *i >= 0 {
          Some(*i as usize)
        } else {
          None
        }
      },
      Value::String(ref s) => s.parse::<usize>().ok(),
      Value::Array(ref inner) => {
        if inner.len() == 1 {
          inner.first().and_then(|v| v.as_usize())
        } else {
          None
        }
      },
      #[cfg(feature = "default-nil-types")]
      Value::Null => Some(0),
      #[cfg(not(feature = "default-nil-types"))]
      Value::Null => None,
      _ => None,
    }
  }

  ///  Read and return the inner value as a `f64`, if possible.
  pub fn as_f64(&self) -> Option<f64> {
    match self {
      Value::Double(ref f) => Some(*f),
      Value::String(ref s) => utils::string_to_f64(s).ok(),
      Value::Integer(ref i) => Some(*i as f64),
      Value::Array(ref inner) => {
        if inner.len() == 1 {
          inner.first().and_then(|v| v.as_f64())
        } else {
          None
        }
      },
      #[cfg(feature = "default-nil-types")]
      Value::Null => Some(0.0),
      #[cfg(not(feature = "default-nil-types"))]
      Value::Null => None,
      _ => None,
    }
  }

  /// Read and return the inner `String` if the value is a string or scalar value.
  pub fn into_string(self) -> Option<String> {
    match self {
      Value::Boolean(b) => Some(b.to_string()),
      Value::Double(f) => Some(f.to_string()),
      Value::String(s) => Some(s.to_string()),
      Value::Bytes(b) => String::from_utf8(b.to_vec()).ok(),
      Value::Integer(i) => Some(i.to_string()),
      Value::Queued => Some(QUEUED.to_owned()),
      Value::Array(mut inner) => {
        if inner.len() == 1 {
          inner.pop().and_then(|v| v.into_string())
        } else {
          None
        }
      },
      #[cfg(feature = "default-nil-types")]
      Value::Null => Some(String::new()),
      #[cfg(not(feature = "default-nil-types"))]
      Value::Null => None,
      _ => None,
    }
  }

  /// Read and return the inner data as a `Str` from the `bytes` crate.
  pub fn into_bytes_str(self) -> Option<Str> {
    match self {
      Value::Boolean(b) => match b {
        true => Some(TRUE_STR.clone()),
        false => Some(FALSE_STR.clone()),
      },
      Value::Double(f) => Some(f.to_string().into()),
      Value::String(s) => Some(s),
      Value::Bytes(b) => Str::from_inner(b).ok(),
      Value::Integer(i) => Some(i.to_string().into()),
      Value::Queued => Some(utils::static_str(QUEUED)),
      Value::Array(mut inner) => {
        if inner.len() == 1 {
          inner.pop().and_then(|v| v.into_bytes_str())
        } else {
          None
        }
      },
      #[cfg(feature = "default-nil-types")]
      Value::Null => Some(Str::new()),
      #[cfg(not(feature = "default-nil-types"))]
      Value::Null => None,
      _ => None,
    }
  }

  /// Read the inner value as a `Str`.
  pub fn as_bytes_str(&self) -> Option<Str> {
    match self {
      Value::Boolean(ref b) => match *b {
        true => Some(TRUE_STR.clone()),
        false => Some(FALSE_STR.clone()),
      },
      Value::Double(ref f) => Some(f.to_string().into()),
      Value::String(ref s) => Some(s.clone()),
      Value::Bytes(ref b) => Str::from_inner(b.clone()).ok(),
      Value::Integer(ref i) => Some(i.to_string().into()),
      Value::Queued => Some(utils::static_str(QUEUED)),
      Value::Array(ref inner) => {
        if inner.len() == 1 {
          inner[0].as_bytes_str()
        } else {
          None
        }
      },
      #[cfg(feature = "default-nil-types")]
      Value::Null => Some(Str::new()),
      #[cfg(not(feature = "default-nil-types"))]
      Value::Null => None,
      _ => None,
    }
  }

  /// Read and return the inner `String` if the value is a string or scalar value.
  ///
  /// Note: this will cast integers and doubles to strings.
  pub fn as_string(&self) -> Option<String> {
    match self {
      Value::Boolean(ref b) => Some(b.to_string()),
      Value::Double(ref f) => Some(f.to_string()),
      Value::String(ref s) => Some(s.to_string()),
      Value::Bytes(ref b) => str::from_utf8(b).ok().map(|s| s.to_owned()),
      Value::Integer(ref i) => Some(i.to_string()),
      Value::Queued => Some(QUEUED.to_owned()),
      #[cfg(feature = "default-nil-types")]
      Value::Null => Some(String::new()),
      #[cfg(not(feature = "default-nil-types"))]
      Value::Null => None,
      _ => None,
    }
  }

  /// Read the inner value as a string slice.
  ///
  /// Null is returned as `"nil"` and scalar values are cast to a string.
  pub fn as_str(&self) -> Option<Cow<str>> {
    let s: Cow<str> = match *self {
      Value::Double(ref f) => Cow::Owned(f.to_string()),
      Value::Boolean(ref b) => Cow::Owned(b.to_string()),
      Value::String(ref s) => Cow::Borrowed(s.deref()),
      Value::Integer(ref i) => Cow::Owned(i.to_string()),
      Value::Queued => Cow::Borrowed(QUEUED),
      Value::Bytes(ref b) => return str::from_utf8(b).ok().map(Cow::Borrowed),
      #[cfg(feature = "default-nil-types")]
      Value::Null => Cow::Borrowed(""),
      #[cfg(not(feature = "default-nil-types"))]
      Value::Null => return None,
      _ => return None,
    };

    Some(s)
  }

  /// Read the inner value as a string, using `String::from_utf8_lossy` on byte slices.
  pub fn as_str_lossy(&self) -> Option<Cow<str>> {
    let s: Cow<str> = match *self {
      Value::Boolean(ref b) => Cow::Owned(b.to_string()),
      Value::Double(ref f) => Cow::Owned(f.to_string()),
      Value::String(ref s) => Cow::Borrowed(s.deref()),
      Value::Integer(ref i) => Cow::Owned(i.to_string()),
      Value::Queued => Cow::Borrowed(QUEUED),
      Value::Bytes(ref b) => String::from_utf8_lossy(b),
      #[cfg(feature = "default-nil-types")]
      Value::Null => Cow::Borrowed(""),
      #[cfg(not(feature = "default-nil-types"))]
      Value::Null => return None,
      _ => return None,
    };

    Some(s)
  }

  /// Read the inner value as an array of bytes, if possible.
  pub fn as_bytes(&self) -> Option<&[u8]> {
    match *self {
      Value::String(ref s) => Some(s.as_bytes()),
      Value::Bytes(ref b) => Some(b),
      Value::Queued => Some(QUEUED.as_bytes()),
      _ => None,
    }
  }

  /// Attempt to convert the value to a `bool`.
  pub fn as_bool(&self) -> Option<bool> {
    match *self {
      Value::Boolean(b) => Some(b),
      Value::Integer(ref i) => match *i {
        0 => Some(false),
        1 => Some(true),
        _ => None,
      },
      Value::String(ref s) => match s.as_bytes() {
        b"true" | b"TRUE" | b"t" | b"T" | b"1" => Some(true),
        b"false" | b"FALSE" | b"f" | b"F" | b"0" => Some(false),
        _ => None,
      },
      Value::Array(ref inner) => {
        if inner.len() == 1 {
          inner.first().and_then(|v| v.as_bool())
        } else {
          None
        }
      },
      #[cfg(feature = "default-nil-types")]
      Value::Null => Some(false),
      #[cfg(not(feature = "default-nil-types"))]
      Value::Null => None,
      _ => None,
    }
  }

  /// Attempt to convert this value to a map if it's an array with an even number of elements.
  pub fn into_map(self) -> Result<Map, Error> {
    match self {
      Value::Map(map) => Ok(map),
      Value::Array(mut values) => {
        if values.len() % 2 != 0 {
          return Err(Error::new(ErrorKind::Unknown, "Expected an even number of elements."));
        }
        let mut inner = HashMap::with_capacity(values.len() / 2);
        while values.len() >= 2 {
          let value = values.pop().unwrap();
          let key: Key = values.pop().unwrap().try_into()?;

          inner.insert(key, value);
        }

        Ok(Map { inner })
      },
      #[cfg(feature = "default-nil-types")]
      Value::Null => Ok(Map::new()),
      _ => Err(Error::new(ErrorKind::Unknown, "Could not convert to map.")),
    }
  }

  pub(crate) fn into_multiple_values(self) -> Vec<Value> {
    match self {
      Value::Array(values) => values,
      Value::Map(map) => map
        .inner()
        .into_iter()
        .flat_map(|(k, v)| [Value::Bytes(k.into_bytes()), v])
        .collect(),
      Value::Null => Vec::new(),
      _ => vec![self],
    }
  }

  /// Convert the array value to a set, if possible.
  pub fn into_set(self) -> Result<HashSet<Value>, Error> {
    match self {
      Value::Array(values) => Ok(values.into_iter().collect()),
      #[cfg(feature = "default-nil-types")]
      Value::Null => Ok(HashSet::new()),
      _ => Err(Error::new_parse("Could not convert to set.")),
    }
  }

  /// Convert a `Value` to `Vec<(Value, f64)>`, if possible.
  pub fn into_zset_result(self) -> Result<Vec<(Value, f64)>, Error> {
    protocol_utils::value_to_zset_result(self)
  }

  /// Convert this value to an array if it's an array or map.
  ///
  /// If the value is not an array or map this returns a single-element array containing the original value.
  pub fn into_array(self) -> Vec<Value> {
    match self {
      Value::Array(values) => values,
      Value::Map(map) => {
        let mut out = Vec::with_capacity(map.len() * 2);
        for (key, value) in map.inner().into_iter() {
          out.extend([key.into(), value]);
        }
        out
      },
      _ => vec![self],
    }
  }

  /// Convert the value to an array of bytes, if possible.
  pub fn into_owned_bytes(self) -> Option<Vec<u8>> {
    let v = match self {
      Value::String(s) => s.to_string().into_bytes(),
      Value::Bytes(b) => b.to_vec(),
      Value::Queued => QUEUED.as_bytes().to_vec(),
      Value::Array(mut inner) => {
        if inner.len() == 1 {
          return inner.pop().and_then(|v| v.into_owned_bytes());
        } else {
          return None;
        }
      },
      Value::Integer(i) => i.to_string().into_bytes(),
      #[cfg(feature = "default-nil-types")]
      Value::Null => Vec::new(),
      #[cfg(not(feature = "default-nil-types"))]
      Value::Null => return None,
      _ => return None,
    };

    Some(v)
  }

  /// Convert the value into a `Bytes` view.
  pub fn into_bytes(self) -> Option<Bytes> {
    let v = match self {
      Value::String(s) => s.inner().clone(),
      Value::Bytes(b) => b,
      Value::Queued => Bytes::from_static(QUEUED.as_bytes()),
      Value::Array(mut inner) => {
        if inner.len() == 1 {
          return inner.pop().and_then(|v| v.into_bytes());
        } else {
          return None;
        }
      },
      Value::Integer(i) => i.to_string().into(),
      #[cfg(feature = "default-nil-types")]
      Value::Null => Bytes::new(),
      #[cfg(not(feature = "default-nil-types"))]
      Value::Null => return None,
      _ => return None,
    };

    Some(v)
  }

  /// Return the length of the inner array if the value is an array.
  pub fn array_len(&self) -> Option<usize> {
    match self {
      Value::Array(ref a) => Some(a.len()),
      _ => None,
    }
  }

  /// Whether the value is an array with one element.
  pub(crate) fn is_single_element_vec(&self) -> bool {
    if let Value::Array(ref d) = self {
      d.len() == 1
    } else {
      false
    }
  }

  /// Pop the first value in the inner array or return the original value.
  ///
  /// This uses unwrap. Use [is_single_element_vec] first.
  pub(crate) fn pop_or_take(self) -> Self {
    if let Value::Array(mut values) = self {
      values.pop().unwrap()
    } else {
      self
    }
  }

  /// Flatten adjacent nested arrays to the provided depth.
  ///
  /// See the [XREAD](crate::interfaces::StreamsInterface::xread) documentation for an example of when this might be
  /// useful.
  pub fn flatten_array_values(self, depth: usize) -> Self {
    utils::flatten_nested_array_values(self, depth)
  }

  /// A utility function to convert the response from `XREAD` or `XREADGROUP` into a type with a less verbose type
  /// declaration.
  ///
  /// This function supports responses in both RESP2 and RESP3 formats.
  ///
  /// See the [XREAD](crate::interfaces::StreamsInterface::xread) (or `XREADGROUP`) documentation for more
  /// information.
  #[cfg(feature = "i-streams")]
  #[cfg_attr(docsrs, doc(cfg(feature = "i-streams")))]
  pub fn into_xread_response<K1, I, K2, V>(self) -> Result<XReadResponse<K1, I, K2, V>, Error>
  where
    K1: FromKey + Hash + Eq,
    K2: FromKey + Hash + Eq,
    I: FromValue,
    V: FromValue,
  {
    self.flatten_array_values(2).convert()
  }

  /// A utility function to convert the response from `XCLAIM`, etc into a type with a less verbose type declaration.
  ///
  /// This function supports responses in both RESP2 and RESP3 formats.
  #[cfg(feature = "i-streams")]
  #[cfg_attr(docsrs, doc(cfg(feature = "i-streams")))]
  pub fn into_xread_value<I, K, V>(self) -> Result<Vec<XReadValue<I, K, V>>, Error>
  where
    K: FromKey + Hash + Eq,
    I: FromValue,
    V: FromValue,
  {
    self.flatten_array_values(1).convert()
  }

  /// A utility function to convert the response from `XAUTOCLAIM` into a type with a less verbose type declaration.
  ///
  /// This function supports responses in both RESP2 and RESP3 formats.
  ///
  /// Note: the new (as of Redis 7.x) return value containing message PIDs that were deleted from the PEL are dropped.
  /// Callers should use `xautoclaim` instead if this data is needed.
  #[cfg(feature = "i-streams")]
  #[cfg_attr(docsrs, doc(cfg(feature = "i-streams")))]
  pub fn into_xautoclaim_values<I, K, V>(self) -> Result<(String, Vec<XReadValue<I, K, V>>), Error>
  where
    K: FromKey + Hash + Eq,
    I: FromValue,
    V: FromValue,
  {
    if let Value::Array(mut values) = self {
      if values.len() == 3 {
        // convert the redis 7.x response format to the v6 format
        trace!("Removing the third message PID elements from XAUTOCLAIM response.");
        values.pop();
      }

      // unwrap checked above
      let entries = values.pop().unwrap();
      let cursor: String = values.pop().unwrap().convert()?;

      Ok((cursor, entries.flatten_array_values(1).convert()?))
    } else {
      Err(Error::new_parse("Expected array response."))
    }
  }

  /// Parse the value as the response from `FUNCTION LIST`, including only functions with the provided library `name`.
  #[cfg(feature = "i-scripts")]
  #[cfg_attr(docsrs, doc(cfg(feature = "i-scripts")))]
  pub fn as_functions(&self, name: &str) -> Result<Vec<Function>, Error> {
    utils::value_to_functions(self, name)
  }

  /// Convert the value into a `GeoPosition`, if possible.
  ///
  /// Null values are returned as `None` to work more easily with the result of the `GEOPOS` command.
  #[cfg(feature = "i-geo")]
  #[cfg_attr(docsrs, doc(cfg(feature = "i-geo")))]
  pub fn as_geo_position(&self) -> Result<Option<GeoPosition>, Error> {
    if self.is_null() {
      Ok(None)
    } else {
      GeoPosition::try_from(self.clone()).map(Some)
    }
  }

  /// Parse the value as the response to any of the relevant GEO commands that return an array of
  /// [GeoRadiusInfo](crate::types::geo::GeoRadiusInfo) values, such as `GEOSEARCH`, GEORADIUS`, etc.
  #[cfg(feature = "i-geo")]
  #[cfg_attr(docsrs, doc(cfg(feature = "i-geo")))]
  pub fn into_geo_radius_result(
    self,
    withcoord: bool,
    withdist: bool,
    withhash: bool,
  ) -> Result<Vec<GeoRadiusInfo>, Error> {
    match self {
      Value::Array(data) => data
        .into_iter()
        .map(|value| GeoRadiusInfo::from_value(value, withcoord, withdist, withhash))
        .collect(),
      Value::Null => Ok(Vec::new()),
      _ => Err(Error::new(ErrorKind::Parse, "Expected array.")),
    }
  }

  /// Replace this value with `Value::Null`, returning the original value.
  pub fn take(&mut self) -> Value {
    mem::replace(self, Value::Null)
  }

  /// Attempt to convert this value to any value that implements the [FromValue](crate::types::FromValue) trait.
  pub fn convert<R>(self) -> Result<R, Error>
  where
    R: FromValue,
  {
    R::from_value(self)
  }

  /// Whether the value can be hashed.
  ///
  /// Some use cases require using `Value` types as keys in a `HashMap`, etc. Trying to do so with an aggregate
  /// type can panic, and this function can be used to more gracefully handle this situation.
  pub fn can_hash(&self) -> bool {
    matches!(
      self.kind(),
      ValueKind::String
        | ValueKind::Boolean
        | ValueKind::Double
        | ValueKind::Integer
        | ValueKind::Bytes
        | ValueKind::Null
        | ValueKind::Array
        | ValueKind::Queued
    )
  }

  /// Convert the value to JSON.
  #[cfg(feature = "serde-json")]
  #[cfg_attr(docsrs, doc(cfg(feature = "serde-json")))]
  pub fn into_json(self) -> Result<serde_json::Value, Error> {
    serde_json::Value::from_value(self)
  }
}

impl Hash for Value {
  fn hash<H: Hasher>(&self, state: &mut H) {
    // used to prevent collisions between different types
    let prefix = match self.kind() {
      ValueKind::Boolean => b'B',
      ValueKind::Double => b'd',
      ValueKind::Integer => b'i',
      ValueKind::String => b's',
      ValueKind::Null => b'n',
      ValueKind::Queued => b'q',
      ValueKind::Array => b'a',
      ValueKind::Map => b'm',
      ValueKind::Bytes => b'b',
    };
    prefix.hash(state);

    match *self {
      Value::Boolean(b) => b.hash(state),
      Value::Double(f) => f.to_be_bytes().hash(state),
      Value::Integer(d) => d.hash(state),
      Value::String(ref s) => s.hash(state),
      Value::Bytes(ref b) => b.hash(state),
      Value::Null => NULL.hash(state),
      Value::Queued => QUEUED.hash(state),
      Value::Array(ref arr) => {
        for value in arr.iter() {
          value.hash(state);
        }
      },
      _ => panic!("Cannot hash aggregate value."),
    }
  }
}

impl From<u16> for Value {
  fn from(d: u16) -> Self {
    Value::Integer(d as i64)
  }
}

impl From<u32> for Value {
  fn from(d: u32) -> Self {
    Value::Integer(d as i64)
  }
}

impl From<i8> for Value {
  fn from(d: i8) -> Self {
    Value::Integer(d as i64)
  }
}

impl From<i16> for Value {
  fn from(d: i16) -> Self {
    Value::Integer(d as i64)
  }
}

impl From<i32> for Value {
  fn from(d: i32) -> Self {
    Value::Integer(d as i64)
  }
}

impl From<i64> for Value {
  fn from(d: i64) -> Self {
    Value::Integer(d)
  }
}

impl From<f32> for Value {
  fn from(f: f32) -> Self {
    Value::Double(f as f64)
  }
}

impl From<f64> for Value {
  fn from(f: f64) -> Self {
    Value::Double(f)
  }
}

impl TryFrom<u64> for Value {
  type Error = Error;

  fn try_from(d: u64) -> Result<Self, Self::Error> {
    if d >= (i64::MAX as u64) {
      return Err(Error::new(ErrorKind::Unknown, "Unsigned integer too large."));
    }

    Ok((d as i64).into())
  }
}

impl TryFrom<u128> for Value {
  type Error = Error;

  fn try_from(d: u128) -> Result<Self, Self::Error> {
    if d >= (i64::MAX as u128) {
      return Err(Error::new(ErrorKind::Unknown, "Unsigned integer too large."));
    }

    Ok((d as i64).into())
  }
}

impl TryFrom<i128> for Value {
  type Error = Error;

  fn try_from(d: i128) -> Result<Self, Self::Error> {
    if d >= (i64::MAX as i128) {
      return Err(Error::new(ErrorKind::Unknown, "Signed integer too large."));
    }

    Ok((d as i64).into())
  }
}

impl TryFrom<usize> for Value {
  type Error = Error;

  fn try_from(d: usize) -> Result<Self, Self::Error> {
    if d >= (i64::MAX as usize) {
      return Err(Error::new(ErrorKind::Unknown, "Unsigned integer too large."));
    }

    Ok((d as i64).into())
  }
}

impl From<Str> for Value {
  fn from(s: Str) -> Self {
    Value::String(s)
  }
}

impl From<Bytes> for Value {
  fn from(b: Bytes) -> Self {
    Value::Bytes(b)
  }
}

impl From<Box<[u8]>> for Value {
  fn from(b: Box<[u8]>) -> Self {
    Value::Bytes(b.into())
  }
}

impl From<String> for Value {
  fn from(d: String) -> Self {
    Value::String(Str::from(d))
  }
}

impl<'a> From<&'a String> for Value {
  fn from(d: &'a String) -> Self {
    Value::String(Str::from(d))
  }
}

impl<'a> From<&'a str> for Value {
  fn from(d: &'a str) -> Self {
    Value::String(Str::from(d))
  }
}

impl<'a> From<&'a [u8]> for Value {
  fn from(b: &'a [u8]) -> Self {
    Value::Bytes(Bytes::from(b.to_vec()))
  }
}

impl From<bool> for Value {
  fn from(d: bool) -> Self {
    Value::Boolean(d)
  }
}

impl<T> TryFrom<Option<T>> for Value
where
  T: TryInto<Value>,
  T::Error: Into<Error>,
{
  type Error = Error;

  fn try_from(d: Option<T>) -> Result<Self, Self::Error> {
    match d {
      Some(i) => to!(i),
      None => Ok(Value::Null),
    }
  }
}

impl<'a, T, const N: usize> TryFrom<&'a [T; N]> for Value
where
  T: TryInto<Value> + Clone,
  T::Error: Into<Error>,
{
  type Error = Error;

  fn try_from(value: &'a [T; N]) -> Result<Self, Self::Error> {
    let values = value
      .iter()
      .map(|v| v.clone().try_into().map_err(|e| e.into()))
      .collect::<Result<Vec<Value>, Error>>()?;

    Ok(Value::Array(values))
  }
}

impl<T, const N: usize> TryFrom<[T; N]> for Value
where
  T: TryInto<Value> + Clone,
  T::Error: Into<Error>,
{
  type Error = Error;

  fn try_from(value: [T; N]) -> Result<Self, Self::Error> {
    let values = value
      .into_iter()
      .map(|v| v.try_into().map_err(|e| e.into()))
      .collect::<Result<Vec<Value>, Error>>()?;

    Ok(Value::Array(values))
  }
}

impl TryFrom<Vec<u8>> for Value {
  type Error = Error;

  fn try_from(value: Vec<u8>) -> Result<Self, Self::Error> {
    Ok(Value::Bytes(value.into()))
  }
}

impl<T> TryFrom<Vec<T>> for Value
where
  T: TryInto<Value>,
  T::Error: Into<Error>,
{
  type Error = Error;

  fn try_from(value: Vec<T>) -> Result<Self, Self::Error> {
    let values = value
      .into_iter()
      .map(|v| v.try_into().map_err(|e| e.into()))
      .collect::<Result<Vec<Value>, Error>>()?;

    Ok(Value::Array(values))
  }
}

impl<T> TryFrom<VecDeque<T>> for Value
where
  T: TryInto<Value>,
  T::Error: Into<Error>,
{
  type Error = Error;

  fn try_from(value: VecDeque<T>) -> Result<Self, Self::Error> {
    let values = value
      .into_iter()
      .map(|v| v.try_into().map_err(|e| e.into()))
      .collect::<Result<Vec<Value>, Error>>()?;

    Ok(Value::Array(values))
  }
}

impl<V> FromIterator<V> for Value
where
  V: Into<Value>,
{
  fn from_iter<I: IntoIterator<Item = V>>(iter: I) -> Self {
    Value::Array(iter.into_iter().map(|v| v.into()).collect())
  }
}

impl<K, V> TryFrom<HashMap<K, V>> for Value
where
  K: TryInto<Key>,
  K::Error: Into<Error>,
  V: TryInto<Value>,
  V::Error: Into<Error>,
{
  type Error = Error;

  fn try_from(d: HashMap<K, V>) -> Result<Self, Self::Error> {
    Ok(Value::Map(Map {
      inner: utils::into_map(d.into_iter())?,
    }))
  }
}

impl<K, V> TryFrom<BTreeMap<K, V>> for Value
where
  K: TryInto<Key>,
  K::Error: Into<Error>,
  V: TryInto<Value>,
  V::Error: Into<Error>,
{
  type Error = Error;

  fn try_from(d: BTreeMap<K, V>) -> Result<Self, Self::Error> {
    Ok(Value::Map(Map {
      inner: utils::into_map(d.into_iter())?,
    }))
  }
}

impl From<Key> for Value {
  fn from(d: Key) -> Self {
    Value::Bytes(d.key)
  }
}

impl From<Map> for Value {
  fn from(m: Map) -> Self {
    Value::Map(m)
  }
}

impl From<()> for Value {
  fn from(_: ()) -> Self {
    Value::Null
  }
}

impl TryFrom<Resp3Frame> for Value {
  type Error = Error;

  fn try_from(value: Resp3Frame) -> Result<Self, Self::Error> {
    protocol_utils::frame_to_results(value)
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn map_from_iter() {
    let map = [("hello", "world")].into_iter().collect::<Map>();
    assert_eq!(map.inner[&Key::from("hello")], Value::from("world"));
  }

  // requires specialization of TryFrom<Vec<u8>> for Value
  #[test]
  fn bytes_from_vec_u8() {
    let input: Vec<u8> = vec![0, 1, 2];
    let output: Value = input.clone().try_into().unwrap();
    assert_eq!(output, Value::Bytes(Bytes::from(input)));
    let input: Vec<u32> = vec![0, 1, 2, 3];
    let output: Value = input.clone().try_into().unwrap();
    assert_eq!(
      output,
      Value::Array(input.into_iter().map(|v| Value::Integer(v as i64)).collect())
    );
  }
}
