use crate::{
  commands::{MAXLEN, MINID},
  error::{Error, ErrorKind},
  types::{Key, LimitCount, StringOrNumber, Value},
  utils,
};
use bytes_utils::Str;
use std::{
  collections::{HashMap, VecDeque},
  convert::{TryFrom, TryInto},
};

/// Representation for the "=" or "~" operator in `XADD`, etc.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum XCapTrim {
  Exact,
  AlmostExact,
}

impl XCapTrim {
  pub(crate) fn to_str(&self) -> Str {
    utils::static_str(match *self {
      XCapTrim::Exact => "=",
      XCapTrim::AlmostExact => "~",
    })
  }
}

impl<'a> TryFrom<&'a str> for XCapTrim {
  type Error = Error;

  fn try_from(s: &'a str) -> Result<Self, Self::Error> {
    Ok(match s {
      "=" => XCapTrim::Exact,
      "~" => XCapTrim::AlmostExact,
      _ => return Err(Error::new(ErrorKind::InvalidArgument, "Invalid XADD trim value.")),
    })
  }
}

/// One or more ordered key-value pairs, typically used as an argument for `XADD`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MultipleOrderedPairs {
  values: Vec<(Key, Value)>,
}

impl MultipleOrderedPairs {
  pub fn len(&self) -> usize {
    self.values.len()
  }

  pub fn inner(self) -> Vec<(Key, Value)> {
    self.values
  }
}

impl From<()> for MultipleOrderedPairs {
  fn from(_: ()) -> Self {
    MultipleOrderedPairs { values: Vec::new() }
  }
}

impl<K, V> TryFrom<(K, V)> for MultipleOrderedPairs
where
  K: Into<Key>,
  V: TryInto<Value>,
  V::Error: Into<Error>,
{
  type Error = Error;

  fn try_from((key, value): (K, V)) -> Result<Self, Self::Error> {
    Ok(MultipleOrderedPairs {
      values: vec![(key.into(), to!(value)?)],
    })
  }
}

impl<K, V> TryFrom<Vec<(K, V)>> for MultipleOrderedPairs
where
  K: Into<Key>,
  V: TryInto<Value>,
  V::Error: Into<Error>,
{
  type Error = Error;

  fn try_from(values: Vec<(K, V)>) -> Result<Self, Self::Error> {
    Ok(MultipleOrderedPairs {
      values: values
        .into_iter()
        .map(|(key, value)| Ok((key.into(), to!(value)?)))
        .collect::<Result<Vec<(Key, Value)>, Error>>()?,
    })
  }
}

impl<K, V> TryFrom<VecDeque<(K, V)>> for MultipleOrderedPairs
where
  K: Into<Key>,
  V: TryInto<Value>,
  V::Error: Into<Error>,
{
  type Error = Error;

  fn try_from(values: VecDeque<(K, V)>) -> Result<Self, Self::Error> {
    Ok(MultipleOrderedPairs {
      values: values
        .into_iter()
        .map(|(key, value)| Ok((key.into(), to!(value)?)))
        .collect::<Result<Vec<(Key, Value)>, Error>>()?,
    })
  }
}

impl<K, V> TryFrom<HashMap<K, V>> for MultipleOrderedPairs
where
  K: Into<Key>,
  V: TryInto<Value>,
  V::Error: Into<Error>,
{
  type Error = Error;

  fn try_from(values: HashMap<K, V>) -> Result<Self, Self::Error> {
    Ok(MultipleOrderedPairs {
      values: values
        .into_iter()
        .map(|(key, value)| Ok((key.into(), to!(value)?)))
        .collect::<Result<Vec<(Key, Value)>, Error>>()?,
    })
  }
}

/// One or more IDs for elements in a stream.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MultipleIDs {
  inner: Vec<XID>,
}

impl MultipleIDs {
  pub fn len(&self) -> usize {
    self.inner.len()
  }

  pub fn inner(self) -> Vec<XID> {
    self.inner
  }
}

impl<T> From<T> for MultipleIDs
where
  T: Into<XID>,
{
  fn from(value: T) -> Self {
    MultipleIDs {
      inner: vec![value.into()],
    }
  }
}

impl<T> From<Vec<T>> for MultipleIDs
where
  T: Into<XID>,
{
  fn from(value: Vec<T>) -> Self {
    MultipleIDs {
      inner: value.into_iter().map(|value| value.into()).collect(),
    }
  }
}

impl<T> From<VecDeque<T>> for MultipleIDs
where
  T: Into<XID>,
{
  fn from(value: VecDeque<T>) -> Self {
    MultipleIDs {
      inner: value.into_iter().map(|value| value.into()).collect(),
    }
  }
}

