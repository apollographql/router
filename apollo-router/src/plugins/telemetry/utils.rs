use std::time::Duration;
use std::time::Instant;

use opentelemetry::KeyValue;

/// Timer implementing Drop to automatically compute the duration between the moment it has been created until it's dropped
///```ignore
/// Timer::new(|duration| {
///     f64_histogram!(
///         "apollo.router.test",
///         "Time spent testing the timer",
///         duration.as_secs_f64()
///     );
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

/// Replace existing attribute with same key, or add new one.
/// This is needed because OTel 0.31+ uses Vec<KeyValue> instead of HashMap for attributes.
pub(crate) fn upsert_attribute(attributes: &mut Vec<KeyValue>, kv: KeyValue) {
    if let Some(existing) = attributes.iter_mut().find(|a| a.key == kv.key) {
        *existing = kv;
    } else {
        attributes.push(kv);
    }
}

/// Extends attributes with new values, updating existing keys instead of duplicating.
pub(crate) fn extend_attributes(
    attrs: &mut Vec<KeyValue>,
    new_attrs: impl IntoIterator<Item = KeyValue>,
) {
    for kv in new_attrs {
        upsert_attribute(attrs, kv);
    }
}
