use std::time::Duration;
use std::time::SystemTime;

use wiremock::Mock;
use wiremock::MockServer;
use wiremock::ResponseTemplate;
use wiremock::matchers::method;
use wiremock::matchers::path;

use super::*;

fn no_env(_: &str) -> Option<String> {
    None
}

fn config(json: serde_json::Value) -> Config {
    serde_json::from_value(json).expect("valid config")
}

fn datadog_config(endpoint: &str) -> Config {
    config(serde_json::json!({
        "heap": {"enabled": true},
        "exporters": {"datadog": {"enabled": true, "endpoint": endpoint}},
    }))
}

async fn mock_agent(status: u16) -> MockServer {
    let agent = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/profiling/v1/input"))
        .respond_with(ResponseTemplate::new(status))
        .mount(&agent)
        .await;
    agent
}

#[test]
fn disabled_by_default() {
    let collector = Collector::from_config(&config(serde_json::json!({})), no_env).unwrap();
    assert!(collector.is_none());
}

#[test]
fn heap_without_exporter_is_rejected() {
    let config = config(serde_json::json!({"heap": {"enabled": true}}));
    let err = Collector::from_config(&config, no_env)
        .err()
        .expect("error");
    assert!(err.to_string().contains("no exporter"), "{err}");
}

#[test]
fn sub_second_interval_is_rejected() {
    let mut config = datadog_config("http://agent.invalid");
    config.interval = Duration::ZERO;
    let err = Collector::from_config(&config, no_env)
        .err()
        .expect("error");
    assert!(err.to_string().contains("at least 1s"), "{err}");
}

fn datadog_exporter(endpoint: &str) -> datadog::DatadogExporter {
    datadog::DatadogExporter::new(&datadog_config(endpoint).exporters.datadog, no_env).unwrap()
}

fn empty_profile() -> Profile {
    Profile {
        start: SystemTime::now(),
        end: SystemTime::now(),
        pprof: vec![],
    }
}

#[tokio::test]
async fn export_failures_are_counted() {
    async {
        let agent = mock_agent(500).await;
        let mut exporters = [Exporter::new(Box::new(datadog_exporter(&agent.uri())))];
        export_all(&mut exporters, &empty_profile()).await;

        assert_eq!(agent.received_requests().await.unwrap().len(), 1);
        assert_counter!(
            "apollo.router.profiling.exports",
            1,
            "profiling.type" = "heap",
            "profiling.exporter" = "datadog",
            "profiling.outcome" = "failure"
        );
    }
    .with_metrics()
    .await;
}

#[tokio::test]
async fn unreachable_agent_fails_the_export() {
    // Bind an ephemeral port, then free it, so nothing is listening there.
    let addr = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap();
    let err = datadog_exporter(&format!("http://{addr}"))
        .export(&empty_profile())
        .await
        .expect_err("nothing is listening");
    let err = err.downcast_ref::<reqwest::Error>().expect("reqwest error");
    assert!(err.is_connect(), "{err}");
}

#[tokio::test]
async fn hung_agent_times_out() {
    let agent = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_delay(Duration::from_secs(60)))
        .mount(&agent)
        .await;
    let err = datadog_exporter(&agent.uri())
        .with_timeout(Duration::from_millis(100))
        .export(&empty_profile())
        .await
        .expect_err("the agent never responds");
    let err = err.downcast_ref::<reqwest::Error>().expect("reqwest error");
    assert!(err.is_timeout(), "{err}");
}

/// Fails the exports it's told to, then succeeds.
struct FlakyExporter {
    failures: Mutex<usize>,
}

#[async_trait::async_trait]
impl ProfileExporter for FlakyExporter {
    fn name(&self) -> &'static str {
        // Unique, because captured logs are shared with other tests.
        "flaky-test-exporter"
    }

    async fn export(&self, _profile: &Profile) -> Result<(), BoxError> {
        let mut failures = self.failures.lock();
        if *failures == 0 {
            return Ok(());
        }
        *failures -= 1;
        Err("agent unavailable".into())
    }
}

#[tokio::test]
async fn persistent_export_failures_warn_once_until_recovery() {
    let _guard = crate::test_harness::tracing_test::dispatcher_guard();
    let mut exporters = [Exporter::new(Box::new(FlakyExporter {
        failures: Mutex::new(3),
    }))];
    for _ in 0..4 {
        export_all(&mut exporters, &empty_profile()).await;
    }

    crate::test_harness::tracing_test::logs_assert(|lines| {
        let count = |level: &str, message: &str| {
            lines
                .iter()
                .filter(|line| {
                    line.contains("flaky-test-exporter")
                        && line.contains(level)
                        && line.contains(message)
                })
                .count()
        };
        let counts = (
            count("WARN", "failed to export heap profile"),
            count("DEBUG", "failed to export heap profile"),
            count("INFO", "exporting heap profiles recovered"),
        );
        if counts == (1, 2, 1) {
            Ok(())
        } else {
            Err(format!("(warn, debug, info) = {counts:?}"))
        }
    })
    .unwrap();
}

