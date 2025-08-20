//! Facilities for using our old version of opentelemetry with our new version of http/hyper.

/// A header extractor that works on http 1.x types.
///
/// The implementation is a straight copy from [opentelemetry_http::HeaderExtractor].
/// This can be removed after we update otel.
pub struct HeaderExtractor<'a>(pub &'a http::HeaderMap);
impl opentelemetry::propagation::Extractor for HeaderExtractor<'_> {
    /// Get a value for a key from the HeaderMap.  If the value is not valid ASCII, returns None.
    fn get(&self, key: &str) -> Option<&str> {
        self.0.get(key).and_then(|value| value.to_str().ok())
    }

    /// Collect all the keys from the HeaderMap.
    fn keys(&self) -> Vec<&str> {
        self.0
            .keys()
            .map(|value| value.as_str())
            .collect::<Vec<_>>()
    }
}

/// A header injector that works on http 1.x types.
///
/// The implementation is a straight copy from [opentelemetry_http::HeaderInjector].
/// This can be removed after we update otel.
pub struct HeaderInjector<'a>(pub &'a mut http::HeaderMap);

impl opentelemetry::propagation::Injector for HeaderInjector<'_> {
    /// Set a key and value in the HeaderMap.  Does nothing if the key or value are not valid inputs.
    fn set(&mut self, key: &str, value: String) {
        if let Ok(name) = http::header::HeaderName::from_bytes(key.as_bytes())
            && let Ok(val) = http::header::HeaderValue::from_str(&value)
        {
            self.0.insert(name, val);
        }
    }
}
