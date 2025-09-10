use crate::{
  error::{Error, ErrorKind},
  types::Value,
  utils,
};
use bytes_utils::Str;
use std::{
  collections::VecDeque,
  convert::{TryFrom, TryInto},
  iter::FromIterator,
};

/// `MIN|MAX` arguments for `BZMPOP`, etc.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ZCmp {
  Min,
  Max,
}

impl ZCmp {
  pub(crate) fn to_str(&self) -> &'static str {
    match self {
      ZCmp::Min => "MIN",
      ZCmp::Max => "MAX",
    }
  }
}

/// Convenience struct for `ZINTERSTORE` and `ZUNIONSTORE` when accepting 1 or more `weights` arguments.
pub struct MultipleWeights {
  values: Vec<f64>,
}

impl MultipleWeights {
  pub fn new() -> MultipleWeights {
    MultipleWeights { values: Vec::new() }
  }

  pub fn inner(self) -> Vec<f64> {
    self.values
  }

  pub fn len(&self) -> usize {
    self.values.len()
  }
}

impl From<Option<f64>> for MultipleWeights {
  fn from(d: Option<f64>) -> Self {
    match d {
      Some(w) => w.into(),
      None => MultipleWeights::new(),
    }
  }
}

impl From<f64> for MultipleWeights {
  fn from(d: f64) -> Self {
    MultipleWeights { values: vec![d] }
  }
}

impl FromIterator<f64> for MultipleWeights {
  fn from_iter<I: IntoIterator<Item = f64>>(iter: I) -> Self {
    MultipleWeights {
      values: iter.into_iter().collect(),
    }
  }
}

impl From<Vec<f64>> for MultipleWeights {
  fn from(d: Vec<f64>) -> Self {
    MultipleWeights { values: d }
  }
}

impl From<VecDeque<f64>> for MultipleWeights {
  fn from(d: VecDeque<f64>) -> Self {
    MultipleWeights {
      values: d.into_iter().collect(),
    }
  }
}

/// Convenience struct for the `ZADD` command to accept 1 or more `(score, value)` arguments.
pub struct MultipleZaddValues {
  values: Vec<(f64, Value)>,
}

impl MultipleZaddValues {
  pub fn new() -> MultipleZaddValues {
    MultipleZaddValues { values: Vec::new() }
  }

  pub fn inner(self) -> Vec<(f64, Value)> {
    self.values
  }

  pub fn len(&self) -> usize {
    self.values.len()
  }
}

impl<T> TryFrom<(f64, T)> for MultipleZaddValues
where
  T: TryInto<Value>,
  T::Error: Into<Error>,
{
  type Error = Error;

  fn try_from((f, d): (f64, T)) -> Result<Self, Self::Error> {
    Ok(MultipleZaddValues {
      values: vec![(f, to!(d)?)],
    })
  }
}

impl<T> FromIterator<(f64, T)> for MultipleZaddValues
where
  T: Into<Value>,
{
  fn from_iter<I: IntoIterator<Item = (f64, T)>>(iter: I) -> Self {
    MultipleZaddValues {
      values: iter.into_iter().map(|(f, d)| (f, d.into())).collect(),
    }
  }
}

impl<T> TryFrom<Vec<(f64, T)>> for MultipleZaddValues
where
  T: TryInto<Value>,
  T::Error: Into<Error>,
{
  type Error = Error;

  fn try_from(d: Vec<(f64, T)>) -> Result<Self, Self::Error> {
    let mut values = Vec::with_capacity(d.len());
    for (f, v) in d.into_iter() {
      values.push((f, to!(v)?));
    }

    Ok(MultipleZaddValues { values })
  }
}

impl<T> TryFrom<VecDeque<(f64, T)>> for MultipleZaddValues
where
  T: TryInto<Value>,
  T::Error: Into<Error>,
{
  type Error = Error;

  fn try_from(d: VecDeque<(f64, T)>) -> Result<Self, Self::Error> {
    let mut values = Vec::with_capacity(d.len());
    for (f, v) in d.into_iter() {
      values.push((f, to!(v)?));
    }

    Ok(MultipleZaddValues { values })
  }
}

/// Ordering options for the ZADD (and related) commands.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Ordering {
  GreaterThan,
  LessThan,
}

impl Ordering {
  pub(crate) fn to_str(&self) -> Str {
    utils::static_str(match *self {
      Ordering::GreaterThan => "GT",
      Ordering::LessThan => "LT",
    })
  }
}

/// Options for the ZRANGE (and related) commands.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ZSort {
  ByScore,
  ByLex,
}

impl ZSort {
  pub(crate) fn to_str(&self) -> Str {
    utils::static_str(match *self {
      ZSort::ByScore => "BYSCORE",
      ZSort::ByLex => "BYLEX",
    })
  }
}

/// An index, score, lexicographical, or +|-|+inf|-inf range bound for the ZRANGE command.
#[derive(Clone, Debug)]
pub enum ZRangeBound {
  /// Index ranges (<https://redis.io/commands/zrange#index-ranges>)
  Index(i64),
  /// Score ranges (<https://redis.io/commands/zrange#score-ranges>)
  Score(f64),
  /// Lexicographical ranges (<https://redis.io/commands/zrange#lexicographical-ranges>)
  Lex(String),
  /// Shortcut for the `+` character.
  InfiniteLex,
  /// Shortcut for the `-` character.
  NegInfinityLex,
  /// Shortcut for the `+inf` range bound.
  InfiniteScore,
  /// Shortcut for the `-inf` range bound.
  NegInfiniteScore,
}

