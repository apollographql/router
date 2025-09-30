use std::env::consts::ARCH;
use std::env::consts::OS;
use std::sync::Arc;
use std::time::Duration;
use std::time::Instant;

use futures::StreamExt;
use http_body::Body as _;
use http_body_util::BodyExt as _;
use opentelemetry::KeyValue;
use opentelemetry::metrics::MeterProvider;
use opentelemetry::metrics::ObservableGauge;
use parking_lot::Mutex;
use schemars::JsonSchema;
use serde::Deserialize;
use sysinfo::System;
use tower::BoxError;
use tower::ServiceExt as _;
use tower::util::BoxService;
use tracing::debug;

use crate::metrics::meter_provider;
use crate::plugin::PluginInit;
use crate::plugin::PluginPrivate;
use crate::services::http::HttpRequest;
use crate::services::http::HttpResponse;
use crate::services::router;

const REFRESH_INTERVAL: Duration = Duration::from_secs(60);
const COMPUTE_DETECTOR_THRESHOLD: u16 = 24576;
const OFFICIAL_HELM_CHART_VAR: &str = "APOLLO_ROUTER_OFFICIAL_HELM_CHART";
const DEPLOYMENT_TYPE_VAR: &str = "APOLLO_ROUTER_DEPLOYMENT_TYPE";

// Valid deployment type values
const UNKNOWN: &str = "unknown";
const OFFICIAL_HELM_CHART: &str = "official_helm_chart";
const OPERATOR: &str = "operator";

/// The fleet detector plugin has no configuration.
#[derive(Debug, Default, Deserialize, JsonSchema)]
struct Conf {}

#[derive(Debug)]
struct SystemGetter {
    system: System,
    start: Instant,
}

impl SystemGetter {
    fn new() -> Self {
        let mut system = System::new();
        system.refresh_cpu_all();
        system.refresh_memory();
        Self {
            system,
            start: Instant::now(),
        }
    }

    fn get_system(&mut self) -> &System {
        if self.start.elapsed() >= REFRESH_INTERVAL {
            self.start = Instant::now();
            self.system.refresh_cpu_all();
            self.system.refresh_memory();
        }
        &self.system
    }
}

