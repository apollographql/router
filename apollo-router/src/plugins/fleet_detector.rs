use std::env;
use std::env::consts::ARCH;
use std::env::consts::OS;
use std::sync::Arc;
use std::sync::Mutex;
use std::time::Duration;
use std::time::Instant;

use futures::StreamExt;
use opentelemetry::metrics::MeterProvider;
use opentelemetry_api::metrics::ObservableGauge;
use opentelemetry_api::metrics::Unit;
use opentelemetry_api::KeyValue;
use schemars::JsonSchema;
use serde::Deserialize;
use sysinfo::System;
use tower::util::BoxService;
use tower::BoxError;
use tower::ServiceExt as _;
use tracing::debug;

use crate::executable::APOLLO_TELEMETRY_DISABLED;
use crate::metrics::meter_provider;
use crate::plugin::PluginInit;
use crate::plugin::PluginPrivate;
use crate::services::http::HttpRequest;
use crate::services::http::HttpResponse;
use crate::services::router;
use crate::services::router::body::RouterBody;

const REFRESH_INTERVAL: Duration = Duration::from_secs(60);
const COMPUTE_DETECTOR_THRESHOLD: u16 = 24576;
const OFFICIAL_HELM_CHART_VAR: &str = "APOLLO_ROUTER_OFFICIAL_HELM_CHART";

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
        system.refresh_all();
        Self {
            system,
            start: Instant::now(),
        }
    }

    fn get_system(&mut self) -> &System {
        if self.start.elapsed() < REFRESH_INTERVAL {
            &self.system
        } else {
            self.start = Instant::now();
            self.system.refresh_cpu_all();
            self.system.refresh_memory();
            &self.system
        }
    }
}

#[derive(Default)]
enum GaugeStore {
    #[default]
    Disabled,
    Pending,
    Active(Vec<ObservableGauge<u64>>),
}

