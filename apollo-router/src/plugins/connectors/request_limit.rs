//! Limits on Connectors requests

use std::collections::HashMap;
use std::fmt::Display;
use std::fmt::Formatter;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;
use std::sync::Arc;

use apollo_federation::sources::connect::ConnectId;
use parking_lot::Mutex;

/// Key to access request limits for a connector
#[derive(Eq, Hash, PartialEq)]
pub(crate) enum RequestLimitKey {
    /// A key to access the request limit for a connector referencing a source directive
    SourceName(String),

    /// A key to access the request limit for a connector without a corresponding source directive
    ConnectorLabel(String),
}

impl Display for RequestLimitKey {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            RequestLimitKey::SourceName(source_name) => {
                write!(f, "connector source {}", source_name)
            }
            RequestLimitKey::ConnectorLabel(connector_label) => {
                write!(f, "connector {}", connector_label)
            }
        }
    }
}

impl From<&ConnectId> for RequestLimitKey {
    fn from(value: &ConnectId) -> Self {
        value
            .source_name
            .as_ref()
            .map(|source_name| RequestLimitKey::SourceName(source_name.clone()))
            .unwrap_or(RequestLimitKey::ConnectorLabel(value.label.clone()))
    }
}

/// Tracks a request limit for a connector
pub(crate) struct RequestLimit {
    max_requests: usize,
    total_requests: AtomicUsize,
}

impl RequestLimit {
    pub(crate) fn new(max_requests: usize) -> Self {
        Self {
            max_requests,
            total_requests: AtomicUsize::new(0),
        }
    }

    pub(crate) fn allow(&self) -> bool {
        self.total_requests.fetch_add(1, Ordering::Relaxed) < self.max_requests
    }
}

/// Tracks the request limits for an operation
pub(crate) struct RequestLimits {
    default_max_requests: Option<usize>,
    limits: Mutex<HashMap<RequestLimitKey, Arc<RequestLimit>>>,
}

impl RequestLimits {
    pub(crate) fn new(default_max_requests: Option<usize>) -> Self {
        Self {
            default_max_requests,
            limits: Mutex::new(HashMap::new()),
        }
    }

    #[allow(clippy::unwrap_used)] // Unwrap checked by invariant
    pub(crate) fn get(
        &self,
        key: RequestLimitKey,
        limit: Option<usize>,
    ) -> Option<Arc<RequestLimit>> {
        if limit.is_none() && self.default_max_requests.is_none() {
            return None;
        }
        Some(
            self.limits
                .lock()
                .entry(key)
                .or_insert_with(|| {
                    Arc::new(RequestLimit::new(
                        limit.or(self.default_max_requests).unwrap(),
                    ))
                }) // unwrap ok, invariant checked above
                .clone(),
        )
    }

    pub(crate) fn log(&self) {
        self.limits.lock().iter().for_each(|(key, limit)| {
            let total = limit.total_requests.load(Ordering::Relaxed);
            if total > limit.max_requests {
                tracing::warn!(
                    "Request limit exceeded for {}: max: {}, attempted: {}",
                    key,
                    limit.max_requests,
                    total,
                );
            }
        });
    }
}