/// The MAXLEN or MINID argument for a stream cap.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum XCapKind {
  MaxLen,
  MinID,
}

impl XCapKind {
  pub(crate) fn to_str(&self) -> Str {
    utils::static_str(match *self {
      XCapKind::MaxLen => MAXLEN,
      XCapKind::MinID => MINID,
    })
  }
}

impl<'a> TryFrom<&'a str> for XCapKind {
  type Error = Error;

  fn try_from(value: &'a str) -> Result<Self, Self::Error> {
    Ok(match value {
      "MAXLEN" => XCapKind::MaxLen,
      "MINID" => XCapKind::MinID,
      _ => return Err(Error::new(ErrorKind::InvalidArgument, "Expected MAXLEN or MINID,")),
    })
  }
}

/// Stream cap arguments for `XADD`, `XTRIM`, etc.
///
/// Equivalent to `[MAXLEN|MINID [=|~] threshold [LIMIT count]]`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct XCap {
  inner: Option<(XCapKind, XCapTrim, StringOrNumber, LimitCount)>,
}

impl XCap {
  pub(crate) fn into_parts(self) -> Option<(XCapKind, XCapTrim, StringOrNumber, LimitCount)> {
    self.inner
  }
}

impl From<Option<()>> for XCap {
  fn from(_: Option<()>) -> Self {
    XCap { inner: None }
  }
}

impl<K, T, S> TryFrom<(K, T, S, Option<i64>)> for XCap
where
  K: TryInto<XCapKind>,
  K::Error: Into<Error>,
  T: TryInto<XCapTrim>,
  T::Error: Into<Error>,
  S: Into<StringOrNumber>,
{
  type Error = Error;

  fn try_from((kind, trim, threshold, limit): (K, T, S, Option<i64>)) -> Result<Self, Self::Error> {
    let (kind, trim) = (to!(kind)?, to!(trim)?);
    Ok(XCap {
      inner: Some((kind, trim, threshold.into(), limit)),
    })
  }
}

impl<K, T, S> TryFrom<(K, T, S)> for XCap
where
  K: TryInto<XCapKind>,
  K::Error: Into<Error>,
  T: TryInto<XCapTrim>,
  T::Error: Into<Error>,
  S: Into<StringOrNumber>,
{
  type Error = Error;

  fn try_from((kind, trim, threshold): (K, T, S)) -> Result<Self, Self::Error> {
    let (kind, trim) = (to!(kind)?, to!(trim)?);
    Ok(XCap {
      inner: Some((kind, trim, threshold.into(), None)),
    })
  }
}

impl<K, S> TryFrom<(K, S)> for XCap
where
  K: TryInto<XCapKind>,
  K::Error: Into<Error>,
  S: Into<StringOrNumber>,
{
  type Error = Error;

  fn try_from((kind, threshold): (K, S)) -> Result<Self, Self::Error> {
    let kind = to!(kind)?;
    Ok(XCap {
      inner: Some((kind, XCapTrim::Exact, threshold.into(), None)),
    })
  }
}

/// Stream ID arguments for `XADD`, `XREAD`, etc.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum XID {
  /// The auto-generated key symbol "*".
  Auto,
  /// An ID specified by the user such as "12345-0".
  Manual(Str),
  /// The highest ID in a stream ("$").
  Max,
  /// For `XREADGROUP`, only return new IDs (">").
  NewInGroup,
}

impl XID {
  pub(crate) fn into_str(self) -> Str {
    match self {
      XID::Auto => utils::static_str("*"),
      XID::Max => utils::static_str("$"),
      XID::NewInGroup => utils::static_str(">"),
      XID::Manual(s) => s,
    }
  }
}

impl<'a> From<&'a str> for XID {
  fn from(value: &'a str) -> Self {
    match value {
      "*" => XID::Auto,
      "$" => XID::Max,
      ">" => XID::NewInGroup,
      _ => XID::Manual(value.into()),
    }
  }
}

impl<'a> From<&'a String> for XID {
  fn from(value: &'a String) -> Self {
    match value.as_ref() {
      "*" => XID::Auto,
      "$" => XID::Max,
      ">" => XID::NewInGroup,
      _ => XID::Manual(value.into()),
    }
  }
}

impl From<String> for XID {
  fn from(value: String) -> Self {
    match value.as_ref() {
      "*" => XID::Auto,
      "$" => XID::Max,
      ">" => XID::NewInGroup,
      _ => XID::Manual(value.into()),
    }
  }
}

