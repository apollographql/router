//! Reads `OTEL_SEMCONV_STABILITY_OPT_IN`, OpenTelemetry's own mechanism for opting into a draft
//! semantic convention ahead of its ratification, for the router's GraphQL subscription metrics.
//!
//! The convention these metrics come from is still an open draft:
//! <https://github.com/open-telemetry/semantic-conventions/pull/3515>.

use std::sync::LazyLock;

/// Which of the router's GraphQL subscription metrics `OTEL_SEMCONV_STABILITY_OPT_IN` selects.
///
/// The variable is a comma-separated list of `{domain}` and `{domain}/dup` tokens; this router
/// only recognizes the `graphql` domain. Per the OpenTelemetry specification, `graphql/dup` takes
/// precedence over a bare `graphql` when both are present.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum GraphqlSemconvMode {
    /// Neither `graphql` nor `graphql/dup` is present. The router does not emit the conventional
    /// GraphQL subscription metrics.
    Unset,
    /// `graphql` is present without `graphql/dup`. The router emits the conventional GraphQL
    /// subscription metrics.
    Conventional,
    /// `graphql/dup` is present. The router emits the conventional GraphQL subscription metrics.
    /// `dup` names a duplication step OpenTelemetry defines for other conventions; this router
    /// has no vendor metric it duplicates against, so `graphql/dup` behaves the same as
    /// `graphql`.
    Dup,
}

impl GraphqlSemconvMode {
    fn from_env_value(raw: &str) -> Self {
        let mut saw_graphql = false;
        for token in raw.split(',').map(str::trim) {
            if token == "graphql/dup" {
                return Self::Dup;
            }
            if token == "graphql" {
                saw_graphql = true;
            }
        }
        if saw_graphql {
            Self::Conventional
        } else {
            Self::Unset
        }
    }

    /// Whether the conventional `graphql.server.subscription.*` metrics should be recorded.
    pub(crate) fn emits_conventional_graphql_metrics(self) -> bool {
        matches!(self, Self::Conventional | Self::Dup)
    }

    /// The value reported on the `apollo.router.config.env` usage-adoption gauge.
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Unset => "unset",
            Self::Conventional => "graphql",
            Self::Dup => "graphql/dup",
        }
    }
}

// `OTEL_SEMCONV_STABILITY_OPT_IN` configures router behaviour at startup, like the other
// `OTEL_*` variables the router honours (`OTEL_EXPORTER_OTLP_*`, `OTEL_BSP_*`), so it is read
// once per process rather than on every subscription event.
static MODE: LazyLock<GraphqlSemconvMode> = LazyLock::new(|| {
    GraphqlSemconvMode::from_env_value(
        &std::env::var("OTEL_SEMCONV_STABILITY_OPT_IN").unwrap_or_default(),
    )
});

/// The active `OTEL_SEMCONV_STABILITY_OPT_IN` mode for the router's draft GraphQL subscription
/// metrics.
pub(crate) fn graphql_semconv_mode() -> GraphqlSemconvMode {
    #[cfg(test)]
    if let Some(mode) = test_override::current() {
        return mode;
    }
    *MODE
}

/// Selects a mode for the duration of one test without touching the environment.
///
/// The process reads `OTEL_SEMCONV_STABILITY_OPT_IN` once into a `LazyLock`, so a test that set
/// the variable would fix the mode for every test sharing the process — passing under a
/// process-per-test runner and failing under `cargo test`. Tests therefore take this lock, which
/// both serialises them and restores the previous value on drop.
#[cfg(test)]
pub(crate) mod test_override {
    use std::sync::Mutex;
    use std::sync::MutexGuard;
    use std::sync::OnceLock;

    use super::GraphqlSemconvMode;

    static CURRENT: Mutex<Option<GraphqlSemconvMode>> = Mutex::new(None);

    fn lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    pub(crate) fn current() -> Option<GraphqlSemconvMode> {
        *CURRENT.lock().expect("semconv override lock poisoned")
    }

    /// Holds the mode until dropped. Keep the guard alive for the whole test.
    pub(crate) struct Guard(#[allow(dead_code)] MutexGuard<'static, ()>);

    impl Drop for Guard {
        fn drop(&mut self) {
            *CURRENT.lock().expect("semconv override lock poisoned") = None;
        }
    }

    pub(crate) fn set(mode: GraphqlSemconvMode) -> Guard {
        let guard = lock()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        *CURRENT.lock().expect("semconv override lock poisoned") = Some(mode);
        Guard(guard)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unset_when_graphql_is_absent() {
        assert_eq!(
            GraphqlSemconvMode::from_env_value(""),
            GraphqlSemconvMode::Unset
        );
        assert_eq!(
            GraphqlSemconvMode::from_env_value("http"),
            GraphqlSemconvMode::Unset
        );
    }

    #[test]
    fn conventional_when_bare_graphql_token_is_present() {
        assert_eq!(
            GraphqlSemconvMode::from_env_value("graphql"),
            GraphqlSemconvMode::Conventional
        );
        assert_eq!(
            GraphqlSemconvMode::from_env_value("http,graphql"),
            GraphqlSemconvMode::Conventional
        );
    }

    #[test]
    fn dup_when_dup_token_is_present() {
        assert_eq!(
            GraphqlSemconvMode::from_env_value("graphql/dup"),
            GraphqlSemconvMode::Dup
        );
        assert_eq!(
            GraphqlSemconvMode::from_env_value("graphql,graphql/dup"),
            GraphqlSemconvMode::Dup
        );
    }

    #[test]
    fn dup_wins_over_bare_graphql_when_both_present() {
        assert_eq!(
            GraphqlSemconvMode::from_env_value("graphql/dup,graphql"),
            GraphqlSemconvMode::Dup
        );
    }
}
