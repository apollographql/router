pub use crate::modules::response::{FromKey, FromValue};
use crate::{error::Error, runtime::JoinHandle};
pub use redis_protocol::resp3::types::{BytesFrame as Resp3Frame, RespVersion};

mod args;
mod builder;

/// Types used to inspect or operate on client connections.
#[cfg(feature = "i-client")]
#[cfg_attr(docsrs, doc(cfg(feature = "i-client")))]
pub mod client;
/// Types used to inspect or operate on clusters or cluster connections.
#[cfg(feature = "i-cluster")]
#[cfg_attr(docsrs, doc(cfg(feature = "i-cluster")))]
pub mod cluster;
mod common;
/// Types used to configure clients or commands.
pub mod config;
mod from_tuple;
/// Types used with the `GEO` interface.
#[cfg(feature = "i-geo")]
#[cfg_attr(docsrs, doc(cfg(feature = "i-geo")))]
pub mod geo;
/// Types used wih the lists interface.
#[cfg(feature = "i-lists")]
#[cfg_attr(docsrs, doc(cfg(feature = "i-lists")))]
pub mod lists;
mod multiple;
/// Types used with the `i-redisearch` interface.
#[cfg(feature = "i-redisearch")]
#[cfg_attr(docsrs, doc(cfg(feature = "i-redisearch")))]
pub mod redisearch;
/// Types used to scan servers.
pub mod scan;
/// Types related to Lua scripts or functions.
#[cfg(feature = "i-scripts")]
#[cfg_attr(docsrs, doc(cfg(feature = "i-scripts")))]
pub mod scripts;
/// Types used in the sorted sets interface.
#[cfg(feature = "i-sorted-sets")]
#[cfg_attr(docsrs, doc(cfg(feature = "i-sorted-sets")))]
pub mod sorted_sets;
/// Types used in the streams interface.
#[cfg(feature = "i-streams")]
#[cfg_attr(docsrs, doc(cfg(feature = "i-streams")))]
pub mod streams;
/// Types used with the `i-time-series` interface.
#[cfg(feature = "i-time-series")]
#[cfg_attr(docsrs, doc(cfg(feature = "i-time-series")))]
pub mod timeseries;

#[cfg(feature = "metrics")]
#[cfg_attr(docsrs, doc(cfg(feature = "metrics")))]
pub use crate::modules::metrics::Stats;
pub use args::*;
pub use builder::*;
pub use common::*;
pub use multiple::*;
pub use semver::Version;

#[cfg(feature = "dns")]
#[cfg_attr(docsrs, doc(cfg(feature = "dns")))]
pub use crate::protocol::types::Resolve;

/// Usage statistics used to scale a [DynamicPool](crate::clients::DynamicPool).
#[cfg(feature = "dynamic-pool")]
#[cfg_attr(docsrs, doc(cfg(feature = "dynamic-pool")))]
pub mod stats;

pub(crate) static QUEUED: &str = "QUEUED";

/// The ANY flag used on certain GEO commands.
pub type Any = bool;
/// The result from any of the `connect` functions showing the error that closed the connection, if any.
pub type ConnectHandle = JoinHandle<Result<(), Error>>;
/// A tuple of `(offset, count)` values for commands that allow paging through results.
pub type Limit = (i64, i64);
/// An argument type equivalent to "[LIMIT count]".
pub type LimitCount = Option<i64>;