#[derive(Default)]
enum GaugeStore {
    #[default]
    Disabled,
    Pending,
    // This `Vec` is not used explicitly but is to be kept alive until the enum is dropped
    Active(#[allow(unused)] Vec<ObservableGauge<u64>>),
}

impl GaugeStore {
    fn active(opts: &GaugeOptions) -> GaugeStore {
        let system_getter = Arc::new(Mutex::new(SystemGetter::new()));
        let meter = meter_provider().meter("apollo/router");

        let mut gauges = Vec::new();
        // apollo.router.instance
        {
            let mut attributes = Vec::new();
            // CPU architecture
            attributes.push(KeyValue::new("host.arch", get_otel_arch()));
            // Operating System
            attributes.push(KeyValue::new("os.type", get_otel_os()));
            if OS == "linux" {
                attributes.push(KeyValue::new(
                    "linux.distribution",
                    System::distribution_id(),
                ));
            }
            // Compute Environment
            if let Some(env) = apollo_environment_detector::detect_one(COMPUTE_DETECTOR_THRESHOLD) {
                attributes.push(KeyValue::new("cloud.platform", env.platform_code()));
                if let Some(cloud_provider) = env.cloud_provider() {
                    attributes.push(KeyValue::new("cloud.provider", cloud_provider.code()));
                }
            }
            // Deployment type
            attributes.push(KeyValue::new(
                "deployment.type",
                opts.deployment_type.clone(),
            ));
            gauges.push(
                meter
                    .u64_observable_gauge("apollo.router.instance")
                    .with_description("The number of instances the router is running on")
                    .with_callback(move |i| {
                        i.observe(1, &attributes);
                    })
                    .init(),
            );
        }
        // apollo.router.instance.cpu_freq
        {
            let system_getter = system_getter.clone();
            gauges.push(
                meter
                    .u64_observable_gauge("apollo.router.instance.cpu_freq")
                    .with_description(
                        "The CPU frequency of the underlying instance the router is deployed to",
                    )
                    .with_unit("Mhz")
                    .with_callback(move |gauge| {
                        let local_system_getter = system_getter.clone();
                        let mut system_getter = local_system_getter.lock();
                        let system = system_getter.get_system();
                        let cpus = system.cpus();
                        let cpu_freq =
                            cpus.iter().map(|cpu| cpu.frequency()).sum::<u64>() / cpus.len() as u64;
                        gauge.observe(cpu_freq, &[])
                    })
                    .init(),
            );
        }
        // apollo.router.instance.cpu_count
        {
            let system_getter = system_getter.clone();
            gauges.push(
                meter
                    .u64_observable_gauge("apollo.router.instance.cpu_count")
                    .with_description(
                        "The number of CPUs reported by the instance the router is running on",
                    )
                    .with_callback(move |gauge| {
                        let local_system_getter = system_getter.clone();
                        let mut system_getter = local_system_getter.lock();
                        let system = system_getter.get_system();
                        let (detection_method, cpu_count) = detect_cpu_count(system);
                        gauge.observe(
                            cpu_count,
                            &[
                                KeyValue::new("host.arch", get_otel_arch()),
                                KeyValue::new("detection_method", detection_method),
                            ],
                        )
                    })
                    .init(),
            );
        }
        // apollo.router.instance.total_memory
        {
            let system_getter = system_getter.clone();
            gauges.push(
                meter
                    .u64_observable_gauge("apollo.router.instance.total_memory")
                    .with_description(
                        "The amount of memory reported by the instance the router is running on",
                    )
                    .with_callback(move |gauge| {
                        let local_system_getter = system_getter.clone();
                        let mut system_getter = local_system_getter.lock();
                        let system = system_getter.get_system();
                        gauge.observe(
                            system.total_memory(),
                            &[KeyValue::new("host.arch", get_otel_arch())],
                        )
                    })
                    .with_unit("bytes")
                    .init(),
            );
        }
        {
            let opts = opts.clone();
            gauges.push(
                meter
                    .u64_observable_gauge("apollo.router.instance.schema")
                    .with_description("Details about the current in-use schema")
                    .with_callback(move |gauge| {
                        // NOTE: this is a fixed gauge. We only care about observing the included
                        // attributes.
                        let mut attributes: Vec<KeyValue> = vec![KeyValue::new(
                            "schema_hash",
                            opts.supergraph_schema_hash.clone(),
                        )];
                        if let Some(launch_id) = opts.launch_id.as_ref() {
                            attributes.push(KeyValue::new("launch_id", launch_id.to_string()));
                        }
                        gauge.observe(1, attributes.as_slice())
                    })
                    .init(),
            )
        }
        GaugeStore::Active(gauges)
    }
}

#[derive(Clone, Default)]
struct GaugeOptions {
    supergraph_schema_hash: String,
    launch_id: Option<String>,
    deployment_type: String,
}

#[derive(Default)]
struct FleetDetector {
    gauge_store: Mutex<GaugeStore>,

    // Options passed to the gauge_store during activation.
    gauge_options: GaugeOptions,
}

#[async_trait::async_trait]
impl PluginPrivate for FleetDetector {
    type Config = Conf;

    async fn new(plugin: PluginInit<Self::Config>) -> Result<Self, BoxError> {
        debug!("initialising fleet detection plugin");

        let deployment_type = get_deployment_type(
            std::env::var_os(OFFICIAL_HELM_CHART_VAR)
                .is_some()
                .then_some("true"),
            std::env::var(DEPLOYMENT_TYPE_VAR).ok().as_deref(),
        );

        let gauge_options = GaugeOptions {
            supergraph_schema_hash: plugin.supergraph_schema_id.to_string(),
            launch_id: plugin.launch_id.map(|s| s.to_string()),
            deployment_type,
        };

        Ok(FleetDetector {
            gauge_store: Mutex::new(GaugeStore::Pending),
            gauge_options,
        })
    }

    fn activate(&self) {
        let mut store = self.gauge_store.lock();
        if matches!(*store, GaugeStore::Pending) {
            *store = GaugeStore::active(&self.gauge_options);
        }
    }