#[cfg(not(all(
    feature = "global-allocator",
    not(feature = "dhat-heap"),
    target_os = "linux"
)))]
#[test]
fn unsupported_platform_disables_profiling_without_failing() {
    let collector =
        Collector::from_config(&datadog_config("http://agent.invalid"), no_env).unwrap();
    assert!(collector.is_none());
}

#[cfg(all(
    feature = "global-allocator",
    not(feature = "dhat-heap"),
    target_os = "linux"
))]
mod supported {
    use std::convert::Infallible;
    use std::io::Read;

    use super::*;
    use crate::allocator::JEMALLOC_PROF_TEST_LOCK;

    /// Just the parts of pprof's `Profile` message these tests look at.
    #[derive(Clone, PartialEq, prost::Message)]
    struct PprofProfile {
        #[prost(message, repeated, tag = "1")]
        sample_type: Vec<PprofValueType>,
        #[prost(message, repeated, tag = "2")]
        sample: Vec<PprofSample>,
        #[prost(string, repeated, tag = "6")]
        string_table: Vec<String>,
    }

    #[derive(Clone, PartialEq, prost::Message)]
    struct PprofValueType {
        #[prost(int64, tag = "1")]
        r#type: i64,
        #[prost(int64, tag = "2")]
        unit: i64,
    }

    #[derive(Clone, PartialEq, prost::Message)]
    struct PprofSample {
        #[prost(int64, repeated, tag = "2")]
        value: Vec<i64>,
    }

    /// The multipart parts of a request received by the fake agent, as (name, bytes).
    async fn multipart_parts(request: &wiremock::Request) -> Vec<(String, Vec<u8>)> {
        let content_type = request
            .headers
            .get(http::header::CONTENT_TYPE)
            .expect("content type")
            .to_str()
            .unwrap();
        let boundary = multer::parse_boundary(content_type).expect("multipart boundary");
        let body = bytes::Bytes::from(request.body.clone());
        let mut multipart = multer::Multipart::new(
            futures::stream::once(async move { Ok::<_, Infallible>(body) }),
            boundary,
        );
        let mut parts = Vec::new();
        while let Some(field) = multipart.next_field().await.unwrap() {
            let name = field.name().unwrap().to_string();
            parts.push((name, field.bytes().await.unwrap().to_vec()));
        }
        parts
    }

