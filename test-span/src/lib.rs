//! the test span library provides you with two functions:
//!
//! `get_logs()` that returns [`prelude::Records`]
//!
//! `get_span()` that returns a [`prelude::Span`],
//! Which can be serialized and used with [insta](https://crates.io/crates/insta) for snapshot tests.
//!  Refer to the tests.rs file to see how it behaves.
//!
//! Example:
//! ```ignore
//! #[test_span]
//! async fn test_it_works() {
//!   futures::join!(do_stuff(), do_stuff())
//! }
//!
//! #[tracing::instrument(name = "do_stuff", level = "info")]
//! async fn do_stuff() -> u8 {
//!     // ...
//!     do_stuff2().await;
//! }
//!
//! #[tracing::instrument(
//!     name = "do_stuff2",
//!     target = "my_crate::an_other_target",
//!     level = "info"
//! )]
//! async fn do_stuff_2(number: u8) -> u8 {
//!     // ...
//! }
//! ```
//! ```text
//! `get_span()` will provide you with:
//!
//!             ┌──────┐
//!             │ root │
//!             └──┬───┘
//!                │
//!        ┌───────┴───────┐
//!        ▼               ▼
//!   ┌──────────┐   ┌──────────┐
//!   │ do_stuff │   │ do_stuff │
//!   └────┬─────┘   └─────┬────┘
//!        │               │
//!        │               │
//!        ▼               ▼
//!  ┌───────────┐   ┌───────────┐
//!  │ do_stuff2 │   │ do_stuff2 │
//!  └───────────┘   └───────────┘
//! ```

use once_cell::sync::Lazy;
use prelude::*;
use std::sync::{Arc, Mutex};
use tracing_subscriber::util::TryInitError;
type LazyMutex<T> = Lazy<Arc<Mutex<T>>>;

mod attribute;
mod layer;
mod log;
mod record;
mod report;

static INIT: Lazy<Result<(), TryInitError>> =
    Lazy::new(|| tracing_subscriber::registry().with(Layer {}).try_init());

pub fn init() {
    Lazy::force(&INIT).as_ref().expect("couldn't set span-test subscriber as a default, maybe tracing has already been initialized somewhere else ?");
}

pub fn get_all_logs(level: &Level) -> Records {
    let logs = layer::ALL_LOGS.lock().unwrap().clone();

    Records::new(logs.all_records_for_level(level))
}

pub fn get_telemetry_for_root(
    root_id: &crate::reexports::tracing::Id,
    level: &Level,
) -> (Span, Records) {
    let report = Report::from_root(root_id.into_u64());

    (report.spans(level), report.logs(level))
}

pub fn get_spans_for_root(root_id: &crate::reexports::tracing::Id, level: &Level) -> Span {
    Report::from_root(root_id.into_u64()).spans(level)
}

pub fn get_logs_for_root(root_id: &crate::reexports::tracing::Id, level: &Level) -> Records {
    Report::from_root(root_id.into_u64()).logs(level)
}
pub mod prelude {
    pub(crate) use crate::layer::Layer;
    pub use crate::record::RecordValue;
    pub use crate::reexports::tracing::{Instrument, Level};
    pub use crate::reexports::tracing_futures::WithSubscriber;
    pub use crate::reexports::tracing_subscriber::prelude::*;
    pub use crate::report::{Records, Report, Span};
    pub use crate::{get_all_logs, get_logs_for_root, get_spans_for_root, get_telemetry_for_root};
    pub use test_span_macro::test_span;
}

pub mod reexports {
    pub use daggy;
    pub use serde;
    pub use tracing;
    pub use tracing_futures;
    pub use tracing_subscriber;
}