    fn router_service(&self, service: router::BoxService) -> router::BoxService {
        service
            // Count the number of request bytes from clients to the router
            .map_request(move |req: router::Request| router::Request {
                router_request: req.router_request.map(move |body| {
                    router::body::from_result_stream(body.into_data_stream().inspect(|res| {
                        if let Ok(bytes) = res {
                            u64_counter!(
                                "apollo.router.operations.request_size",
                                "Total number of request bytes from clients",
                                bytes.len() as u64
                            );
                        }
                    }))
                }),
                context: req.context,
            })
            // Count the number of response bytes from the router to clients
            .map_response(move |res: router::Response| {
                router::Response::http_response_builder()
                    .response(res.response.map(move |body| {
                        router::body::from_result_stream(body.into_data_stream().inspect(|res| {
                            if let Ok(bytes) = res {
                                u64_counter!(
                                    "apollo.router.operations.response_size",
                                    "Total number of response bytes to clients",
                                    bytes.len() as u64
                                );
                            }
                        }))
                    }))
                    .context(res.context)
                    .build()
                    .unwrap()
            })
            .boxed()
    }

    fn http_client_service(
        &self,
        subgraph_name: &str,
        service: BoxService<HttpRequest, HttpResponse, BoxError>,
    ) -> BoxService<HttpRequest, HttpResponse, BoxError> {
        let sn_req = Arc::new(subgraph_name.to_string());
        let sn_res = sn_req.clone();
        service
            // Count the number of bytes per subgraph fetch request
            .map_request(move |req: HttpRequest| {
                let sn = sn_req.clone();
                HttpRequest {
                    http_request: req.http_request.map(move |body| {
                        let sn = sn.clone();
                        let size_hint = body.size_hint();

                        // Short-circuit for complete bodies
                        //
                        // If the `SizeHint` gives us an exact value, we can use this for the
                        // metric and return without wrapping the request Body into a stream.
                        if let Some(size) = size_hint.exact() {
                            let sn = sn.clone();
                            u64_counter!(
                                "apollo.router.operations.fetch.request_size",
                                "Total number of request bytes for subgraph fetches",
                                size,
                                subgraph.name = sn.to_string()
                            );
                            return body;
                        }

                        // For streaming bodies, we need to wrap the stream and count bytes as we go
                        router::body::from_result_stream(body.into_data_stream().inspect(
                            move |res| {
                                if let Ok(bytes) = res {
                                    let sn = sn.clone();
                                    u64_counter!(
                                        "apollo.router.operations.fetch.request_size",
                                        "Total number of request bytes for subgraph fetches",
                                        bytes.len() as u64,
                                        subgraph.name = sn.to_string()
                                    );
                                }
                            },
                        ))
                    }),
                    context: req.context,
                }
            })
            // Count the number of fetches, and the number of bytes per subgraph fetch response
            .map_result(move |res| {
                let sn = sn_res.clone();
                match res {
                    Ok(res) => {
                        u64_counter!(
                            "apollo.router.operations.fetch",
                            "Number of subgraph fetches",
                            1u64,
                            subgraph.name = sn.to_string(),
                            client_error = false,
                            http.response.status_code = res.http_response.status().as_u16() as i64
                        );
                        let sn = sn_res.clone();
                        Ok(HttpResponse {
                            http_response: res.http_response.map(move |body| {
                                let sn = sn.clone();
                                router::body::from_result_stream(body.into_data_stream().inspect(
                                    move |res| {
                                        if let Ok(bytes) = res {
                                            let sn = sn.clone();
                                            u64_counter!(
                                            "apollo.router.operations.fetch.response_size",
                                            "Total number of response bytes for subgraph fetches",
                                            bytes.len() as u64,
                                            subgraph.name = sn.to_string()
                                        );
                                        }
                                    },
                                ))
                            }),
                            context: res.context,
                        })
                    }
                    Err(err) => {
                        u64_counter!(
                            "apollo.router.operations.fetch",
                            "Number of subgraph fetches",
                            1u64,
                            subgraph.name = sn.to_string(),
                            client_error = true
                        );
                        Err(err)
                    }
                }
            })
            .boxed()
    }
}

#[cfg(not(target_os = "linux"))]
fn detect_cpu_count(system: &System) -> (&'static str, u64) {
    ("system", system.cpus().len() as u64)
}

// Because Linux provides CGroups as a way of controlling the proportion of CPU time each
// process gets we can perform slightly more introspection here than simply appealing to the
// raw number of processors. Hence, the extra logic including below.
#[cfg(target_os = "linux")]
fn detect_cpu_count(system: &System) -> (&'static str, u64) {
    use std::fs;

    let system_cpus = system.cpus().len() as u64;
    // Grab the contents of /proc/filesystems
    match fs::read_to_string("/proc/filesystems").map(|fs| detect_cgroup_version(&fs)) {
        Ok(CGroupVersion::CGroup2) => {
            // If we're looking at cgroup2 then we need to look in `cpu.max`
            match fs::read_to_string("/sys/fs/cgroup/cpu.max") {
                Ok(readings) => {
                    // The format of the file lists the quota first, followed by the period,
                    // but the quota could also be max which would mean there are no restrictions.
                    if readings.starts_with("max") {
                        ("system", system_cpus)
                    } else {
                        // If it's not max then divide the two to get an integer answer
                        match readings.split_once(' ') {
                            None => ("system", system_cpus),
                            Some((quota, period)) => (
                                "cgroup2",
                                calculate_cpu_count_with_default(system_cpus, quota, period),
                            ),
                        }
                    }
                }
                Err(_) => ("system", system_cpus),
            }
        }
        Ok(CGroupVersion::CGroup) => {
            // If we're in cgroup v1 then we need to read from two separate files
            let quota = fs::read_to_string("/sys/fs/cgroup/cpu/cpu.cfs_quota_us")
                .map(|s| String::from(s.trim()))
                .ok();
            let period = fs::read_to_string("/sys/fs/cgroup/cpu/cpu.cfs_period_us")
                .map(|s| String::from(s.trim()))
                .ok();
            match (quota, period) {
                (Some(quota), Some(period)) => {
                    // In v1 quota being -1 indicates no restrictions so return the maximum (all
                    // system CPUs) otherwise divide the two.
                    if quota == "-1" {
                        ("system", system_cpus)
                    } else {
                        (
                            "cgroup",
                            calculate_cpu_count_with_default(system_cpus, &quota, &period),
                        )
                    }
                }
                _ => ("system", system_cpus),
            }
        }
        // Error reading the file or no cgroup support
        _ => ("system", system_cpus),
    }
}

/// Detect the cgroup version supported in Linux based on the content of the `/proc/filesystems`
/// file
#[allow(unused)]
fn detect_cgroup_version(filesystems: &str) -> CGroupVersion {
    use std::collections::HashSet;
    let versions: HashSet<_> = filesystems
        .lines()
        .flat_map(|line: &str| line.split_whitespace())
        .filter(|x| x.contains("cgroup"))
        .collect();

    if versions.contains("cgroup2") {
        CGroupVersion::CGroup2
    } else if versions.contains("cgroup") {
        CGroupVersion::CGroup
    } else {
        CGroupVersion::None
    }
}

#[allow(unused)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum CGroupVersion {
    CGroup2,
    CGroup,
    #[default]
    None,
}

