use crate::{
  error::{Error, ErrorKind},
  types::Value,
  utils,
};
use bytes_utils::Str;
use std::collections::HashMap;

/// Encoding arguments for certain timeseries commands.
#[cfg_attr(docsrs, doc(cfg(feature = "i-time-series")))]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Encoding {
  Compressed,
  Uncompressed,
}

impl Encoding {
  pub(crate) fn to_str(&self) -> Str {
    utils::static_str(match *self {
      Encoding::Compressed => "COMPRESSED",
      Encoding::Uncompressed => "UNCOMPRESSED",
    })
  }
}

/// The duplicate policy used with certain timeseries commands.
#[cfg_attr(docsrs, doc(cfg(feature = "i-time-series")))]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DuplicatePolicy {
  Block,
  First,
  Last,
  Min,
  Max,
  Sum,
}

impl DuplicatePolicy {
  pub(crate) fn to_str(&self) -> Str {
    utils::static_str(match *self {
      DuplicatePolicy::Block => "BLOCK",
      DuplicatePolicy::First => "FIRST",
      DuplicatePolicy::Last => "LAST",
      DuplicatePolicy::Min => "MIN",
      DuplicatePolicy::Max => "MAX",
      DuplicatePolicy::Sum => "SUM",
    })
  }
}

/// A timestamp used in most timeseries commands.
#[cfg_attr(docsrs, doc(cfg(feature = "i-time-series")))]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Timestamp {
  /// Unix time (milliseconds since epoch).
  Custom(i64),
  /// The server's current time, equivalent to "*".
  Now,
}

impl Default for Timestamp {
  fn default() -> Self {
    Timestamp::Now
  }
}

impl Timestamp {
  pub(crate) fn to_value(&self) -> Value {
    match *self {
      Timestamp::Now => Value::String(utils::static_str("*")),
      Timestamp::Custom(v) => Value::Integer(v),
    }
  }

  pub(crate) fn from_str(value: &str) -> Result<Self, Error> {
    match value {
      "*" => Ok(Timestamp::Now),
      _ => Ok(Timestamp::Custom(value.parse::<i64>()?)),
    }
  }
}

impl From<i64> for Timestamp {
  fn from(value: i64) -> Self {
    Timestamp::Custom(value)
  }
}

impl TryFrom<&str> for Timestamp {
  type Error = Error;

  fn try_from(value: &str) -> Result<Self, Self::Error> {
    Self::from_str(value)
  }
}

impl TryFrom<Str> for Timestamp {
  type Error = Error;

  fn try_from(value: Str) -> Result<Self, Self::Error> {
    Self::from_str(&value)
  }
}

impl TryFrom<String> for Timestamp {
  type Error = Error;

  fn try_from(value: String) -> Result<Self, Self::Error> {
    Self::from_str(&value)
  }
}

/// An aggregation policy to use with certain timeseries commands.
#[cfg_attr(docsrs, doc(cfg(feature = "i-time-series")))]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Aggregator {
  Avg,
  Sum,
  Min,
  Max,
  Range,
  Count,
  First,
  Last,
  StdP,
  StdS,
  VarP,
  VarS,
  TWA,
}

impl Aggregator {
  pub(crate) fn to_str(&self) -> Str {
    utils::static_str(match *self {
      Aggregator::Avg => "avg",
      Aggregator::Sum => "sum",
      Aggregator::Min => "min",
      Aggregator::Max => "max",
      Aggregator::Range => "range",
      Aggregator::Count => "count",
      Aggregator::First => "first",
      Aggregator::Last => "last",
      Aggregator::StdP => "std.p",
      Aggregator::StdS => "std.s",
      Aggregator::VarP => "var.p",
      Aggregator::VarS => "var.s",
      Aggregator::TWA => "twa",
    })
  }
}

/// Arguments equivalent to `WITHLABELS | SELECTED_LABELS label...` in various time series GET functions.
#[cfg_attr(docsrs, doc(cfg(feature = "i-time-series")))]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum GetLabels {
  WithLabels,
  SelectedLabels(Vec<Str>),
}

impl GetLabels {
  pub(crate) fn args_len(&self) -> usize {
    match *self {
      GetLabels::WithLabels => 1,
      GetLabels::SelectedLabels(ref s) => 1 + s.len(),
    }
  }
}

impl<S> FromIterator<S> for GetLabels
where
  S: Into<Str>,
{
  fn from_iter<I: IntoIterator<Item = S>>(iter: I) -> Self {
    GetLabels::SelectedLabels(iter.into_iter().map(|v| v.into()).collect())
  }
}

