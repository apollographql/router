//! Export profiles to a Datadog agent.
//!
//! The agent proxies profiles to Datadog's intake from `/profiling/v1/input`. The request format
//! (a multipart form with an `event` JSON part describing a set of pprof attachments) follows
//! libdatadog's exporter, which is what Datadog's own profilers, including ddprof, use.

use std::sync::LazyLock;
use std::time::Duration;

use schemars::JsonSchema;
use serde::Deserialize;
use tower::BoxError;

use super::Profile;
use super::ProfileExporter;
use crate::plugins::telemetry::endpoint::UriEndpoint;
use crate::plugins::telemetry::tracing::datadog::agent_endpoint;

const ORIGIN: &str = "apollo-router";
const VERSION: &str = env!("CARGO_PKG_VERSION");
const TIMEOUT: Duration = Duration::from_secs(10);

/// Identifies this process across uploads, as Datadog's profilers do.
static RUNTIME_ID: LazyLock<String> = LazyLock::new(|| uuid::Uuid::new_v4().to_string());

/// Datadog profile exporter configuration.
///
/// Profiles are tagged with `DD_SERVICE` (default `router`), `DD_ENV`, and `DD_VERSION` (default
/// the router's version), as Datadog's own profilers are.
#[derive(Clone, Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, default)]
#[schemars(rename = "ContinuousProfilingDatadogConfig")]
pub(crate) struct Config {
    /// Enable exporting profiles to a Datadog agent
    pub(crate) enabled: bool,

    /// The Datadog agent endpoint. As with the Datadog trace exporter, `DD_TRACE_AGENT_URL`, or
    /// `DD_AGENT_HOST` and `DD_TRACE_AGENT_PORT`, take precedence. Defaults to
    /// `http://127.0.0.1:8126`.
    endpoint: UriEndpoint,
}

pub(crate) struct DatadogExporter {
    client: reqwest::Client,
    url: String,
    tags: String,
}

impl DatadogExporter {
    /// `env` looks up an environment variable.
    pub(crate) fn new(
        config: &Config,
        env: impl Fn(&str) -> Option<String>,
    ) -> Result<Self, BoxError> {
        let env = |key: &str| env(key).filter(|value| !value.is_empty());
        let agent = agent_endpoint(&config.endpoint, |key| {
            env(key).filter(|value| {
                // Unix sockets aren't supported yet. Rather than fail, fall back to
                // `DD_AGENT_HOST` or the configured endpoint.
                let unix = key == "DD_TRACE_AGENT_URL" && value.starts_with("unix:");
                if unix {
                    tracing::warn!("ignoring DD_TRACE_AGENT_URL: continuous profiling can't use a Unix socket yet; set DD_AGENT_HOST or the exporter's endpoint");
                }
                !unix
            })
        })?;

        let mut tags = vec![
            format!(
                "service:{}",
                env("DD_SERVICE").unwrap_or_else(|| "router".to_string())
            ),
            format!(
                "version:{}",
                env("DD_VERSION").unwrap_or_else(|| VERSION.to_string())
            ),
            "language:native".to_string(),
            format!("profiler_version:{VERSION}"),
            format!("runtime-id:{}", *RUNTIME_ID),
        ];
        if let Some(dd_env) = env("DD_ENV") {
            tags.push(format!("env:{dd_env}"));
        }

        Ok(Self {
            client: client(TIMEOUT)?,
            url: format!(
                "{}/profiling/v1/input",
                agent.to_string().trim_end_matches('/')
            ),
            tags: tags.join(","),
        })
    }
}

fn client(timeout: Duration) -> Result<reqwest::Client, BoxError> {
    Ok(reqwest::Client::builder().timeout(timeout).build()?)
}

#[cfg(test)]
impl DatadogExporter {
    pub(super) fn with_timeout(mut self, timeout: Duration) -> Self {
        self.client = client(timeout).expect("client");
        self
    }
}

#[async_trait::async_trait]
impl ProfileExporter for DatadogExporter {
    fn name(&self) -> &'static str {
        "datadog"
    }

    async fn export(&self, profile: &Profile) -> Result<(), BoxError> {
        let event = serde_json::json!({
            "attachments": ["profile.pprof"],
            "tags_profiler": self.tags,
            "start": humantime::format_rfc3339_nanos(profile.start).to_string(),
            "end": humantime::format_rfc3339_nanos(profile.end).to_string(),
            "family": "native",
            "version": "4",
        });
        let form = reqwest::multipart::Form::new()
            // The intake looks for the part named `event`, not the file name.
            .part(
                "event",
                reqwest::multipart::Part::text(event.to_string())
                    .file_name("event.json")
                    .mime_str("application/json")?,
            )
            .part(
                "profile.pprof",
                reqwest::multipart::Part::bytes(profile.pprof.clone()).file_name("profile.pprof"),
            );

        self.client
            .post(&self.url)
            .header("DD-EVP-ORIGIN", ORIGIN)
            .header("DD-EVP-ORIGIN-VERSION", VERSION)
            .multipart(form)
            .send()
            .await?
            .error_for_status()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    fn exporter(env: &[(&str, &str)]) -> DatadogExporter {
        let env: HashMap<_, _> = env.iter().copied().collect();
        let config: Config = serde_json::from_value(serde_json::json!({"enabled": true})).unwrap();
        DatadogExporter::new(&config, |key| env.get(key).map(|v| v.to_string())).unwrap()
    }

    #[test]
    fn tags_come_from_unified_service_tagging_env_vars() {
        let tags = exporter(&[
            ("DD_SERVICE", "my-router"),
            ("DD_ENV", "prod"),
            ("DD_VERSION", "1.2.3"),
        ])
        .tags;
        assert!(
            tags.starts_with("service:my-router,version:1.2.3,language:native,"),
            "{tags}"
        );
        assert!(tags.ends_with(",env:prod"), "{tags}");

        let tags = exporter(&[("DD_ENV", "")]).tags;
        assert!(
            tags.starts_with(&format!("service:router,version:{VERSION},")),
            "{tags}"
        );
        assert!(!tags.contains("env:"), "{tags}");
    }

    #[test]
    fn unix_socket_agent_url_falls_back_instead_of_failing() {
        let exporter = exporter(&[
            ("DD_TRACE_AGENT_URL", "unix:///var/run/datadog/apm.socket"),
            ("DD_AGENT_HOST", "agent"),
        ]);
        assert_eq!(exporter.url, "http://agent:8126/profiling/v1/input");
    }
}