#[cfg(target_os = "linux")]
fn calculate_cpu_count_with_default(default: u64, quota: &str, period: &str) -> u64 {
    if let (Ok(q), Ok(p)) = (quota.parse::<u64>(), period.parse::<u64>()) {
        q / p
    } else {
        default
    }
}

fn get_otel_arch() -> &'static str {
    match ARCH {
        "x86_64" => "amd64",
        "aarch64" => "arm64",
        "arm" => "arm32",
        "powerpc" => "ppc32",
        "powerpc64" => "ppc64",
        a => a,
    }
}

fn get_otel_os() -> &'static str {
    match OS {
        "apple" => "darwin",
        "dragonfly" => "dragonflybsd",
        "macos" => "darwin",
        "ios" => "darwin",
        a => a,
    }
}

fn get_deployment_type(official_helm_chart: Option<&str>, deployment_type: Option<&str>) -> String {
    if official_helm_chart.is_some() {
        OFFICIAL_HELM_CHART.to_string()
    } else if let Some(val) = deployment_type
        && !val.is_empty()
    {
        // Only allow specific deployment types
        match val {
            UNKNOWN | OFFICIAL_HELM_CHART | OPERATOR => val.to_string(),
            _ => {
                // Invalid deployment type, fall back to unknown
                tracing::warn!(
                    "Invalid deployment type '{}', falling back to '{}'",
                    val,
                    UNKNOWN
                );
                UNKNOWN.to_string()
            }
        }
    } else {
        UNKNOWN.to_string()
    }
}

