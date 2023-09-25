use std::time::Duration;
use std::time::Instant;

use tracing_core::field::Value;

pub(crate) trait TracingUtils {
    fn or_empty(&self) -> &dyn Value;
}

impl TracingUtils for bool {
    fn or_empty(&self) -> &dyn Value {
        if *self {
            self as &dyn Value
        } else {
            &::tracing::field::Empty
        }
    }
}

/// Timer implementing Drop to automatically compute the duration between the moment it has been created until it's dropped
///```ignore
/// Timer::new(|duration| {
///     tracing::info!(histogram.apollo_router_test = duration.as_secs_f64());
/// })
/// ```
pub(crate) struct Timer<F>
where
    F: FnOnce(Duration),
{
    start: Instant,
    f: Option<F>,
}

impl<F> Timer<F>
where
    F: FnOnce(Duration),
{
    pub(crate) fn new(f: F) -> Self {
        Self {
            start: Instant::now(),
            f: f.into(),
        }
    }
}

impl<F> Drop for Timer<F>
where
    F: FnOnce(Duration),
{
    fn drop(&mut self) {
        self.f.take().expect("f must exist")(self.start.elapsed())
    }
}