impl<S, const N: usize> From<[S; N]> for GetLabels
where
  S: Into<Str>,
{
  fn from(value: [S; N]) -> Self {
    GetLabels::SelectedLabels(value.into_iter().map(|v| v.into()).collect())
  }
}

impl<S> From<Vec<S>> for GetLabels
where
  S: Into<Str>,
{
  fn from(value: Vec<S>) -> Self {
    GetLabels::SelectedLabels(value.into_iter().map(|v| v.into()).collect())
  }
}

/// A timestamp query used in commands such as `TS.MRANGE`.
#[cfg_attr(docsrs, doc(cfg(feature = "i-time-series")))]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum GetTimestamp {
  /// Equivalent to `-`.
  Earliest,
  /// Equivalent to `+`
  Latest,
  Custom(i64),
}

impl GetTimestamp {
  pub(crate) fn to_value(&self) -> Value {
    match *self {
      GetTimestamp::Earliest => static_val!("-"),
      GetTimestamp::Latest => static_val!("+"),
      GetTimestamp::Custom(i) => i.into(),
    }
  }
}

impl TryFrom<&str> for GetTimestamp {
  type Error = Error;

  fn try_from(value: &str) -> Result<Self, Self::Error> {
    Ok(match value {
      "-" => GetTimestamp::Earliest,
      "+" => GetTimestamp::Latest,
      _ => GetTimestamp::Custom(value.parse::<i64>()?),
    })
  }
}

impl From<i64> for GetTimestamp {
  fn from(value: i64) -> Self {
    GetTimestamp::Custom(value)
  }
}

/// A struct representing `[ALIGN align] AGGREGATION aggregator bucketDuration [BUCKETTIMESTAMP bt] [EMPTY]` in
/// commands such as `TS.MRANGE`.
#[cfg_attr(docsrs, doc(cfg(feature = "i-time-series")))]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RangeAggregation {
  pub align:            Option<GetTimestamp>,
  pub aggregation:      Aggregator,
  pub bucket_duration:  u64,
  pub bucket_timestamp: Option<BucketTimestamp>,
  pub empty:            bool,
}

impl From<(Aggregator, u64)> for RangeAggregation {
  fn from((aggregation, duration): (Aggregator, u64)) -> Self {
    RangeAggregation {
      aggregation,
      bucket_duration: duration,
      align: None,
      bucket_timestamp: None,
      empty: false,
    }
  }
}

/// A `REDUCER` argument in commands such as `TS.MRANGE`.
#[cfg_attr(docsrs, doc(cfg(feature = "i-time-series")))]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Reducer {
  Avg,
  Sum,
  Min,
  Max,
  Range,
  Count,
  StdP,
  StdS,
  VarP,
  VarS,
}

impl Reducer {
  pub(crate) fn to_str(&self) -> Str {
    utils::static_str(match *self {
      Reducer::Avg => "avg",
      Reducer::Sum => "sum",
      Reducer::Min => "min",
      Reducer::Max => "max",
      Reducer::Range => "range",
      Reducer::Count => "count",
      Reducer::StdP => "std.p",
      Reducer::StdS => "std.s",
      Reducer::VarP => "var.p",
      Reducer::VarS => "var.s",
    })
  }
}

/// A struct representing `GROUPBY label REDUCE reducer` in commands such as `TS.MRANGE`.
#[cfg_attr(docsrs, doc(cfg(feature = "i-time-series")))]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GroupBy {
  pub groupby: Str,
  pub reduce:  Reducer,
}

impl<S: Into<Str>> From<(S, Reducer)> for GroupBy {
  fn from((groupby, reduce): (S, Reducer)) -> Self {
    GroupBy {
      groupby: groupby.into(),
      reduce,
    }
  }
}

/// A `BUCKETTIMESTAMP` argument in commands such as `TS.MRANGE`.
#[cfg_attr(docsrs, doc(cfg(feature = "i-time-series")))]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum BucketTimestamp {
  Start,
  End,
  Mid,
}

impl TryFrom<&str> for BucketTimestamp {
  type Error = Error;

  fn try_from(value: &str) -> Result<Self, Self::Error> {
    Ok(match value {
      "-" | "start" => BucketTimestamp::Start,
      "+" | "end" => BucketTimestamp::End,
      "~" | "mid" => BucketTimestamp::Mid,
      _ => return Err(Error::new(ErrorKind::InvalidArgument, "Invalid bucket timestamp.")),
    })
  }
}

impl BucketTimestamp {
  pub(crate) fn to_str(&self) -> Str {
    utils::static_str(match *self {
      BucketTimestamp::Start => "-",
      BucketTimestamp::End => "+",
      BucketTimestamp::Mid => "~",
    })
  }
}