impl GaugeStore {
    fn active() -> GaugeStore {
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
            attributes.push(KeyValue::new("deployment.type", get_deployment_type()));
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
                    .with_unit(Unit::new("Mhz"))
                    .with_callback(move |i| {
                        let local_system_getter = system_getter.clone();
                        let mut system_getter = local_system_getter.lock().unwrap();
                        let system = system_getter.get_system();
                        let cpus = system.cpus();
                        let cpu_freq =
                            cpus.iter().map(|cpu| cpu.frequency()).sum::<u64>() / cpus.len() as u64;
                        i.observe(cpu_freq, &[])
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
                    .with_callback(move |i| {
                        let local_system_getter = system_getter.clone();
                        let mut system_getter = local_system_getter.lock().unwrap();
                        let system = system_getter.get_system();
                        let cpu_count = detect_cpu_count(system);
                        i.observe(cpu_count, &[KeyValue::new("host.arch", get_otel_arch())])
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
                    .with_callback(move |i| {
                        let local_system_getter = system_getter.clone();
                        let mut system_getter = local_system_getter.lock().unwrap();
                        let system = system_getter.get_system();
                        i.observe(
                            system.total_memory(),
                            &[KeyValue::new("host.arch", get_otel_arch())],
                        )
                    })
                    .with_unit(Unit::new("bytes"))
                    .init(),
            );
        }
        GaugeStore::Active(gauges)
    }
}

#[derive(Default)]
struct FleetDetector {
    enabled: bool,
    gauge_store: Mutex<GaugeStore>,
}
#[async_trait::async_trait]
impl PluginPrivate for FleetDetector {
    type Config = Conf;

    async fn new(_: PluginInit<Self::Config>) -> Result<Self, BoxError> {
        debug!("initialising fleet detection plugin");
        if let Ok(val) = env::var(APOLLO_TELEMETRY_DISABLED) {
            if val == "true" {
                debug!("fleet detection disabled, no telemetry will be sent");
                return Ok(FleetDetector::default());
            }
        }

        Ok(FleetDetector {
            enabled: true,
            gauge_store: Mutex::new(GaugeStore::Pending),
        })
    }

    fn activate(&self) {
        let mut store = self.gauge_store.lock().expect("lock poisoned");
        if matches!(*store, GaugeStore::Pending) {
            *store = GaugeStore::active();
        }
    }

    fn router_service(&self, service: router::BoxService) -> router::BoxService {
        if !self.enabled {
            return service;
        }

        service
            // Count the number of request bytes from clients to the router
            .map_request(move |req: router::Request| router::Request {
                router_request: req.router_request.map(move |body| {
                    router::Body::wrap_stream(body.map(move |res| {
                        res.map(move |bytes| {
                            u64_counter!(
                                "apollo.router.operations.request_size",
                                "Total number of request bytes from clients",
                                bytes.len() as u64
                            );
                            bytes
                        })
                    }))
                }),
                context: req.context,
            })
            // Count the number of response bytes from the router to clients
            .map_response(move |res: router::Response| router::Response {
                response: res.response.map(move |body| {
                    router::Body::wrap_stream(body.map(move |res| {
                        res.map(move |bytes| {
                            u64_counter!(
                                "apollo.router.operations.response_size",
                                "Total number of response bytes to clients",
                                bytes.len() as u64
                            );
                            bytes
                        })
                    }))
                }),
                context: res.context,
            })
            .boxed()
    }

    fn http_client_service(
        &self,
        subgraph_name: &str,
        service: BoxService<HttpRequest, HttpResponse, BoxError>,
    ) -> BoxService<HttpRequest, HttpResponse, BoxError> {
        if !self.enabled {
            return service;
        }
        let sn_req = Arc::new(subgraph_name.to_string());
        let sn_res = sn_req.clone();
        service
            // Count the number of bytes per subgraph fetch request
            .map_request(move |req: HttpRequest| {
                let sn = sn_req.clone();
                HttpRequest {
                    http_request: req.http_request.map(move |body| {
                        let sn = sn.clone();
                        RouterBody::wrap_stream(body.map(move |res| {
                            let sn = sn.clone();
                            res.map(move |bytes| {
                                u64_counter!(
                                    "apollo.router.operations.fetch.request_size",
                                    "Total number of request bytes for subgraph fetches",
                                    bytes.len() as u64,
                                    subgraph.service.name = sn.to_string()
                                );
                                bytes
                            })
                        }))
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
                            subgraph.service.name = sn.to_string(),
                            http.response.status_code = res.http_response.status().as_u16() as i64
                        );
                        let sn = sn_res.clone();
                        Ok(HttpResponse {
                            http_response: res.http_response.map(move |body| {
                                let sn = sn.clone();
                                RouterBody::wrap_stream(body.map(move |res| {
                                    let sn = sn.clone();
                                    res.map(move |bytes| {
                                        u64_counter!(
                                            "apollo.router.operations.fetch.response_size",
                                            "Total number of response bytes for subgraph fetches",
                                            bytes.len() as u64,
                                            subgraph.service.name = sn.to_string()
                                        );
                                        bytes
                                    })
                                }))
                            }),
                            context: res.context,
                        })
                    }
                    Err(err) => {
                        // On fetch errors, report the status code as 0
                        u64_counter!(
                            "apollo.router.operations.fetch",
                            "Number of subgraph fetches",
                            1u64,
                            subgraph.service.name = sn.to_string(),
                            http.response.status_code = 0i64
                        );
                        Err(err)
                    }
                }
            })
            .boxed()
    }
}

#[cfg(not(target_os = "linux"))]
fn detect_cpu_count(system: &System) -> u64 {
    system.cpus().len() as u64
}

// Because Linux provides CGroups as a way of controlling the proportion of CPU time each
// process gets we can perform slightly more introspection here than simply appealing to the
// raw number of processors. Hence, the extra logic including below.
#[cfg(target_os = "linux")]
fn detect_cpu_count(system: &System) -> u64 {
    use std::collections::HashSet;
    use std::fs;

    let system_cpus = system.cpus().len() as u64;
    // Grab the contents of /proc/filesystems
    let fses: HashSet<String> = match fs::read_to_string("/proc/filesystems") {
        Ok(content) => content
            .lines()
            .map(|x| x.split_whitespace().next().unwrap_or("").to_string())
            .filter(|x| x.contains("cgroup"))
            .collect(),
        Err(_) => return system_cpus,
    };

    if fses.contains("cgroup2") {
        // If we're looking at cgroup2 then we need to look in `cpu.max`
        match fs::read_to_string("/sys/fs/cgroup/cpu.max") {
            Ok(readings) => {
                // The format of the file lists the quota first, followed by the period,
                // but the quota could also be max which would mean there are no restrictions.
                if readings.starts_with("max") {
                    system_cpus
                } else {
                    // If it's not max then divide the two to get an integer answer
                    match readings.split_once(' ') {
                        None => system_cpus,
                        Some((quota, period)) => {
                            calculate_cpu_count_with_default(system_cpus, quota, period)
                        }
                    }
                }
            }
            Err(_) => system_cpus,
        }
    } else if fses.contains("cgroup") {
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
                    system_cpus
                } else {
                    calculate_cpu_count_with_default(system_cpus, &quota, &period)
                }
            }
            _ => system_cpus,
        }
    } else {
        system_cpus
    }
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

fn get_deployment_type() -> &'static str {
    // Official Apollo helm chart
    if std::env::var_os(OFFICIAL_HELM_CHART_VAR).is_some() {
        return "official_helm_chart";
    }
    "unknown"
}

register_private_plugin!("apollo", "fleet_detector", FleetDetector);
