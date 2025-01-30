//! Health Check plugin
//!
//! Provides liveness and readiness checks for the router.
//!
//! This module needs to be executed prior to traffic shaping so that it can capture the responses
//! of requests which have been load shed.
//!

use std::net::SocketAddr;
use std::str::FromStr;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use http::StatusCode;
use multimap::MultiMap;
use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;
use tokio::time::Instant;
use tower::service_fn;
use tower::BoxError;
use tower::ServiceBuilder;
use tower::ServiceExt;

use crate::configuration::ListenAddr;
use crate::plugin::PluginInit;
use crate::plugin::PluginPrivate;
use crate::register_private_plugin;
use crate::services::router;
use crate::Endpoint;

#[derive(Debug, Serialize)]
#[serde(rename_all = "UPPERCASE")]
#[allow(dead_code)]
enum HealthStatus {
    Up,
    Down,
}

#[derive(Debug, Serialize)]
struct Health {
    status: HealthStatus,
}

/// Configuration options pertaining to the readiness health interval sub-component.
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[serde(default)]
pub(crate) struct ReadinessIntervalConfig {
    #[serde(deserialize_with = "humantime_serde::deserialize", default)]
    #[serde(serialize_with = "humantime_serde::serialize")]
    #[schemars(with = "Option<String>", default)]
    /// The sampling interval (default: 5s)
    pub(crate) sampling: Duration,

    #[serde(deserialize_with = "humantime_serde::deserialize")]
    #[serde(serialize_with = "humantime_serde::serialize")]
    #[schemars(with = "Option<String>")]
    /// The unready interval (default: sampling interval)
    pub(crate) unready: Option<Duration>,
}

/// Configuration options pertaining to the readiness health sub-component.
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[serde(default)]
pub(crate) struct ReadinessConfig {
    /// The readiness interval configuration
    pub(crate) interval: ReadinessIntervalConfig,

    /// How many errors/interval are allowed until unready (default: 100)
    pub(crate) allowed: usize,
}

impl Default for ReadinessIntervalConfig {
    fn default() -> Self {
        Self {
            sampling: Duration::from_secs(5),
            unready: None,
        }
    }
}

impl Default for ReadinessConfig {
    fn default() -> Self {
        Self {
            interval: Default::default(),
            allowed: 100,
        }
    }
}

/// Configuration options pertaining to the health component.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[serde(default)]
pub(crate) struct Config {
    /// The socket address and port to listen on
    /// Defaults to 127.0.0.1:8088
    pub(crate) listen: ListenAddr,

    /// Set to false to disable the health check
    pub(crate) enabled: bool,

    /// Optionally set a custom healthcheck path
    /// Defaults to /health
    pub(crate) path: String,

    /// Optionally specify readiness configuration
    pub(crate) readiness: ReadinessConfig,
}

#[cfg(test)]
pub(crate) fn test_listen() -> ListenAddr {
    SocketAddr::from_str("127.0.0.1:0").unwrap().into()
}

fn default_health_check_listen() -> ListenAddr {
    SocketAddr::from_str("127.0.0.1:8088").unwrap().into()
}

fn default_health_check_enabled() -> bool {
    true
}

fn default_health_check_path() -> String {
    "/health".to_string()
}

#[buildstructor::buildstructor]
impl Config {
    #[builder]
    pub(crate) fn new(
        listen: Option<ListenAddr>,
        enabled: Option<bool>,
        path: Option<String>,
        readiness: Option<ReadinessConfig>,
    ) -> Self {
        let mut path = path.unwrap_or_else(default_health_check_path);
        if !path.starts_with('/') {
            path = format!("/{path}");
        }

        Self {
            listen: listen.unwrap_or_else(default_health_check_listen),
            enabled: enabled.unwrap_or_else(default_health_check_enabled),
            path,
            readiness: readiness.unwrap_or_default(),
        }
    }
}

impl Default for Config {
    fn default() -> Self {
        Self::builder().build()
    }
}

struct HealthCheck {
    config: Config,
    live: Arc<AtomicBool>,
    ready: Arc<AtomicBool>,
    rejected: Arc<AtomicUsize>,
    ticker: tokio::task::JoinHandle<()>,
}

#[async_trait::async_trait]
impl PluginPrivate for HealthCheck {
    type Config = Config;

    async fn new(init: PluginInit<Self::Config>) -> Result<Self, BoxError> {
        tracing::info!(
            "Health check exposed at {}{}",
            init.config.listen,
            init.config.path
        );
        let live = Arc::new(AtomicBool::new(false));
        let ready = Arc::new(AtomicBool::new(false));
        let rejected = Arc::new(AtomicUsize::new(0));

        let allowed = init.config.readiness.allowed;
        let my_sampling_interval = init.config.readiness.interval.sampling;
        let my_recovery_interval = init
            .config
            .readiness
            .interval
            .unready
            .unwrap_or(my_sampling_interval);
        let my_rejected = rejected.clone();
        let my_ready = ready.clone();

        let ticker = tokio::spawn(async move {
            loop {
                let start = Instant::now() + my_sampling_interval;
                let mut interval = tokio::time::interval_at(start, my_sampling_interval);
                loop {
                    my_rejected.store(0, Ordering::Relaxed);
                    interval.tick().await;
                    tracing::info!(rejected = %my_rejected.load(Ordering::Relaxed), allowed, "check for unready");
                    if my_rejected.load(Ordering::Relaxed) > allowed {
                        my_ready.store(false, Ordering::SeqCst);
                        tokio::time::sleep(my_recovery_interval).await;
                        my_ready.store(true, Ordering::SeqCst);
                        break;
                    }
                }
            }
        });
        Ok(Self {
            config: init.config,
            live,
            ready,
            rejected,
            ticker,
        })
    }