impl From<Str> for XID {
  fn from(value: Str) -> Self {
    match &*value {
      "*" => XID::Auto,
      "$" => XID::Max,
      ">" => XID::NewInGroup,
      _ => XID::Manual(value),
    }
  }
}

/// A struct representing the trailing optional arguments to [XPENDING](https://redis.io/commands/xpending).
///
/// See the `From` implementations for various shorthand representations of these arguments. Callers should use `()`
/// to represent no arguments.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct XPendingArgs {
  pub idle:     Option<u64>,
  pub start:    Option<XID>,
  pub end:      Option<XID>,
  pub count:    Option<u64>,
  pub consumer: Option<Str>,
}

impl XPendingArgs {
  pub(crate) fn into_parts(self) -> Result<Option<(Option<u64>, XID, XID, u64, Option<Str>)>, Error> {
    let is_empty = self.idle.is_none()
      && self.start.is_none()
      && self.end.is_none()
      && self.count.is_none()
      && self.consumer.is_none();

    if is_empty {
      Ok(None)
    } else {
      let start = match self.start {
        Some(s) => s,
        None => {
          return Err(Error::new(
            ErrorKind::InvalidArgument,
            "The `start` argument is required in this context.",
          ))
        },
      };
      let end = match self.end {
        Some(s) => s,
        None => {
          return Err(Error::new(
            ErrorKind::InvalidArgument,
            "The `end` argument is required in this context.",
          ))
        },
      };
      let count = match self.count {
        Some(s) => s,
        None => {
          return Err(Error::new(
            ErrorKind::InvalidArgument,
            "The `count` argument is required in this context.",
          ))
        },
      };

      Ok(Some((self.idle, start, end, count, self.consumer)))
    }
  }
}

impl From<()> for XPendingArgs {
  fn from(_: ()) -> Self {
    XPendingArgs {
      idle:     None,
      start:    None,
      end:      None,
      count:    None,
      consumer: None,
    }
  }
}

impl<S, E> From<(S, E, u64)> for XPendingArgs
where
  S: Into<XID>,
  E: Into<XID>,
{
  fn from((start, end, count): (S, E, u64)) -> Self {
    XPendingArgs {
      idle:     None,
      start:    Some(start.into()),
      end:      Some(end.into()),
      count:    Some(count),
      consumer: None,
    }
  }
}

impl<S, E, C> From<(S, E, u64, C)> for XPendingArgs
where
  S: Into<XID>,
  E: Into<XID>,
  C: Into<Str>,
{
  fn from((start, end, count, consumer): (S, E, u64, C)) -> Self {
    XPendingArgs {
      idle:     None,
      start:    Some(start.into()),
      end:      Some(end.into()),
      count:    Some(count),
      consumer: Some(consumer.into()),
    }
  }
}

impl<S, E> From<(u64, S, E, u64)> for XPendingArgs
where
  S: Into<XID>,
  E: Into<XID>,
{
  fn from((idle, start, end, count): (u64, S, E, u64)) -> Self {
    XPendingArgs {
      idle:     Some(idle),
      start:    Some(start.into()),
      end:      Some(end.into()),
      count:    Some(count),
      consumer: None,
    }
  }
}

impl<S, E, C> From<(u64, S, E, u64, C)> for XPendingArgs
where
  S: Into<XID>,
  E: Into<XID>,
  C: Into<Str>,
{
  fn from((idle, start, end, count, consumer): (u64, S, E, u64, C)) -> Self {
    XPendingArgs {
      idle:     Some(idle),
      start:    Some(start.into()),
      end:      Some(end.into()),
      count:    Some(count),
      consumer: Some(consumer.into()),
    }
  }
}

/// A generic helper type describing the ID and associated map for each record in a stream.
///
/// See the [XReadResponse](crate::types::streams::XReadResponse) type for more information.
pub type XReadValue<I, K, V> = (I, HashMap<K, V>);
/// A generic helper type describing the top level response from `XREAD` or `XREADGROUP`.
///
/// See the [xread](crate::interfaces::StreamsInterface::xread) documentation for more information.
///
/// The inner type declarations refer to the following:
/// * K1 - The type of the outer key for the stream. Usually a `String` or `Key`.
/// * I - The type of the ID for a stream record ("abc-123"). This is usually a `String`.
/// * K2 - The type of key in the map associated with each stream record.
/// * V - The type of value in the map associated with each stream record.
///
/// To support heterogeneous values in the map describing each stream element it is recommended to declare the last
/// type as `Value` and [convert](crate::types::Value::convert) as needed.
pub type XReadResponse<K1, I, K2, V> = HashMap<K1, Vec<XReadValue<I, K2, V>>>;
