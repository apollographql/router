//! Health Check plugin
//!
//! Provides liveness and readiness checks for the router.
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

/// Configuration options pertaining to the readiness health sub-component.
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[serde(default)]
pub(crate) struct ReadinessConfig {
    #[serde(deserialize_with = "humantime_serde::deserialize", default)]
    #[schemars(with = "Option<String>", default)]
    /// The sampling duration (default: 5s)
    pub(crate) duration: Duration,

    /// How many errors/interval are allowed until unready (default: 100)
    pub(crate) allowed: usize,
}

impl Default for ReadinessConfig {
    fn default() -> Self {
        Self {
            duration: Duration::from_secs(5),
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

/*
#[buildstructor::buildstructor]
impl Config {
    #[builder]
    pub(crate) fn new(
        listen: Option<ListenAddr>,
        enabled: Option<bool>,
        path: Option<String>,
        readiness: Option<ReadinessConfig>,
    ) -> Self {
        println!("readiness: {readiness:?}");
        let mut path = path.unwrap_or_else(default_health_check_path);
        if !path.starts_with('/') {
            path = format!("/{path}");
        }

        Self {
            listen: listen.unwrap_or_else(default_health_check_listen),
            enabled: enabled.unwrap_or_else(default_health_check_enabled),
            path,
            readiness: readiness.unwrap_or_else(Default::default),
        }
    }
}

#[cfg(test)]
#[buildstructor::buildstructor]
impl Config {
    #[builder]
    pub(crate) fn fake_new(
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
            listen: listen.unwrap_or_else(test_listen),
            enabled: enabled.unwrap_or_else(default_health_check_enabled),
            path,
            readiness: readiness.unwrap_or_else(Default::default),
        }
    }
}
*/

impl Default for Config {
    fn default() -> Self {
        Self {
            listen: default_health_check_listen(),
            enabled: default_health_check_enabled(),
            path: default_health_check_path(),
            readiness: Default::default(),
        }
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
        tracing::info!(config = ?init.config, "healthcheck config");
        tracing::info!(
            "Health check exposed at {}{}",
            init.config.listen,
            init.config.path
        );
        let live = Arc::new(AtomicBool::new(false));
        let ready = Arc::new(AtomicBool::new(false));
        let rejected = Arc::new(AtomicUsize::new(0));

        let allowed = init.config.readiness.allowed;
        let my_duration = init.config.readiness.duration;
        let my_rejected = rejected.clone();
        let my_ready = ready.clone();

        let ticker = tokio::spawn(async move {
            'outer: loop {
                let start = Instant::now() + my_duration;
                let mut interval = tokio::time::interval_at(start, my_duration);
                loop {
                    my_rejected.store(0, Ordering::Relaxed);
                    interval.tick().await;
                    tracing::info!("TICKED AND CHECKING");
                    tracing::info!(%allowed, rejected = %my_rejected.load(Ordering::Relaxed));
                    if my_rejected.load(Ordering::Relaxed) > allowed {
                        my_ready.store(false, Ordering::SeqCst);
                        tokio::time::sleep(my_duration).await;
                        my_ready.store(true, Ordering::SeqCst);
                        break 'outer;
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

impl Drop for HealthCheck {
    fn drop(&mut self) {
        self.ticker.abort();
    }
}

register_private_plugin!("apollo", "healthcheck", HealthCheck);

#[cfg(test)]
mod test {}