    // Track rejected requests due to traffic shaping.
    fn router_service(&self, service: router::BoxService) -> router::BoxService {
        let my_rejected = self.rejected.clone();

        ServiceBuilder::new()
            .map_response(move |res: router::Response| {
                if res.response.status() == StatusCode::SERVICE_UNAVAILABLE
                    || res.response.status() == StatusCode::GATEWAY_TIMEOUT
                {
                    my_rejected.fetch_add(1, Ordering::Relaxed);
                }
                res
            })
            .service(service)
            .boxed()
    }

    // Support the health-check endpoint for the router, incorporating both live and ready.
    fn web_endpoints(&self) -> MultiMap<ListenAddr, Endpoint> {
        let mut map = MultiMap::new();

        let my_ready = self.ready.clone();
        let my_live = self.live.clone();

        let endpoint = Endpoint::from_router_service(
            self.config.path.clone(),
            service_fn(move |req: router::Request| {
                let mut status_code = StatusCode::OK;
                let health = if let Some(query) = req.router_request.uri().query() {
                    let query_upper = query.to_ascii_uppercase();
                    // Could be more precise, but sloppy match is fine for this use case
                    if query_upper.starts_with("READY") {
                        let status = if my_ready.load(Ordering::SeqCst) {
                            HealthStatus::Up
                        } else {
                            // It's hard to get k8s to parse payloads. Especially since we
                            // can't install curl or jq into our docker images because of CVEs.
                            // So, compromise, k8s will interpret this as probe fail.
                            status_code = StatusCode::SERVICE_UNAVAILABLE;
                            HealthStatus::Down
                        };
                        Health { status }
                    } else if query_upper.starts_with("LIVE") {
                        let status = if my_live.load(Ordering::SeqCst) {
                            HealthStatus::Up
                        } else {
                            // It's hard to get k8s to parse payloads. Especially since we
                            // can't install curl or jq into our docker images because of CVEs.
                            // So, compromise, k8s will interpret this as probe fail.
                            status_code = StatusCode::SERVICE_UNAVAILABLE;
                            HealthStatus::Down
                        };
                        Health { status }
                    } else {
                        Health {
                            status: HealthStatus::Up,
                        }
                    }
                } else {
                    Health {
                        status: HealthStatus::Up,
                    }
                };
                tracing::trace!(?health, request = ?req.router_request, "health check");
                async move {
                    Ok(router::Response {
                        response: http::Response::builder().status(status_code).body(
                            router::body::from_bytes(
                                serde_json::to_vec(&health).map_err(BoxError::from)?,
                            ),
                        )?,
                        context: req.context,
                    })
                }
            })
            .boxed(),
        );

        map.insert(self.config.listen.clone(), endpoint);

        map
    }

    /// The point of no return this plugin is about to go live
    fn activate(&self) {
        self.live.store(true, Ordering::SeqCst);
        self.ready.store(true, Ordering::SeqCst);
    }
}

// When a new configuration is made available we need to drop our old ticker.
impl Drop for HealthCheck {
    fn drop(&mut self) {
        self.ticker.abort();
    }
}

register_private_plugin!("apollo", "health_check", HealthCheck);

#[cfg(test)]
mod test {
    use serde_json::json;
    use tower::Service;
    use tower::ServiceExt;

    use super::*;
    use crate::plugins::test::PluginTestHarness;

    async fn get_axum_router(listen_addr: ListenAddr, config: &'static str) -> axum::Router {
        let test_harness: PluginTestHarness<HealthCheck> =
            PluginTestHarness::builder().config(config).build().await;

        let endpoints = test_harness.web_endpoints();

        let endpoint = endpoints.get(&listen_addr).expect("it better be there");

        endpoint.clone().into_router()
    }

    async fn base_test_health_check(router_addr: &str, config: &'static str) {
        let listen_addr: ListenAddr = SocketAddr::from_str(router_addr).unwrap().into();

        let mut axum_router = get_axum_router(listen_addr, config).await;

        let request = http::Request::builder()
            .uri(format!("http://{}/health", router_addr))
            .body(http_body_util::Empty::new())
            .expect("valid request");

        let mut svc = axum_router.as_service();
        let response = svc
            .ready()
            .await
            .expect("readied")
            .call(request)
            .await
            .expect("called it");

        assert_eq!(response.status(), StatusCode::OK);
        let j: serde_json::Value = serde_json::from_slice(
            &crate::services::router::body::into_bytes(response)
                .await
                .expect("we have a body"),
        )
        .expect("some json");
        assert_eq!(json!({"status": "UP" }), j)
    }

    #[tokio::test]
    async fn test_health_check() {
        let router_addr = "127.0.0.1:8088";
        base_test_health_check(
            router_addr,
            include_str!("testdata/default_listener.router.yaml"),
        )
        .await;
    }

    #[tokio::test]
    async fn test_health_check_custom_listener() {
        let router_addr = "127.0.0.1:4012";
        base_test_health_check(
            router_addr,
            include_str!("testdata/custom_listener.router.yaml"),
        )
        .await;
    }
}