register_private_plugin!("apollo", "fleet_detector", FleetDetector);

#[cfg(test)]
mod tests {
    use std::convert::Infallible;

    use http::StatusCode;
    use tower::Service as _;

    use super::*;
    use crate::metrics::FutureMetricsExt as _;
    use crate::plugin::test::MockHttpClientService;
    use crate::plugin::test::MockRouterService;
    use crate::services::router::Body;

    #[tokio::test]
    async fn test_enabled_router_service() {
        async {
            let plugin = FleetDetector::default();

            // GIVEN a router service request
            let mut mock_bad_request_service = MockRouterService::new();
            mock_bad_request_service
                .expect_call()
                .times(1)
                .returning(|req: router::Request| {
                    router::Response::http_response_builder()
                        .context(req.context)
                        .response(
                            http::Response::builder()
                                .status(StatusCode::BAD_REQUEST)
                                .header("content-type", "application/json")
                                // making sure the request body is consumed
                                .body(req.router_request.into_body())
                                .unwrap(),
                        )
                        .build()
                });
            let mut bad_request_router_service =
                plugin.router_service(mock_bad_request_service.boxed());
            let router_req = router::Request::fake_builder()
                .body(router::body::from_bytes("request"))
                .build()
                .unwrap();
            let _router_response = bad_request_router_service
                .ready()
                .await
                .unwrap()
                .call(router_req)
                .await
                .unwrap()
                .next_response()
                .await
                .unwrap();

            // THEN operation size metrics should exist
            assert_counter!("apollo.router.operations.request_size", 7, &[]);
            assert_counter!("apollo.router.operations.response_size", 7, &[]);
        }
        .with_metrics()
        .await;
    }

    #[tokio::test]
    async fn test_enabled_http_client_service_full() {
        async {
            let plugin = FleetDetector::default();

            // GIVEN an http client service request with a complete body
            let mut mock_bad_request_service = MockHttpClientService::new();
            mock_bad_request_service
                .expect_call()
                .times(1)
                .returning(|req| {
                    Box::pin(async {
                        Ok(http::Response::builder()
                            .status(StatusCode::BAD_REQUEST)
                            .header("content-type", "application/json")
                            // making sure the request body is consumed
                            .body(req.into_body())
                            .unwrap())
                    })
                });
            let mut bad_request_http_client_service = plugin.http_client_service(
                "subgraph",
                mock_bad_request_service
                    .map_request(|req: HttpRequest| req.http_request)
                    .map_response(|res| HttpResponse {
                        http_response: res,
                        context: Default::default(),
                    })
                    .boxed(),
            );
            let http_client_req = HttpRequest {
                http_request: http::Request::builder()
                    .body(router::body::from_bytes("request"))
                    .unwrap(),
                context: Default::default(),
            };
            let http_client_response = bad_request_http_client_service
                .ready()
                .await
                .unwrap()
                .call(http_client_req)
                .await
                .unwrap();

            // making sure the response body is consumed
            let _data = http_client_response
                .http_response
                .into_body()
                .collect()
                .await
                .unwrap();

            // THEN fetch metrics should exist
            assert_counter!(
                "apollo.router.operations.fetch",
                1,
                &[
                    KeyValue::new("subgraph.name", "subgraph"),
                    KeyValue::new("http.response.status_code", 400),
                    KeyValue::new("client_error", false)
                ]
            );
            assert_counter!(
                "apollo.router.operations.fetch.request_size",
                7,
                &[KeyValue::new("subgraph.name", "subgraph"),]
            );
            assert_counter!(
                "apollo.router.operations.fetch.response_size",
                7,
                &[KeyValue::new("subgraph.name", "subgraph"),]
            );
        }
        .with_metrics()
        .await;
    }