impl From<i64> for ZRangeBound {
  fn from(i: i64) -> Self {
    ZRangeBound::Index(i)
  }
}

impl<'a> From<&'a str> for ZRangeBound {
  fn from(s: &'a str) -> Self {
    if s == "+inf" {
      ZRangeBound::InfiniteScore
    } else if s == "-inf" {
      ZRangeBound::NegInfiniteScore
    } else {
      ZRangeBound::Lex(s.to_owned())
    }
  }
}

impl From<String> for ZRangeBound {
  fn from(s: String) -> Self {
    if s == "+inf" {
      ZRangeBound::InfiniteScore
    } else if s == "-inf" {
      ZRangeBound::NegInfiniteScore
    } else {
      ZRangeBound::Lex(s)
    }
  }
}

impl<'a> From<&'a String> for ZRangeBound {
  fn from(s: &'a String) -> Self {
    s.as_str().into()
  }
}

impl TryFrom<f64> for ZRangeBound {
  type Error = Error;

  fn try_from(f: f64) -> Result<Self, Self::Error> {
    let value = if f.is_infinite() && f.is_sign_negative() {
      ZRangeBound::NegInfiniteScore
    } else if f.is_infinite() {
      ZRangeBound::InfiniteScore
    } else if f.is_nan() {
      return Err(Error::new(ErrorKind::Unknown, "Cannot use NaN as zrange field."));
    } else {
      ZRangeBound::Score(f)
    };

    Ok(value)
  }
}

/// The type of range interval bound.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ZRangeKind {
  Inclusive,
  Exclusive,
}

impl Default for ZRangeKind {
  fn default() -> Self {
    ZRangeKind::Inclusive
  }
}

/// A wrapper struct for a range bound in a sorted set command.
#[derive(Clone, Debug)]
pub struct ZRange {
  pub kind:  ZRangeKind,
  pub range: ZRangeBound,
}

impl ZRange {
  pub(crate) fn into_value(self) -> Result<Value, Error> {
    let value = if self.kind == ZRangeKind::Exclusive {
      match self.range {
        ZRangeBound::Index(i) => format!("({}", i).into(),
        ZRangeBound::Score(f) => utils::f64_to_zrange_bound(f, &self.kind)?.into(),
        ZRangeBound::Lex(s) => utils::check_lex_str(s, &self.kind).into(),
        ZRangeBound::InfiniteLex => Value::from_static_str("+"),
        ZRangeBound::NegInfinityLex => Value::from_static_str("-"),
        ZRangeBound::InfiniteScore => Value::from_static_str("+inf"),
        ZRangeBound::NegInfiniteScore => Value::from_static_str("-inf"),
      }
    } else {
      match self.range {
        ZRangeBound::Index(i) => i.into(),
        ZRangeBound::Score(f) => f.try_into()?,
        ZRangeBound::Lex(s) => utils::check_lex_str(s, &self.kind).into(),
        ZRangeBound::InfiniteLex => Value::from_static_str("+"),
        ZRangeBound::NegInfinityLex => Value::from_static_str("-"),
        ZRangeBound::InfiniteScore => Value::from_static_str("+inf"),
        ZRangeBound::NegInfiniteScore => Value::from_static_str("-inf"),
      }
    };

    Ok(value)
  }
}

impl From<i64> for ZRange {
  fn from(i: i64) -> Self {
    ZRange {
      kind:  ZRangeKind::default(),
      range: i.into(),
    }
  }
}

impl<'a> From<&'a str> for ZRange {
  fn from(s: &'a str) -> Self {
    ZRange {
      kind:  ZRangeKind::default(),
      range: s.into(),
    }
  }
}

impl From<String> for ZRange {
  fn from(s: String) -> Self {
    ZRange {
      kind:  ZRangeKind::default(),
      range: s.into(),
    }
  }
}

impl<'a> From<&'a String> for ZRange {
  fn from(s: &'a String) -> Self {
    ZRange {
      kind:  ZRangeKind::default(),
      range: s.as_str().into(),
    }
  }
}

impl TryFrom<f64> for ZRange {
  type Error = Error;

  fn try_from(f: f64) -> Result<Self, Self::Error> {
    Ok(ZRange {
      kind:  ZRangeKind::default(),
      range: f.try_into()?,
    })
  }
}

impl<'a> From<&'a ZRange> for ZRange {
  fn from(range: &'a ZRange) -> Self {
    range.clone()
  }
}

/// Aggregate options for the [zinterstore](https://redis.io/commands/zinterstore) (and related) commands.
pub enum AggregateOptions {
  Sum,
  Min,
  Max,
}

impl AggregateOptions {
  #[cfg(feature = "i-sorted-sets")]
  pub(crate) fn to_str(&self) -> Str {
    utils::static_str(match *self {
      AggregateOptions::Sum => "SUM",
      AggregateOptions::Min => "MIN",
      AggregateOptions::Max => "MAX",
    })
  }
}