/// Shorthand for the result of commands such as `MGET`, `MRANGE`, etc.
///
/// * **K** - The key type, usually a `Key`, `Str`, or `String`.
/// * **Lk** - The label key type, usually a `Str` or `String`.
/// * **Lv** - The label value type, often some kind of string type.
///
/// The fastest/cheapest option is usually `TimeseriesValues<Key, Str, Str>`.
///
/// ```rust
/// # use fred::prelude::*;
/// # use tokio::time::sleep;
/// # use std::time::Duration;
/// # use bytes_utils::Str;
/// # use fred::types::{RespVersion, timeseries::{GetLabels, Resp2TimeSeriesValues}};
/// async fn example(client: &Client) -> Result<(), Error> {
///   assert_eq!(client.protocol_version(), RespVersion::RESP2);
///
///   client
///     .ts_add("foo", "*", 1.1, None, None, None, None, ("a", "b"))
///     .await?;
///   sleep(Duration::from_millis(5)).await;
///   client
///     .ts_add("foo", "*", 2.2, None, None, None, None, ("a", "b"))
///     .await?;
///   sleep(Duration::from_millis(5)).await;
///   client
///     .ts_add("bar", "*", 3.3, None, None, None, None, ("a", "b"))
///     .await?;
///   sleep(Duration::from_millis(5)).await;
///   client
///     .ts_add("bar", "*", 4.4, None, None, None, None, ("a", "b"))
///     .await?;
///
///   let ranges: Resp2TimeSeriesValues<Key, Str, Str> = client
///     .ts_mrange(
///       "-",
///       "+",
///       true,
///       [],
///       None,
///       Some(GetLabels::WithLabels),
///       None,
///       None,
///       ["a=b"],
///       None,
///     )
///     .await?;
///
///   for (key, labels, values) in ranges.into_iter() {
///     println!("{} [{:?}] {:?}", key.as_str_lossy(), labels, values);
///   }
///   // bar [[("a", "b")]] [(1705355605510, 3.3), (1705355605517, 4.4)]
///   // foo [[("a", "b")]] [(1705355605498, 1.1), (1705355605504, 2.2)]
///   Ok(())
/// }
/// ```
///
/// See [Resp3TimeSeriesValues](crate::types::timeseries::Resp3TimeSeriesValues) for the RESP3 equivalent.
#[cfg_attr(docsrs, doc(cfg(feature = "i-time-series")))]
pub type Resp2TimeSeriesValues<K, Lk, Lv> = Vec<(K, Vec<(Lk, Lv)>, Vec<(i64, f64)>)>;

/// The RESP3 equivalent of [Resp2TimeSeriesValues](crate::types::timeseries::Resp2TimeSeriesValues).
///
/// The timeseries interface uses slightly different type signatures in RESP3 mode.
///
/// ```rust
/// # use fred::prelude::*;
/// # use tokio::time::sleep;
/// # use std::time::Duration;
/// # use bytes_utils::Str;
/// # use fred::types::{RespVersion, timeseries::{GetLabels, Resp3TimeSeriesValues}};
/// async fn example(client: &Client) -> Result<(), Error> {
///   assert_eq!(client.protocol_version(), RespVersion::RESP3);
///
///   client
///     .ts_add("foo", "*", 1.1, None, None, None, None, ("a", "b"))
///     .await?;
///   sleep(Duration::from_millis(5)).await;
///   client
///     .ts_add("foo", "*", 2.2, None, None, None, None, ("a", "b"))
///     .await?;
///   sleep(Duration::from_millis(5)).await;
///   client
///     .ts_add("bar", "*", 3.3, None, None, None, None, ("a", "b"))
///     .await?;
///   sleep(Duration::from_millis(5)).await;
///   client
///     .ts_add("bar", "*", 4.4, None, None, None, None, ("a", "b"))
///     .await?;
///
///   let ranges: Resp3TimeSeriesValues<Key, Str, Str> = client
///     .ts_mget(false, Some(GetLabels::WithLabels), ["a=b"])
///     .await?;
///
///   for (key, (labels, values)) in ranges.into_iter() {
///     println!("{} [{:?}] {:?}", key.as_str_lossy(), labels, values);
///   }
///   // bar [[("a", "b")]] [(1705355605517, 4.4)]
///   // foo [[("a", "b")]] [(1705355605504, 2.2)]
///   Ok(())
/// }
/// ```
#[cfg_attr(docsrs, doc(cfg(feature = "i-time-series")))]
pub type Resp3TimeSeriesValues<K, Lk, Lv> = HashMap<K, (Vec<(Lk, Lv)>, Vec<(i64, f64)>)>;