    /// Polls `condition` until it holds, failing after `timeout`.
    async fn wait_until<F: Future<Output = bool>>(
        what: &str,
        timeout: Duration,
        mut condition: impl FnMut() -> F,
    ) {
        tokio::time::timeout(timeout, async {
            while !condition().await {
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await
        .unwrap_or_else(|_| panic!("timed out waiting until {what}"));
    }

    fn running_owner() -> Option<u64> {
        RUNNING.lock().as_ref().map(|running| running.owner)
    }

    fn window_start() -> Option<SystemTime> {
        RUNNING
            .lock()
            .as_ref()
            .map(|running| *running.window_start.lock())
    }

    /// Allocations made here should show up, by name, in the heap profile.
    #[inline(never)]
    fn allocate_for_profile() -> Vec<Vec<u8>> {
        (0..64)
            .map(|_| std::hint::black_box(vec![1u8; 1024 * 1024]))
            .collect()
    }

    /// The router's own jemalloc → symbolized pprof → a fake Datadog agent.
    #[tokio::test]
    async fn exports_symbolized_heap_profile_to_datadog_agent() {
        let _lock = JEMALLOC_PROF_TEST_LOCK.lock().await;
        async {
            let agent = mock_agent(200).await;
            let mut collector = Collector::from_config(&datadog_config(&agent.uri()), no_env)
                .unwrap()
                .expect("heap profiling is supported here");

            heap::set_active(true).unwrap();
            let held = allocate_for_profile();
            let profile = collector
                .collect(
                    SystemTime::now() - Duration::from_secs(60),
                    SystemTime::now(),
                )
                .await
                .expect("collected a heap profile");
            export_all(&mut collector.exporters, &profile).await;
            drop(held);
            heap::set_active(false).unwrap();

            assert_counter!(
                "apollo.router.profiling.collections",
                1,
                "profiling.type" = "heap",
                "profiling.outcome" = "success"
            );
            assert_counter!(
                "apollo.router.profiling.exports",
                1,
                "profiling.type" = "heap",
                "profiling.exporter" = "datadog",
                "profiling.outcome" = "success"
            );

            let requests = agent.received_requests().await.unwrap();
            assert_eq!(requests.len(), 1);
            let request = &requests[0];
            assert_eq!(
                request.headers.get("DD-EVP-ORIGIN").unwrap(),
                "apollo-router"
            );

            let parts = multipart_parts(request).await;
            let names: Vec<_> = parts.iter().map(|(name, _)| name.as_str()).collect();
            assert_eq!(names, ["event", "profile.pprof"]);

            let event: serde_json::Value = serde_json::from_slice(&parts[0].1).unwrap();
            assert_eq!(event["family"], "native");
            assert_eq!(event["version"], "4");
            assert_eq!(event["attachments"], serde_json::json!(["profile.pprof"]));
            let tags = event["tags_profiler"].as_str().unwrap();
            assert!(tags.starts_with("service:router,"), "{tags}");

            let mut pprof = Vec::new();
            flate2::read::GzDecoder::new(parts[1].1.as_slice())
                .read_to_end(&mut pprof)
                .expect("gzipped pprof");
            let pprof = <PprofProfile as prost::Message>::decode(pprof.as_slice()).unwrap();
            let sample_type = &pprof.sample_type[0];
            assert_eq!(
                pprof.string_table[sample_type.r#type as usize],
                "inuse-space"
            );
            assert_eq!(pprof.string_table[sample_type.unit as usize], "bytes");

            // The 64MiB held above (sampled, then scaled back up by jemalloc's sampling rate).
            let total: i64 = pprof.sample.iter().map(|s| s.value[0]).sum();
            assert!(total >= 32 * 1024 * 1024, "in-use bytes: {total}");
            assert!(
                pprof
                    .string_table
                    .iter()
                    .any(|s| s.ends_with("supported::allocate_for_profile")),
                "profile is symbolized in-process"
            );
        }
        .with_metrics()
        .await;
    }

    /// A reload's new pipeline takes over the one loop; the old pipeline going away doesn't stop it.
    #[tokio::test]
    async fn reload_hands_over_a_single_loop() {
        let _lock = JEMALLOC_PROF_TEST_LOCK.lock().await;
        heap::set_active(false).unwrap();
        let plugin = || async {
            let init =
                PluginInit::fake_new(datadog_config("http://agent.invalid"), Default::default());
            <ContinuousProfiling as PluginPrivate>::new(init)
                .await
                .unwrap()
        };

        let old = plugin().await;
        old.activate();
        assert_eq!(running_owner(), Some(old.id));
        assert!(heap::is_active().unwrap());

        let new = plugin().await;
        new.activate();
        assert_eq!(running_owner(), Some(new.id));

        drop(old);
        assert_eq!(running_owner(), Some(new.id));
        assert!(heap::is_active().unwrap());

        drop(new);
        assert_eq!(running_owner(), None);
        assert!(!heap::is_active().unwrap());
    }

    /// Through the real router: config → plugin → activation → uploads, then stopping with it.
    #[tokio::test]
    async fn runs_with_the_router_and_stops_with_it() {
        let _lock = JEMALLOC_PROF_TEST_LOCK.lock().await;
        async {
            heap::set_active(false).unwrap();
            let agent = mock_agent(200).await;
            let router = crate::TestHarness::builder()
                .configuration_json(serde_json::json!({
                    "experimental_continuous_profiling": {
                        "interval": "1s",
                        "heap": {"enabled": true},
                        "exporters": {"datadog": {"enabled": true, "endpoint": agent.uri()}},
                    }
                }))
                .unwrap()
                .build_router()
                .await
                .unwrap();

            assert!(heap::is_active().unwrap());
            // The window advances only once a profile has been collected, exported, and counted,
            // so stopping right after can't cut an upload short.
            let first_window = window_start().expect("the loop is running");
            wait_until("a profile is exported", Duration::from_secs(30), || async {
                window_start().is_some_and(|window| window > first_window)
            })
            .await;

            drop(router);
            wait_until("the loop stops", Duration::from_secs(10), || async {
                running_owner().is_none()
            })
            .await;
            assert!(!heap::is_active().unwrap());

            let uploads = agent.received_requests().await.unwrap().len() as u64;
            assert!(uploads >= 1);
            assert_counter!(
                "apollo.router.profiling.exports",
                uploads,
                "profiling.type" = "heap",
                "profiling.exporter" = "datadog",
                "profiling.outcome" = "success"
            );
        }
        .with_metrics()
        .await;
    }
}