    #[tokio::test]
    async fn test_enabled_http_client_service_stream() {
        async {
            let plugin = FleetDetector::default();

            // GIVEN an http client service request with a streaming body
            let mut mock_bad_request_service = MockHttpClientService::new();
            mock_bad_request_service.expect_call().times(1).returning(
                |req: http::Request<Body>| {
                    Box::pin(async {
                        // making sure the request body is consumed
                        let data = router::body::into_bytes(req.into_body()).await?;
                        Ok(http::Response::builder()
                            .status(StatusCode::BAD_REQUEST)
                            .header("content-type", "application/json")
                            .body(router::body::from_bytes(data))
                            .unwrap())
                    })
                },
            );
            let mut bad_request_http_client_service = plugin.http_client_service(
                "subgraph",
                mock_bad_request_service
                    .map_request(|req: HttpRequest| req.http_request)
                    .map_response(|res: http::Response<Body>| HttpResponse {
                        http_response: res.map(Body::from),
                        context: Default::default(),
                    })
                    .boxed(),
            );
            let http_client_req = HttpRequest {
                http_request: http::Request::builder()
                    .body(router::body::from_result_stream(futures::stream::once(
                        async { Ok::<_, Infallible>(bytes::Bytes::from("request")) },
                    )))
                    .unwrap(),
                context: Default::default(),
            };
            let http_client_response = bad_request_http_client_service
                .ready()
                .await
                .unwrap()
                .call(http_client_req)
                .await
                .unwrap();

            // making sure the response body is consumed
            let _data = router::body::into_bytes(http_client_response.http_response.into_body())
                .await
                .unwrap();

            // THEN fetch metrics should exist
            assert_counter!(
                "apollo.router.operations.fetch",
                1,
                &[
                    KeyValue::new("subgraph.name", "subgraph"),
                    KeyValue::new("http.response.status_code", 400),
                    KeyValue::new("client_error", false)
                ]
            );
            assert_counter!(
                "apollo.router.operations.fetch.request_size",
                7,
                &[KeyValue::new("subgraph.name", "subgraph"),]
            );
            assert_counter!(
                "apollo.router.operations.fetch.response_size",
                7,
                &[KeyValue::new("subgraph.name", "subgraph"),]
            );
        }
        .with_metrics()
        .await;
    }

    #[test]
    fn test_detect_cgroup_version_2() {
        const PROC_FILESYSTEMS_CGROUP2: &str = "nodev   proc
nodev   cgroup
nodev   cgroup2
        ext3
        ext2
        ext4";

        let res = detect_cgroup_version(PROC_FILESYSTEMS_CGROUP2);
        assert_eq!(res, CGroupVersion::CGroup2)
    }

    #[test]
    fn test_detect_cgroup_version_1() {
        const PROC_FILESYSTEMS_CGROUP2: &str = "nodev   proc
nodev   cgroup
        ext3
        ext2
        ext4";

        let res = detect_cgroup_version(PROC_FILESYSTEMS_CGROUP2);
        assert_eq!(res, CGroupVersion::CGroup)
    }

    #[test]
    fn test_detect_cgroup_version_none() {
        const PROC_FILESYSTEMS_CGROUP2: &str = "nodev   proc
        ext3
        ext2
        ext4";

        let res = detect_cgroup_version(PROC_FILESYSTEMS_CGROUP2);
        assert_eq!(res, CGroupVersion::None)
    }

    #[test]
    fn test_get_deployment_type_official_helm_chart() {
        assert_eq!(get_deployment_type(Some("true"), None), OFFICIAL_HELM_CHART);
    }

    #[test]
    fn test_get_deployment_type_custom() {
        assert_eq!(
            get_deployment_type(None, Some("custom_deployment")),
            UNKNOWN
        );
    }

    #[test]
    fn test_get_deployment_type_custom_empty() {
        assert_eq!(get_deployment_type(None, Some("")), UNKNOWN);
    }

    #[test]
    fn test_get_deployment_type_default() {
        assert_eq!(get_deployment_type(None, None), UNKNOWN);
    }

    #[test]
    fn test_get_deployment_type_priority() {
        // Set both environment variables - official helm chart should take priority
        assert_eq!(
            get_deployment_type(Some("true"), Some("custom_deployment")),
            OFFICIAL_HELM_CHART
        );
    }

    #[test]
    fn test_get_deployment_type_valid_values() {
        // Test that valid deployment types are accepted
        assert_eq!(get_deployment_type(None, Some(UNKNOWN)), UNKNOWN);
        assert_eq!(
            get_deployment_type(None, Some(OFFICIAL_HELM_CHART)),
            OFFICIAL_HELM_CHART
        );
        assert_eq!(get_deployment_type(None, Some(OPERATOR)), OPERATOR);
    }
}
