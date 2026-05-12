use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::ffi::OsString;
use std::fs;
use std::net::SocketAddr;
use std::net::TcpListener;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use std::sync::LazyLock;
use std::sync::OnceLock;
use std::time::Duration;
use std::time::SystemTime;
use std::time::UNIX_EPOCH;

use buildstructor::buildstructor;
use flate2::read::GzDecoder;
use fred::clients::Client as RedisClient;
use fred::interfaces::ClientLike;
use fred::interfaces::KeysInterface;
use fred::prelude::Config as RedisConfig;
use fred::types::scan::Scanner;
use futures::StreamExt;
use http::header::ACCEPT;
use http::header::CONTENT_TYPE;
use mime::APPLICATION_JSON;
use opentelemetry::Context;
use opentelemetry::KeyValue;
use opentelemetry::global;
use opentelemetry::propagation::TextMapPropagator;
use opentelemetry::trace::SpanContext;
use opentelemetry::trace::TraceContextExt;
use opentelemetry::trace::TraceId;
use opentelemetry::trace::TracerProvider as OtherTracerProvider;
use opentelemetry_otlp::WithExportConfig;
use opentelemetry_proto::tonic::collector::metrics::v1::ExportMetricsServiceRequest;
use opentelemetry_sdk::Resource;
use opentelemetry_sdk::runtime;
use opentelemetry_sdk::testing::trace::NoopSpanExporter;
use opentelemetry_sdk::trace::BatchConfigBuilder;
use opentelemetry_sdk::trace::SdkTracerProvider;
use opentelemetry_sdk::trace::span_processor_with_async_runtime::BatchSpanProcessor;
use opentelemetry_semantic_conventions::resource::SERVICE_NAME;
use parking_lot::Mutex;
use prost::Message;
use regex::Regex;
use reqwest::Request;
use serde_json::Value;
use serde_json::json;
use tokio::io::AsyncBufReadExt;
use tokio::io::AsyncWriteExt;
use tokio::io::BufReader;
use tokio::process::Child;
use tokio::process::Command;
use tokio::task;
use tokio::time::Instant;
use tracing::info_span;
use tracing_core::Dispatch;
use tracing_core::LevelFilter;
use tracing_futures::Instrument;
use tracing_futures::WithSubscriber;
use tracing_opentelemetry::OpenTelemetrySpanExt;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::Layer;
use tracing_subscriber::Registry;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use uuid::Uuid;
use wiremock::Mock;
use wiremock::Respond;
use wiremock::ResponseTemplate;
use wiremock::http::Method;
use wiremock::matchers::body_string_contains;
use wiremock::matchers::method;
use wiremock::matchers::path;
use wiremock::matchers::path_regex;

/// Redact the query hash in cache debug keys so snapshots are stable.
/// Uses the next `:` after `:hash:` as the end marker (e.g. `:hash:[^:]*`) so it remains
/// correct if additional fields are added between hash and data.
#[allow(dead_code)] // used by integration/response_cache and integration/coprocessor test binaries
pub(crate) fn redact_cache_debug_query_hash(key: &str) -> String {
    static REDACT_HASH_RE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r":hash:[^:]*").expect("redact regex"));
    REDACT_HASH_RE
        .replace(key, ":hash:[query-hash]")
        .into_owned()
}

/// Default test license JWT served by `mock_license_uplink()` and
/// validated by the spawned router against `TEST_JWKS_ENDPOINT` (via
/// `APOLLO_TEST_INTERNAL_UPLINK_JWKS`, set in `IntegrationTest::start()`).
///
/// Mirrors `JWT_WITH_ALLOWED_FEATURES_NONE` in
/// `tests/integration/allowed_features.rs`: signed by the HS256 test
/// secret in `license.jwks.json`, no `allowedFeatures` claim. The
/// router's `LicenseLimits::default()` interprets a missing claim as
/// "all features allowed" (legacy compatibility for licenses minted
/// before the claim was introduced), so the spawned router unlocks
/// commercial features (federated subscriptions, coprocessors, entity
/// caching, traffic shaping, datadog/apollo OTLP telemetry) under
/// this license. Tests that need a *specific* license (eg. expired,
/// or a constrained `allowedFeatures` set) override this default
/// using `.jwt(...)` on the builder, which sets
/// `APOLLO_ROUTER_LICENSE` directly and beats Uplink-fetched
/// licenses via `LicenseSource::Env` precedence.
///
/// The JWT is minted at runtime each time the harness starts a test,
/// using the bundled HS256 test secret and a rolling `warnAt`/`haltAt`
/// of `now() + ~6 months`. This eliminates the periodic-rotation
/// toil a static-pinned JWT would incur. The 6-month horizon stays
/// well within tokio's `Instant`-based `DelayQueue` scheduler cap
/// (the consumer is `apollo-router/src/uplink/license_stream.rs`,
/// which calls `DelayQueue::insert_at(claims.halt_at)`).
static TEST_LICENSE_JWT_FULL_FEATURES: LazyLock<String> = LazyLock::new(mint_test_license_jwt);

/// HS256 test secret bundled in `apollo-router/src/uplink/testdata/license.jwks.json`.
/// JWK format (`oct`/`HS256`/`use=sig`) with `k` base64url-encoded.
/// Decoded value is the byte string `make_a_long_secret_for_rfc_7518_256_bits_requirement_blah`.
const TEST_LICENSE_JWKS_SECRET_BASE64URL: &str =
    "bWFrZV9hX2xvbmdfc2VjcmV0X2Zvcl9yZmNfNzUxOF8yNTZfYml0c19yZXF1aXJlbWVudF9ibGFo";

/// Mint a fresh test license JWT signed with the bundled HS256 test secret.
/// `warnAt` and `haltAt` are pinned to `now() + ~6 months` so the JWT
/// stays valid through any reasonable test session and well within
/// tokio's `DelayQueue` scheduler cap.
fn mint_test_license_jwt() -> String {
    use std::time::SystemTime;
    use std::time::UNIX_EPOCH;

    use base64::Engine;
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;

    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock before unix epoch")
        .as_secs();
    let six_months_secs: u64 = 60 * 60 * 24 * 180;
    let halt_at = now + six_months_secs;

    let secret_bytes = URL_SAFE_NO_PAD
        .decode(TEST_LICENSE_JWKS_SECRET_BASE64URL)
        .expect("test JWKS secret is valid base64url");
    let key = jsonwebtoken::EncodingKey::from_secret(&secret_bytes);
    let header = jsonwebtoken::Header::new(jsonwebtoken::Algorithm::HS256);
    let claims = serde_json::json!({
        "exp": 10000000000_u64,
        "iss": "https://www.apollographql.com/",
        "sub": "apollo",
        "aud": "SELF_HOSTED",
        "warnAt": halt_at,
        "haltAt": halt_at,
    });
    jsonwebtoken::encode(&header, &claims, &key).expect("sign test license JWT")
}

/// Stand up a per-test wiremock that stands in for
/// `uplink.api.apollographql.com`. The harness wires this server's URL
/// into the spawned router as `APOLLO_UPLINK_ENDPOINTS` whenever the
/// router is going to receive an `APOLLO_KEY` (which is the harness
/// default — see `IntegrationTest::start()`), so neither the License
/// poller (`LicenseSource::Registry`) nor the schema poller
/// (`SchemaSource::Registry`) reaches the real public Internet during
/// integration tests.
///
/// Three matchers, in priority order:
///
/// 1. `LicenseQuery` (priority 5) returns
///    `RouterEntitlementsResult { entitlement: { jwt: ... }, .. }`
///    where the JWT is `TEST_LICENSE_JWT_FULL_FEATURES`. The
///    license-stream `From` impl runs `License::from_str` on that
///    JWT, which validates against the JWKS pointed at by
///    `APOLLO_TEST_INTERNAL_UPLINK_JWKS` (also set in `start()`) and
///    yields a license with all commercial features enabled.
///    `apollo.router.uplink.fetch.count.total{status="success",query="License"}`
///    increments on every poll.
///
///    Tests that need a specific license (eg. expired, or a
///    constrained `allowedFeatures` set) call `.jwt(...)` on the
///    builder, which sets `APOLLO_ROUTER_LICENSE` directly. The
///    router's executable orders `LicenseSource::Env` ahead of
///    `LicenseSource::Registry`, so the Uplink mock is bypassed for
///    those tests.
///
/// 2. Catch-all `POST → 200 {"data": null}` (priority 10) for any
///    other Uplink operation a router version might issue (eg.
///    `SupergraphSdlQuery` if a test exercises
///    `SchemaSource::Registry`, or `PersistedQueriesManifestQuery`
///    when `APOLLO_GRAPH_REF` is set during PQ tests). Returns 200
///    with an empty data envelope rather than 404, so the polling
///    loop doesn't enter an error path that pollutes test logs.
///
/// Note: the harness does NOT bootstrap `SchemaSource::Registry`. The
/// typical path pins schema via `--supergraph` at the CLI; tests that
/// would activate Registry (`APOLLO_GRAPH_REF` set in `self.env`
/// without `--supergraph`) will hang in Startup because the catch-all
/// returns no useful payload and `UplinkResponse::Unchanged` cannot
/// bootstrap a missing baseline. If a future test needs Registry-source
/// schema, the mock has to return a real `supergraphSdl` body (mirror
/// the License JWT pattern above).
///
/// Lifted into the harness from a per-test helper that originally
/// lived in `tests/integration/telemetry/metrics.rs::test_metrics_reloading`
/// (`b3a0986e0`).
async fn mock_license_uplink() -> wiremock::MockServer {
    let server = wiremock::MockServer::start().await;

    Mock::given(method(Method::POST))
        .and(body_string_contains("LicenseQuery"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": {
                "routerEntitlements": {
                    "__typename": "RouterEntitlementsResult",
                    "id": "test-license-id",
                    "minDelaySeconds": 1,
                    "entitlement": {
                        "jwt": &*TEST_LICENSE_JWT_FULL_FEATURES,
                    },
                }
            }
        })))
        .with_priority(5)
        .mount(&server)
        .await;

    Mock::given(method(Method::POST))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"data": null})))
        .with_priority(10)
        .mount(&server)
        .await;

    server
}

/// Global registry to keep track of allocated ports across all tests
/// This helps avoid port conflicts between concurrent tests
static ALLOCATED_PORTS: OnceLock<Arc<Mutex<HashMap<u16, String>>>> = OnceLock::new();

/// Global endpoint for JWKS used in testing. If you need to mint a test key, refer to the internal
/// router team's documentation for a script
#[allow(dead_code)]
pub static TEST_JWKS_ENDPOINT: LazyLock<PathBuf> = LazyLock::new(|| {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("src")
        .join("uplink")
        .join("testdata")
        .join("license.jwks.json")
});

fn get_allocated_ports() -> &'static Arc<Mutex<HashMap<u16, String>>> {
    ALLOCATED_PORTS.get_or_init(|| Arc::new(Mutex::new(HashMap::new())))
}

/// Allocate a port that's currently available
/// The port is not actually bound, just marked as allocated to avoid conflicts
fn allocate_port(name: &str) -> std::io::Result<u16> {
    let ports_registry = get_allocated_ports();

    // Try to find an available port
    for _ in 0..100 {
        // Try up to 100 times to find a port
        let listener = TcpListener::bind("127.0.0.1:0")?;
        let port = listener.local_addr()?.port();
        drop(listener); // Release the port immediately

        let mut ports = ports_registry.lock();
        if let Entry::Vacant(e) = ports.entry(port) {
            e.insert(name.to_string());
            return Ok(port);
        }
    }

    Err(std::io::Error::new(
        std::io::ErrorKind::AddrInUse,
        "Could not find available port after 100 attempts",
    ))
}

#[derive(Clone)]
pub struct Query {
    traced: bool,
    psr: Option<&'static str>,
    headers: HashMap<String, String>,
    content_type: String,
    body: Value,
}

impl Default for Query {
    fn default() -> Self {
        Query::builder().build()
    }
}

#[buildstructor::buildstructor]
impl Query {
    #[builder]
    pub fn new(
        traced: Option<bool>,
        psr: Option<&'static str>,
        body: Option<Value>,
        content_type: Option<String>,
        headers: HashMap<String, String>,
    ) -> Self {
        Self {
            traced: traced.unwrap_or(true),
            psr,
            body: body.unwrap_or(
                json!({"query":"query ExampleQuery {topProducts{name}}","variables":{}}),
            ),
            content_type: content_type
                .unwrap_or_else(|| APPLICATION_JSON.essence_str().to_string()),
            headers,
        }
    }
}
impl Query {
    #[allow(dead_code)]
    pub fn with_bad_content_type(mut self) -> Self {
        self.content_type = "garbage".to_string();
        self
    }

    #[allow(dead_code)]
    pub fn with_bad_query(mut self) -> Self {
        self.body = json!({"garbage":{}});
        self
    }

    #[allow(dead_code)]
    pub fn with_invalid_query(mut self) -> Self {
        self.body = json!({"query": "query {anInvalidField}", "variables":{}});
        self
    }

    #[allow(dead_code)]
    pub fn with_anonymous(mut self) -> Self {
        self.body = json!({"query":"query {topProducts{name}}","variables":{}});
        self
    }

    #[allow(dead_code)]
    pub fn with_huge_query(mut self) -> Self {
        self.body = json!({"query":"query {topProducts{name, name, name, name, name, name, name, name, name, name, name, name, name, name, name, name, name, name, name, name, name, name, name, name, name, name, name, name, name, name, name}}","variables":{}});
        self
    }

    #[allow(dead_code)]
    pub fn introspection() -> Query {
        Query::builder()
            .body(json!({"query":"{__schema {types {name}}}","variables":{}}))
            .build()
    }
}

pub struct IntegrationTest {
    router: Option<Child>,
    test_config_location: PathBuf,
    test_schema_location: PathBuf,
    router_location: PathBuf,
    stdio_tx: tokio::sync::mpsc::Sender<String>,
    stdio_rx: tokio::sync::mpsc::Receiver<String>,
    stderr_tx: tokio::sync::mpsc::Sender<String>,
    apollo_otlp_metrics_rx: tokio::sync::mpsc::Receiver<ExportMetricsServiceRequest>,
    collect_stdio: Option<(tokio::sync::oneshot::Sender<String>, regex::Regex)>,
    _subgraphs: wiremock::MockServer,
    _apollo_otlp_server: wiremock::MockServer,
    /// Per-test wiremock that stands in for `uplink.api.apollographql.com`.
    /// The harness wires `APOLLO_UPLINK_ENDPOINTS` to this server's URL
    /// **only in the default-credentials branch** of `start()` —
    /// i.e. when `with_real_studio_creds == false`. The opt-in real-creds
    /// branch leaves `APOLLO_UPLINK_ENDPOINTS` unset so the spawned router
    /// reaches the real `uplink.api.apollographql.com`, and the per-test
    /// mock is left idle (still bound to a loopback ephemeral port; just
    /// not referenced).
    ///
    /// See `mock_license_uplink()` for the matchers. The `LicenseQuery`
    /// matcher returns a `RouterEntitlementsResult` whose `entitlement.jwt`
    /// is `TEST_LICENSE_JWT_FULL_FEATURES` — a real HS256-signed JWT that
    /// the spawned router validates against the test JWKS exposed via
    /// `APOLLO_TEST_INTERNAL_UPLINK_JWKS=TEST_JWKS_ENDPOINT` (also set by
    /// the default-credentials branch). The JWT carries no
    /// `allowedFeatures` claim, which `LicenseLimits::default()`'s
    /// legacy-compat path interprets as "all features allowed," so paid
    /// features (federated subscriptions, coprocessors, OTLP, entity
    /// caching, traffic shaping) are unlocked under the test license.
    _apollo_uplink_server: wiremock::MockServer,
    telemetry: Telemetry,
    extra_propagator: Telemetry,

    pub _tracer_provider_client: SdkTracerProvider,
    pub _tracer_provider_subgraph: SdkTracerProvider,
    subscriber_client: Dispatch,

    _subgraph_overrides: HashMap<String, String>,
    bind_address: Arc<Mutex<Option<SocketAddr>>>,
    redis_namespace: String,
    redis_urls: Option<Vec<String>>,
    log: String,
    subgraph_context: Arc<Mutex<Option<SpanContext>>>,
    logs: Vec<String>,
    port_replacements: HashMap<String, u16>,
    jwt: Option<String>,
    env: Option<HashMap<String, OsString>>,
    hot_reload: bool,
    reqwest_client: reqwest::Client,
    /// When `true`, the harness forwards the host's `TEST_APOLLO_KEY`
    /// and `TEST_APOLLO_GRAPH_REF` (real Studio credentials) into the
    /// spawned router as `APOLLO_KEY` / `APOLLO_GRAPH_REF`, and does
    /// **not** override `APOLLO_UPLINK_ENDPOINTS` or set
    /// `APOLLO_TELEMETRY_DISABLED=true`. So the spawned router will
    /// reach **real Uplink** (`uplink.api.apollographql.com`) and
    /// **real orbiter** (`router.apollo.dev/telemetry`).
    ///
    /// **Note:** Studio reporting (`usage-reporting.api.apollographql.com`)
    /// is NOT reached even in the opt-in branch. `merge_overrides()`
    /// unconditionally pins `telemetry.apollo.endpoint` and
    /// `telemetry.apollo.experimental_otlp_endpoint` in the YAML config
    /// to the per-test `apollo_otlp_server` mock, regardless of this
    /// flag. That pinning is load-bearing for keeping CI off the
    /// public Internet. If a future test genuinely needs real Studio
    /// reporting, that change has to also amend `merge_overrides()`.
    ///
    /// Reserve this for the rare end-to-end test that genuinely needs
    /// to talk to production Apollo's License + Uplink + orbiter; every
    /// other test should accept the default (fake credentials, per-test
    /// mock Uplink, per-test mock Studio reporting) so that the suite
    /// passes on runners with restricted egress.
    with_real_studio_creds: bool,
}

impl IntegrationTest {
    pub(crate) fn bind_address(&self) -> SocketAddr {
        self.bind_address
            .lock()
            .expect("no bind address set, router must be started first.")
    }

    /// Reserve a port for use in the test and return it
    /// The port placeholder will be immediately replaced in the config file
    /// Panics if the placeholder is not found in the config
    /// This helps avoid port conflicts between concurrent tests
    #[allow(dead_code)]
    pub fn reserve_address(&mut self, placeholder_name: &str) -> u16 {
        let port = allocate_port(placeholder_name).expect("Failed to allocate port");
        self.set_address(placeholder_name, port);
        port
    }

    /// Reserve a specific port for use in the test
    /// The port placeholder will be immediately replaced in the config file
    /// Panics if the placeholder is not found in the config
    #[allow(dead_code)]
    pub fn set_address(&mut self, placeholder_name: &str, port: u16) {
        // Read current config
        let current_config = std::fs::read_to_string(&self.test_config_location)
            .expect("Failed to read config file");

        // Check if placeholder exists in config
        let placeholder_pattern = format!("{{{{{placeholder_name}}}}}");
        let port_pattern = format!(":{{{{{placeholder_name}}}}}");
        let addr_pattern = format!("127.0.0.1:{{{{{placeholder_name}}}}}");

        if !current_config.contains(&placeholder_pattern)
            && !current_config.contains(&port_pattern)
            && !current_config.contains(&addr_pattern)
        {
            panic!(
                "Placeholder '{placeholder_name}' not found in config file. Expected one of: '{placeholder_pattern}', '{port_pattern}', or '{addr_pattern}'"
            );
        }

        // Store the replacement
        self.port_replacements
            .insert(placeholder_name.to_string(), port);

        // Apply the replacement immediately
        let updated_config = merge_overrides(
            &current_config,
            &self._subgraph_overrides,
            &self._apollo_otlp_server.uri().to_string(),
            None, // Don't override bind address here
            &self.redis_namespace,
            Some(&self.port_replacements),
        );

        std::fs::write(
            &self.test_config_location,
            serde_yaml::to_string(&updated_config).unwrap(),
        )
        .expect("Failed to write updated config");
    }

    /// Set environment variables for the router subprocess
    #[allow(dead_code)]
    pub fn set_env(&mut self, env: HashMap<String, OsString>) {
        self.env.get_or_insert_with(HashMap::new).extend(env);
    }

    /// Path to the temp file holding this test's supergraph schema. Tests
    /// that need to set `APOLLO_ROUTER_SUPERGRAPH_PATH` directly (e.g. to
    /// pin schema source while still setting `APOLLO_GRAPH_REF` for license
    /// reasons) can read this to construct that env var.
    #[allow(dead_code)]
    pub fn test_schema_location(&self) -> &PathBuf {
        &self.test_schema_location
    }

    /// Address of the per-test Uplink mock. Exposed for the rare test
    /// that wants to assert directly against the mock (eg. inspect the
    /// queries the router posted) or wire its own subprocess at the
    /// same mock. Most tests don't need this — the harness wires
    /// `APOLLO_UPLINK_ENDPOINTS` automatically in `start()` whenever
    /// the test isn't opted into real Studio credentials.
    #[allow(dead_code)]
    pub fn apollo_uplink_endpoint(&self) -> String {
        self._apollo_uplink_server.uri()
    }

    /// Set an address placeholder using a URI, extracting the port automatically
    /// This is a convenience method for the common pattern of extracting port from a server URI
    #[allow(dead_code)]
    pub fn set_address_from_uri(&mut self, placeholder_name: &str, uri: &str) {
        let port = uri
            .split(':')
            .next_back()
            .expect("URI should contain a port")
            .parse::<u16>()
            .expect("Port should be a valid u16");
        self.set_address(placeholder_name, port);
    }

    /// Replace a string in the config file (for non-port replacements)
    /// This is useful for dynamic config adjustments beyond port replacements
    #[allow(dead_code)]
    pub fn replace_config_string(&mut self, from: &str, to: &str) {
        let current_config = std::fs::read_to_string(&self.test_config_location)
            .expect("Failed to read config file");

        let updated_config = current_config.replace(from, to);

        std::fs::write(&self.test_config_location, updated_config)
            .expect("Failed to write updated config");
    }

    /// Replace a string in the config file (for non-port replacements)
    /// This is useful for dynamic config adjustments beyond port replacements
    #[allow(dead_code)]
    pub fn replace_schema_string(&mut self, from: &str, to: &str) {
        let current_schema = std::fs::read_to_string(&self.test_schema_location)
            .expect("Failed to read schema file");

        let updated_schema = current_schema.replace(from, to);

        std::fs::write(&self.test_schema_location, updated_schema)
            .expect("Failed to write updated schema");
    }
}

struct TracedResponder {
    response_template: ResponseTemplate,
    telemetry: Telemetry,
    extra_propagator: Telemetry,
    subscriber_subgraph: Dispatch,
    subgraph_callback: Option<Box<dyn Fn() + Send + Sync>>,
    subgraph_context: Arc<Mutex<Option<SpanContext>>>,
}

impl Respond for TracedResponder {
    fn respond(&self, request: &wiremock::Request) -> ResponseTemplate {
        let context = self.telemetry.extract_context(request, &Context::new());
        let context = self.extra_propagator.extract_context(request, &context);

        *self.subgraph_context.lock() = Some(context.span().span_context().clone());
        tracing_core::dispatcher::with_default(&self.subscriber_subgraph, || {
            let _context_guard = context.attach();
            let span = info_span!("subgraph server");
            let _span_guard = span.enter();
            if let Some(callback) = &self.subgraph_callback {
                callback();
            }
            self.response_template.clone()
        })
    }
}

#[derive(Debug, Clone, Default)]
#[allow(dead_code)]
pub enum Telemetry {
    Otlp {
        endpoint: Option<String>,
    },
    Datadog,
    Zipkin,
    #[default]
    None,
}

impl Telemetry {
    fn tracer_provider(&self, service_name: &str) -> SdkTracerProvider {
        let resource = Resource::builder_empty()
            .with_attributes([KeyValue::new(SERVICE_NAME, service_name.to_string())])
            .build();

        match self {
            Telemetry::Otlp {
                endpoint: Some(endpoint),
            } => SdkTracerProvider::builder()
                .with_resource(resource)
                .with_span_processor(
                    BatchSpanProcessor::builder(
                        opentelemetry_otlp::SpanExporter::builder()
                            .with_http()
                            .with_endpoint(endpoint)
                            .build()
                            .expect("otlp pipeline failed"),
                        runtime::Tokio,
                    )
                    .with_batch_config(
                        BatchConfigBuilder::default()
                            .with_scheduled_delay(Duration::from_millis(10))
                            .build(),
                    )
                    .build(),
                )
                .build(),
            Telemetry::Datadog => SdkTracerProvider::builder()
                .with_resource(resource)
                .with_span_processor(
                    BatchSpanProcessor::builder(
                        opentelemetry_datadog::new_pipeline()
                            .with_service_name(service_name)
                            .build_exporter()
                            .expect("datadog pipeline failed"),
                        runtime::Tokio,
                    )
                    .with_batch_config(
                        BatchConfigBuilder::default()
                            .with_scheduled_delay(Duration::from_millis(10))
                            .build(),
                    )
                    .build(),
                )
                .build(),
            Telemetry::Zipkin => SdkTracerProvider::builder()
                .with_resource(resource)
                .with_span_processor(
                    BatchSpanProcessor::builder(
                        opentelemetry_zipkin::ZipkinExporter::builder()
                            .with_collector_endpoint("http://127.0.0.1:9411/api/v2/spans")
                            .build()
                            .expect("zipkin pipeline failed"),
                        runtime::Tokio,
                    )
                    .with_batch_config(
                        BatchConfigBuilder::default()
                            .with_scheduled_delay(Duration::from_millis(10))
                            .build(),
                    )
                    .build(),
                )
                .build(),
            Telemetry::None | Telemetry::Otlp { endpoint: None } => SdkTracerProvider::builder()
                .with_resource(resource)
                .with_simple_exporter(NoopSpanExporter::default())
                .build(),
        }
    }

    fn inject_context(&self, request: &mut Request) {
        let ctx = tracing::span::Span::current().context();

        match self {
            Telemetry::Datadog => {
                // Get the existing PSR header if it exists. This is because the existing telemetry propagator doesn't support PSR properly yet.
                // In testing we are manually setting the PSR header, and we don't want to override it.
                let psr = request
                    .headers()
                    .get("x-datadog-sampling-priority")
                    .cloned();
                let propagator = opentelemetry_datadog::DatadogPropagator::new();
                propagator.inject_context(
                    &ctx,
                    &mut apollo_router::otel_compat::HeaderInjector(request.headers_mut()),
                );

                if let Some(psr) = psr {
                    request
                        .headers_mut()
                        .insert("x-datadog-sampling-priority", psr);
                }
            }
            Telemetry::Otlp { .. } => {
                let propagator = opentelemetry_sdk::propagation::TraceContextPropagator::default();
                propagator.inject_context(
                    &ctx,
                    &mut apollo_router::otel_compat::HeaderInjector(request.headers_mut()),
                )
            }
            Telemetry::Zipkin => {
                let propagator = opentelemetry_zipkin::Propagator::new();
                propagator.inject_context(
                    &ctx,
                    &mut apollo_router::otel_compat::HeaderInjector(request.headers_mut()),
                )
            }
            _ => {}
        }
    }

    pub(crate) fn extract_context(
        &self,
        request: &wiremock::Request,
        context: &Context,
    ) -> Context {
        let headers: HashMap<String, String> = request
            .headers
            .iter()
            .map(|(name, value)| {
                (
                    name.as_str().to_string(),
                    value
                        .to_str()
                        .expect("non-UTF-8 header value in tests")
                        .to_string(),
                )
            })
            .collect();

        match self {
            Telemetry::Datadog => {
                let span_ref = context.span();
                let original_span_context = span_ref.span_context();
                let propagator = opentelemetry_datadog::DatadogPropagator::new();
                let mut context = propagator.extract_with_context(context, &headers);
                // We're going to override the sampled so that we can test sampling priority
                if let Some(psr) = headers.get("x-datadog-sampling-priority") {
                    let state = context
                        .span()
                        .span_context()
                        .trace_state()
                        .insert("psr", psr.to_string())
                        .expect("psr");
                    let new_trace_id = if original_span_context.is_valid() {
                        original_span_context.trace_id()
                    } else {
                        context.span().span_context().trace_id()
                    };
                    context = context.with_remote_span_context(SpanContext::new(
                        new_trace_id,
                        context.span().span_context().span_id(),
                        context.span().span_context().trace_flags(),
                        true,
                        state,
                    ));
                }

                context
            }
            Telemetry::Otlp { .. } => {
                let propagator = opentelemetry_sdk::propagation::TraceContextPropagator::default();
                propagator.extract_with_context(context, &headers)
            }
            Telemetry::Zipkin => {
                let propagator = opentelemetry_zipkin::Propagator::new();
                propagator.extract_with_context(context, &headers)
            }
            _ => context.clone(),
        }
    }
}

#[buildstructor]
impl IntegrationTest {
    #[builder]
    pub async fn new(
        config: String,
        telemetry: Option<Telemetry>,
        extra_propagator: Option<Telemetry>,
        responder: Option<ResponseTemplate>,
        collect_stdio: Option<tokio::sync::oneshot::Sender<String>>,
        supergraph: Option<PathBuf>,
        mut subgraph_overrides: HashMap<String, String>,
        log: Option<String>,
        subgraph_callback: Option<Box<dyn Fn() + Send + Sync>>,
        http_method: Option<String>,
        jwt: Option<String>,
        env: Option<HashMap<String, OsString>>,
        redis_namespace: Option<String>,
        hot_reload: Option<bool>,
        reqwest_client: Option<reqwest::Client>,
        // Opt-in to forwarding the host's real `TEST_APOLLO_KEY` /
        // `TEST_APOLLO_GRAPH_REF` to the spawned router. Default `false`,
        // which forwards fake credentials and pins
        // `APOLLO_UPLINK_ENDPOINTS` to the per-test
        // `_apollo_uplink_server` mock — the right answer for every test
        // that doesn't need production Apollo to be reachable. Buildstructor
        // surfaces this as `IntegrationTest::builder().with_real_studio_creds(true)`.
        with_real_studio_creds: Option<bool>,
    ) -> Self {
        let redis_namespace = redis_namespace.unwrap_or_else(|| Uuid::new_v4().to_string());
        let telemetry = telemetry.unwrap_or_default();
        let extra_propagator = extra_propagator.unwrap_or_default();
        let tracer_provider_client = telemetry.tracer_provider("client");
        let subscriber_client = Self::dispatch(&tracer_provider_client);
        let tracer_provider_subgraph = telemetry.tracer_provider("subgraph");

        let listener = TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0))).unwrap();
        let address = listener.local_addr().unwrap();
        let url = format!("http://{address}/");

        let apollo_otlp_listener =
            TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0))).unwrap();
        let apollo_otlp_address = apollo_otlp_listener.local_addr().unwrap();
        let apollo_otlp_endpoint = format!("http://{apollo_otlp_address}");

        // Add a default override for products, if not specified
        subgraph_overrides
            .entry("products".into())
            .or_insert(url.clone());

        // Add a default override for jsonPlaceholder (connectors), if not specified
        subgraph_overrides
            .entry("jsonPlaceholder".into())
            .or_insert(url.clone());

        // Insert the overrides into the config
        let config = merge_overrides(
            &config,
            &subgraph_overrides,
            &apollo_otlp_endpoint,
            None,
            &redis_namespace,
            None,
        );

        // pull the redis urls from the config
        let redis_urls = get_redis_urls(&config);

        let supergraph = supergraph.unwrap_or(PathBuf::from_iter([
            "..",
            "examples",
            "graphql",
            "local.graphql",
        ]));
        let subgraphs = wiremock::MockServer::builder()
            .listener(listener)
            .start()
            .await;

        // Allow for GET or POST so that connectors works
        let http_method = match http_method.unwrap_or("POST".to_string()).as_str() {
            "GET" => Method::GET,
            "POST" => Method::POST,
            _ => panic!("Unknown http method specified"),
        };
        let subgraph_context = Arc::new(Mutex::new(None));
        Mock::given(method(http_method))
            .and(path_regex(".*")) // Match any path so that connectors functions
            .respond_with(TracedResponder {
                response_template: responder.unwrap_or_else(|| {
                    ResponseTemplate::new(200).set_body_json(json!({
                        "data": {
                            "topProducts": [
                                { "name": "Table" },
                                { "name": "Couch" },
                                { "name": "Chair" },
                            ],
                        },
                    }))
                }),
                telemetry: telemetry.clone(),
                extra_propagator: extra_propagator.clone(),
                subscriber_subgraph: Self::dispatch(&tracer_provider_subgraph),
                subgraph_callback,
                subgraph_context: subgraph_context.clone(),
            })
            .mount(&subgraphs)
            .await;

        let mut test_config_location = std::env::temp_dir();
        let mut test_schema_location = test_config_location.clone();
        let location = format!("apollo-router-test-{}.yaml", Uuid::new_v4());
        test_config_location.push(location);
        test_schema_location.push(format!("apollo-router-test-{}.graphql", Uuid::new_v4()));

        fs::write(
            &test_config_location,
            serde_yaml::to_string(&config).unwrap(),
        )
        .expect("could not write config");
        fs::copy(&supergraph, &test_schema_location).expect("could not write schema");

        let (stdio_tx, stdio_rx) = tokio::sync::mpsc::channel(2000);
        // we separate stderr and stdio (previously they were both just handled by a single
        // channel) to avoid congestion in one to contend the other

        let (stderr_tx, mut stderr_rx) = tokio::sync::mpsc::channel::<String>(2000);
        // we want to continually drain stderr, not let it build up backpressure
        task::spawn(async move {
            while stderr_rx.recv().await.is_some() {
                // we discard stderr to prevent backpressure
            }
        });
        let collect_stdio = collect_stdio.map(|sender| {
            let version_line_re = regex::Regex::new("Apollo Router v[^ ]+ ").unwrap();
            (sender, version_line_re)
        });

        let (apollo_otlp_metrics_tx, apollo_otlp_metrics_rx) = tokio::sync::mpsc::channel(100);
        let apollo_otlp_server = wiremock::MockServer::builder()
            .listener(apollo_otlp_listener)
            .start()
            .await;
        Mock::given(method(Method::POST))
            .and(path("/v1/metrics"))
            .and(move |req: &wiremock::Request| {
                // Decompress gzip body if Content-Encoding indicates it, then decode as protobuf
                let body: &[u8] = req.body.as_ref();
                let is_gzip = req
                    .headers
                    .get("content-encoding")
                    .and_then(|v| v.to_str().ok())
                    .map(|v| v.contains("gzip"))
                    .unwrap_or(false);
                let decoded = if is_gzip {
                    let mut decoder = GzDecoder::new(body);
                    let mut buf = Vec::new();
                    std::io::Read::read_to_end(&mut decoder, &mut buf)
                        .ok()
                        .map(|_| buf)
                } else {
                    Some(body.to_vec())
                };
                if let Some(bytes) = decoded
                    && let Ok(msg) = ExportMetricsServiceRequest::decode(bytes.as_slice())
                {
                    let _ = apollo_otlp_metrics_tx.try_send(msg);
                }
                // Always match so we return 200 OK
                true
            })
            .respond_with(ResponseTemplate::new(200))
            .mount(&apollo_otlp_server)
            .await;

        // Catch-all fallback so that other Apollo Studio reporting paths
        // (eg. `/v1/traces` for OTLP traces, `/api/ingress/traces` for the
        // Apollo-protocol exporter) return 200 instead of 404. Without this,
        // any test whose router is wired up to send Studio telemetry would
        // silently fail every report after the first non-`/v1/metrics`
        // request, and the corresponding `apollo_router_telemetry_studio_
        // reports_total` counter would never increment. Lower priority
        // (higher number) than the default 5 so the body-capturing
        // `/v1/metrics` route above still wins for that path.
        Mock::given(method(Method::POST))
            .respond_with(ResponseTemplate::new(200))
            .with_priority(10)
            .mount(&apollo_otlp_server)
            .await;

        // Per-test Uplink mock. Stood up unconditionally so it's
        // available regardless of whether the test ends up opting into
        // real Studio creds — an unused MockServer is essentially free
        // (a tokio-backed listener bound to a loopback ephemeral port,
        // dropped when `IntegrationTest` drops).
        let apollo_uplink_server = mock_license_uplink().await;

        Self {
            router: None,
            router_location: Self::router_location(),
            test_config_location,
            test_schema_location,
            stdio_tx,
            stdio_rx,
            stderr_tx,
            apollo_otlp_metrics_rx,
            collect_stdio,
            _subgraphs: subgraphs,
            _subgraph_overrides: subgraph_overrides,
            _apollo_otlp_server: apollo_otlp_server,
            _apollo_uplink_server: apollo_uplink_server,
            bind_address: Default::default(),
            _tracer_provider_client: tracer_provider_client,
            subscriber_client,
            _tracer_provider_subgraph: tracer_provider_subgraph,
            telemetry,
            extra_propagator,
            redis_namespace,
            redis_urls,
            log: log.unwrap_or_else(|| "error,apollo_router=info".to_owned()),
            subgraph_context,
            logs: vec![],
            port_replacements: HashMap::new(),
            jwt,
            env,
            hot_reload: hot_reload.unwrap_or(true),
            reqwest_client: reqwest_client.unwrap_or_default(),
            with_real_studio_creds: with_real_studio_creds.unwrap_or(false),
        }
    }

    fn dispatch(tracer_provider: &SdkTracerProvider) -> Dispatch {
        let tracer = tracer_provider.tracer("tracer");
        let tracing_layer = tracing_opentelemetry::layer()
            .with_tracer(tracer)
            .with_filter(LevelFilter::INFO);

        let subscriber = Registry::default().with(tracing_layer).with(
            tracing_subscriber::fmt::Layer::default()
                .compact()
                .with_filter(EnvFilter::from_default_env()),
        );
        Dispatch::new(subscriber)
    }

    #[allow(dead_code)]
    pub fn subgraph_context(&self) -> SpanContext {
        self.subgraph_context.lock().as_ref().unwrap().clone()
    }

    pub fn router_location() -> PathBuf {
        PathBuf::from(env!("CARGO_BIN_EXE_router"))
    }

    #[allow(dead_code)]
    pub async fn start(&mut self) {
        let mut router = Command::new(&self.router_location);

        let mut needs_supergraph_cli_arg = true;
        let non_file_startup_env = &[
            "APOLLO_ROUTER_SUPERGRAPH_PATH",
            "APOLLO_ROUTER_SUPERGRAPH_URLS",
            "APOLLO_GRAPH_ARTIFACT_REFERENCE",
            "APOLLO_GRAPH_REF",
        ];

        // Harness defaults. Forward fake Studio credentials so the
        // spawned router activates
        // `LicenseSource::Registry` (and therefore exercises the
        // license-stream code path that ships in production), and pin
        // `APOLLO_UPLINK_ENDPOINTS` to the per-test mock so neither the
        // license poller nor a `SchemaSource::Registry` activation can
        // reach `uplink.api.apollographql.com`. Skipped when the test
        // opted into real Studio credentials via
        // `IntegrationTest::builder().with_real_studio_creds(true)`,
        // which is reserved for the rare end-to-end test that genuinely
        // needs to talk to production Apollo.
        //
        // We set these via `router.env()` (not `self.env`) so they don't
        // flip `needs_supergraph_cli_arg` to `false` — the harness's
        // `--supergraph` CLI arg should still win for schema source.
        // Tests that intentionally exercise `SchemaSource::Registry`
        // put `APOLLO_GRAPH_REF` (or one of the other
        // `non_file_startup_env` keys) into `self.env`, where the loop
        // below picks them up and suppresses the CLI arg.
        if !self.with_real_studio_creds {
            router.env("APOLLO_KEY", "test-mocked-key");
            router.env("APOLLO_GRAPH_REF", "test-mocked-graph@current");
            router.env("APOLLO_UPLINK_ENDPOINTS", self._apollo_uplink_server.uri());
            // Point the spawned router's `License::jwks()` lookup at the
            // test JWKS (HS256 secret bundled in
            // `apollo-router/src/uplink/testdata/license.jwks.json`) so it
            // can validate the JWT that `mock_license_uplink()` returns
            // for `LicenseQuery`. Without this, the router would fall
            // back to the production JWKS baked into the binary, fail
            // the signature check, and reject the test license — paid
            // features (federated subscriptions, coprocessors, OTLP,
            // entity caching, traffic shaping, ...) would then be
            // gated off.
            //
            // Tests that explicitly set `.jwt(...)` on the builder
            // also rely on this — every test JWT in
            // `tests/integration/allowed_features.rs` is signed with
            // the same secret. Setting it unconditionally in the
            // default branch means individual tests no longer need to
            // remember to inject it themselves.
            router.env(
                "APOLLO_TEST_INTERNAL_UPLINK_JWKS",
                TEST_JWKS_ENDPOINT.as_os_str(),
            );
            // Strip any inherited license-source env vars so a developer
            // who happens to have a real production-signed
            // `APOLLO_ROUTER_LICENSE` exported in their shell doesn't
            // see `LicenseSource::Env` win, fail signature verification
            // against the test HS256 JWKS we just pinned above, and
            // then hang in `Startup` because the license stream emits
            // no `UpdateLicense` event on parse error. Tests that need a
            // license reach through `.jwt(...)` (which sets
            // `APOLLO_ROUTER_LICENSE` after this strip), or via the
            // mocked Uplink response below.
            router.env_remove("APOLLO_ROUTER_LICENSE");
            router.env_remove("APOLLO_ROUTER_LICENSE_PATH");
            // Disable the "orbiter" anonymous-usage telemetry, which
            // otherwise POSTs to `https://router.apollo.dev/telemetry`
            // unconditionally on every router boot — independent of
            // `APOLLO_KEY` and entirely separate from Uplink. Without
            // this, an integration test on a runner with restricted
            // egress to `*.apollo.dev` would see a 1× outbound HTTPS
            // request per spawned router (the orbiter is fire-and-forget
            // so the test wouldn't *fail*, but the connection attempt
            // would still leak — defeating the egress-block invariant
            // the harness establishes by default).
            router.env("APOLLO_TELEMETRY_DISABLED", "true");
        }

        // Any env vars set via the env argument should be passed along
        // as-is. These run *after* the harness defaults so an
        // explicit `.env(...)` builder call wins over the defaults
        // (eg. for a test that wants to override `APOLLO_UPLINK_ENDPOINTS`
        // to its own custom mock).
        if let Some(env) = &self.env {
            for (key, val) in env {
                // If env vars are used to configure which schema to load, do not
                // override later with the --supergraph cli arg
                if non_file_startup_env.iter().any(|x| x == key) {
                    needs_supergraph_cli_arg = false;
                }
                router.env(key, val);
            }
        }

        // Opt-in real Studio credentials. Reads `TEST_APOLLO_KEY` and
        // `TEST_APOLLO_GRAPH_REF` from the host environment (CI sets
        // these on machines that have access to a real Studio
        // account) and forwards them as `APOLLO_KEY` /
        // `APOLLO_GRAPH_REF`. `APOLLO_UPLINK_ENDPOINTS` is
        // intentionally NOT overridden, so the spawned router talks to
        // the real `uplink.api.apollographql.com` and the per-test
        // Uplink mock is left idle.
        //
        // Pre-2026-05 the harness forwarded these creds unconditionally
        // whenever `TEST_APOLLO_KEY` was present in the host env, which
        // turned every CircleCI run of the integration suite into a
        // real-Studio-traffic exercise — a credential gate (whether the
        // test runs at all) that doubled as a network gate (whether the
        // router talks to production Apollo). This branch makes that
        // coupling opt-in.
        if self.with_real_studio_creds {
            if let Ok(apollo_key) = std::env::var("TEST_APOLLO_KEY") {
                router.env("APOLLO_KEY", apollo_key);
            }
            if let Ok(apollo_graph_ref) = std::env::var("TEST_APOLLO_GRAPH_REF") {
                router.env("APOLLO_GRAPH_REF", apollo_graph_ref);
            }
        }

        if let Some(jwt) = &self.jwt {
            router.env("APOLLO_ROUTER_LICENSE", jwt);
        }

        // Build arguments conditionally based on APOLLO_GRAPH_ARTIFACT_REGISTRY
        let config_path = self.test_config_location.to_string_lossy();
        let mut args = vec!["--config", &config_path, "--log", &self.log];
        if self.hot_reload {
            args.insert(0, "--hr");
        }

        // Add --supergraph if launch env vars are not set
        let schema_path = self.test_schema_location.to_string_lossy();
        if needs_supergraph_cli_arg {
            tracing::info!("Loading supergraph from file");
            args.push("--supergraph");
            args.push(&schema_path);
        }

        router
            .args(args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut router = router.spawn().expect("router should start");
        let reader = BufReader::new(router.stdout.take().expect("out"));
        let stderr_reader = BufReader::new(router.stderr.take().expect("err"));
        let stdio_tx = self.stdio_tx.clone();
        let collect_stdio = self.collect_stdio.take();
        let bind_address = self.bind_address.clone();
        let bind_address_regex =
            Regex::new(r".*GraphQL endpoint exposed at http://(?<address>[^/]+).*").unwrap();
        // We need to read from stdout otherwise we will hang
        task::spawn(async move {
            let mut collected = Vec::new();
            let mut lines = reader.lines();
            while let Ok(Some(line)) = lines.next_line().await {
                // Extract the bind address from a log line that looks like this: GraphQL endpoint exposed at http://127.0.0.1:51087/
                if let Some(captures) = bind_address_regex.captures(&line) {
                    let address = captures.name("address").unwrap().as_str();
                    let mut bind_address = bind_address.lock();
                    *bind_address = Some(address.parse().unwrap());
                }

                if let Some((_sender, version_line_re)) = &collect_stdio {
                    #[derive(serde::Deserialize)]
                    struct Log {
                        #[allow(unused)]
                        timestamp: String,
                        level: String,
                        message: String,
                    }
                    let Ok(log) = serde_json::from_str::<Log>(&line) else {
                        panic!(
                            "line: '{line}' isn't JSON, might you have some debug output in the logging?"
                        );
                    };
                    // Omit this message from snapshots since it depends on external environment
                    if !log.message.starts_with("RUST_BACKTRACE=full detected") {
                        collected.push(format!(
                            "{}: {}",
                            log.level,
                            // Redacted so we don't need to update snapshots every release
                            version_line_re
                                .replace(&log.message, "Apollo Router [version number] ")
                        ))
                    }
                }
                let _ = stdio_tx.send(line).await;
            }
            if let Some((sender, _version_line_re)) = collect_stdio {
                let _ = sender.send(collected.join("\n"));
            }
        });

        // we separate out stderr to avoid congestion there affecting stdout; previous to this, we
        // had both stdout and stderr in the same channel, allowing one's congestion to swamp the other
        let stderr_tx = self.stderr_tx.clone();
        task::spawn(async move {
            let mut lines = stderr_reader.lines();
            while let Ok(Some(line)) = lines.next_line().await {
                // try_send to never block - if the channel is full, just drop the line
                let _ = stderr_tx.try_send(line);
            }
        });
        self.router = Some(router);
    }

    #[allow(dead_code)]
    pub async fn assert_started(&mut self) {
        self.wait_for_log_message("GraphQL endpoint exposed").await;
    }

    #[allow(dead_code)]
    pub async fn assert_not_started(&mut self) {
        self.wait_for_log_message("no valid configuration").await;
    }

    #[allow(dead_code)]
    pub async fn touch_config(&self) {
        let mut f = tokio::fs::OpenOptions::new()
            .append(true)
            .open(&self.test_config_location)
            .await
            .expect("must have been able to open config file");
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time before UNIX epoch")
            .as_nanos();
        f.write_all(format!("\n#touched-{stamp}\n").as_bytes())
            .await
            .expect("must be able to write config file");
    }

    #[allow(dead_code)]
    pub async fn update_config(&self, yaml: &str) {
        let config = merge_overrides(
            yaml,
            &self._subgraph_overrides,
            &self._apollo_otlp_server.uri().to_string(),
            None,
            &self.redis_namespace,
            Some(&self.port_replacements),
        );
        let mut content = serde_yaml::to_string(&config).unwrap();
        // Append a unique comment so file content always changes. PollWatcher uses
        // with_compare_contents(true); identical rewrites may not emit Data events on Windows.
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time before UNIX epoch")
            .as_nanos();
        content.push_str(&format!("\n# update-{stamp}\n"));
        tokio::fs::write(&self.test_config_location, content)
            .await
            .expect("must be able to write config");
    }

    #[allow(dead_code)]
    pub fn update_subgraph_overrides(&mut self, overrides: HashMap<String, String>) {
        self._subgraph_overrides = overrides;
    }

    #[allow(dead_code)]
    pub async fn update_schema(&self, supergraph_path: &PathBuf) {
        fs::copy(supergraph_path, &self.test_schema_location).expect("could not write schema");
    }

    #[allow(dead_code)]
    pub fn execute_default_query(
        &self,
    ) -> impl std::future::Future<Output = (TraceId, reqwest::Response)> + use<> {
        self.execute_query(Query::builder().build())
    }

    #[allow(dead_code)]
    pub async fn execute_several_default_queries(
        &self,
        times: usize,
    ) -> Vec<(TraceId, reqwest::Response)> {
        let mut results = Vec::with_capacity(3 * times);
        for _ in 0..times {
            results.push(self.execute_query(Query::default()).await);
            results.push(self.execute_query(Query::default().with_anonymous()).await);
            results.push(
                self.execute_query(Query::default().with_invalid_query())
                    .await,
            );
        }
        results
    }

    #[allow(dead_code)]
    pub fn execute_query(
        &self,
        query: Query,
    ) -> impl std::future::Future<Output = (TraceId, reqwest::Response)> + use<> {
        assert!(
            self.router.is_some(),
            "router was not started, call `router.start().await; router.assert_started().await`"
        );
        let telemetry = self.telemetry.clone();
        let extra_propagator = self.extra_propagator.clone();

        let url = format!("http://{}", self.bind_address());
        let subgraph_context = self.subgraph_context.clone();

        let client = self.reqwest_client.clone();

        async move {
            let span = info_span!("client_request");
            let trace_id = span.context().span().span_context().trace_id();
            async move {
                let mut builder = client.post(url).header(CONTENT_TYPE, query.content_type);

                for (name, value) in query.headers {
                    builder = builder.header(name, value);
                }

                if let Some(psr) = query.psr {
                    builder = builder.header("x-datadog-sampling-priority", psr);
                }

                let mut request = builder.json(&query.body).build().unwrap();
                if query.traced {
                    telemetry.inject_context(&mut request);
                    extra_propagator.inject_context(&mut request);
                }

                match client.execute(request).await {
                    Ok(response) => {
                        if query.traced {
                            (trace_id, response)
                        } else {
                            (
                                subgraph_context
                                    .lock()
                                    .as_ref()
                                    .expect("subgraph context")
                                    .trace_id(),
                                response,
                            )
                        }
                    }
                    Err(err) => {
                        panic!("unable to send successful request to router, {err}")
                    }
                }
            }
            .instrument(span)
            .await
        }
        .with_subscriber(self.subscriber_client.clone())
    }

    /// Make a raw multipart request to the router.
    #[allow(dead_code)]
    pub fn execute_multipart_request(
        &self,
        request: reqwest::multipart::Form,
        transform: Option<fn(reqwest::Request) -> reqwest::Request>,
    ) -> impl std::future::Future<Output = (String, reqwest::Response)> + use<> {
        assert!(
            self.router.is_some(),
            "router was not started, call `router.start().await; router.assert_started().await`"
        );

        let url = format!("http://{}", self.bind_address());
        let client = self.reqwest_client.clone();

        async move {
            let span = info_span!("client_raw_request");
            let span_id = span.context().span().span_context().trace_id().to_string();

            async move {
                let mut request = client
                    .post(url)
                    .header("apollographql-client-name", "custom_name")
                    .header("apollographql-client-version", "1.0")
                    .header("apollo-require-preflight", "test")
                    .multipart(request)
                    .build()
                    .unwrap();

                // Optionally transform the request if needed
                let transformer = transform.unwrap_or(core::convert::identity);

                global::get_text_map_propagator(|propagator| {
                    propagator.inject_context(
                        &tracing::span::Span::current().context(),
                        &mut apollo_router::otel_compat::HeaderInjector(request.headers_mut()),
                    );
                });
                request.headers_mut().remove(ACCEPT);
                match client.execute(transformer(request)).await {
                    Ok(response) => (span_id, response),
                    Err(err) => {
                        panic!("unable to send successful request to router, {err}")
                    }
                }
            }
            .instrument(span)
            .await
        }
        .with_subscriber(self.subscriber_client.clone())
    }

    #[allow(dead_code)]
    pub async fn run_subscription(&self, subscription: &str) -> (String, reqwest::Response) {
        assert!(
            self.router.is_some(),
            "router was not started, call `router.start().await; router.assert_started().await`"
        );
        let id = Uuid::new_v4().to_string();
        let span = info_span!("client_request", unit_test = id.as_str());
        let _span_guard = span.enter();

        let mut request = self
            .reqwest_client
            .post(format!("http://{}", self.bind_address()))
            .header(CONTENT_TYPE, APPLICATION_JSON.essence_str())
            .header(ACCEPT, "multipart/mixed;subscriptionSpec=1.0")
            .header("apollographql-client-name", "custom_name")
            .header("apollographql-client-version", "1.0")
            .json(&json!({"query":subscription,"variables":{}}))
            .build()
            .unwrap();

        global::get_text_map_propagator(|propagator| {
            propagator.inject_context(
                &span.context(),
                &mut apollo_router::otel_compat::HeaderInjector(request.headers_mut()),
            );
        });

        match self.reqwest_client.execute(request).await {
            Ok(response) => (id, response),
            Err(err) => {
                panic!("unable to send successful request to router, {err}")
            }
        }
    }

    #[allow(dead_code)]
    pub async fn get_metrics_response(&self) -> reqwest::Result<reqwest::Response> {
        let request = self
            .reqwest_client
            .get(format!("http://{}/metrics", self.bind_address()))
            .header("apollographql-client-name", "custom_name")
            .header("apollographql-client-version", "1.0")
            .build()
            .unwrap();

        self.reqwest_client.execute(request).await
    }

    /// Waits for any metrics to be emitted for the given duration. This will return as soon as the
    /// first batch of metrics is received.
    #[allow(dead_code)]
    pub async fn wait_for_emitted_otel_metrics(
        &mut self,
        duration: Duration,
    ) -> Vec<ExportMetricsServiceRequest> {
        let deadline = Instant::now() + duration;
        let mut metrics = Vec::new();

        while Instant::now() < deadline {
            let remaining = deadline.saturating_duration_since(Instant::now());
            match tokio::time::timeout(remaining, self.apollo_otlp_metrics_rx.recv()).await {
                Ok(Some(msg)) => {
                    // Only break once we see a batch with metrics in it
                    if msg
                        .resource_metrics
                        .iter()
                        .any(|rm| !rm.scope_metrics.is_empty())
                    {
                        metrics.push(msg);
                        break;
                    }
                }
                Ok(None) => {
                    // channel closed
                    break;
                }
                Err(_) => {
                    // timeout elapsed
                    break;
                }
            }
        }

        metrics
    }

    #[allow(dead_code)]
    #[cfg(target_family = "unix")]
    pub async fn graceful_shutdown(&mut self) {
        // Send a sig term and then wait for the process to finish.
        unsafe {
            libc::kill(self.pid(), libc::SIGTERM);
        }
        self.assert_shutdown().await;
    }

    #[cfg(target_os = "windows")]
    pub async fn graceful_shutdown(&mut self) {
        // We don’t have SIGTERM on Windows, so do a non-graceful kill instead
        self.kill().await
    }

    #[allow(dead_code)]
    pub async fn kill(&mut self) {
        let _ = self
            .router
            .as_mut()
            .expect("router not started")
            .kill()
            .await;
        self.assert_shutdown().await;
    }

    #[allow(dead_code)]
    pub(crate) fn pid(&self) -> i32 {
        self.router
            .as_ref()
            .expect("router must have been started")
            .id()
            .expect("id expected") as i32
    }

    #[allow(dead_code)]
    pub async fn assert_reloaded(&mut self) {
        self.wait_for_log_message("reload complete").await;
    }

    #[allow(dead_code)]
    pub async fn assert_no_reload_necessary(&mut self) {
        self.wait_for_log_message("no reload necessary").await;
    }

    #[allow(dead_code)]
    pub async fn assert_not_reloaded(&mut self) {
        self.wait_for_log_message("still running with previous configuration")
            .await;
    }

    #[allow(dead_code)]
    pub async fn wait_for_log_message(&mut self, msg: &str) {
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            while let Ok(line) = self.stdio_rx.try_recv() {
                self.logs.push(line.clone());
                if line.contains(msg) {
                    return;
                }
            }
            if Instant::now() >= deadline {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        self.dump_stack_traces();
        panic!(
            "'{msg}' not detected in logs. Log dump below:\n\n{logs}",
            logs = self.logs.join("\n")
        );
    }

    /// Sync fn using a loop to println!() each log
    #[allow(dead_code)]
    pub fn print_logs(&self) {
        for line in &self.logs {
            println!("{line}");
        }
    }

    #[allow(dead_code)]
    pub fn read_logs(&mut self) {
        while let Ok(line) = self.stdio_rx.try_recv() {
            self.logs.push(line);
        }
    }

    #[allow(dead_code)]
    pub fn capture_logs<T>(&mut self, try_match_line: impl Fn(String) -> Option<T>) -> Vec<T> {
        let mut logs = Vec::new();
        while let Ok(line) = self.stdio_rx.try_recv() {
            if let Some(log) = try_match_line(line) {
                logs.push(log);
            }
        }
        logs
    }

    #[allow(dead_code)]
    pub fn assert_log_contained(&self, msg: &str) {
        for line in &self.logs {
            if line.contains(msg) {
                return;
            }
        }

        panic!(
            "'{msg}' not detected in logs. Log dump below:\n\n{logs}",
            logs = self.logs.join("\n")
        );
    }

    #[allow(dead_code)]
    pub async fn assert_log_not_contains(&mut self, msg: &str) {
        let now = Instant::now();
        while now.elapsed() < Duration::from_secs(5) {
            if let Ok(line) = self.stdio_rx.try_recv()
                && line.contains(msg)
            {
                self.dump_stack_traces();
                panic!(
                    "'{msg}' detected in logs. Log dump below:\n\n{logs}",
                    logs = self.logs.join("\n")
                );
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    #[allow(dead_code)]
    pub fn assert_log_not_contained(&self, msg: &str) {
        for line in &self.logs {
            if line.contains(msg) {
                panic!(
                    "'{msg}' detected in logs. Log dump below:\n\n{logs}",
                    logs = self.logs.join("\n")
                );
            }
        }
    }

    #[allow(dead_code)]
    pub fn error_logs(&mut self) -> Vec<String> {
        // Read any remaining logs from buffer
        self.read_logs();

        const JSON_ERROR_INDICATORS: [&str; 3] = ["\"level\":\"ERROR\"", "panic", "PANIC"];

        let mut error_logs = Vec::new();
        for line in &self.logs {
            if JSON_ERROR_INDICATORS.iter().any(|err| line.contains(err))
                || (line.contains("ERROR") && !line.contains("level"))
            {
                error_logs.push(line.clone());
            }
        }
        error_logs
    }
    #[allow(dead_code)]
    pub async fn assert_error_log_contained(&mut self, msg: &str) {
        let now = Instant::now();
        let mut found_error_message = false;
        while now.elapsed() < Duration::from_secs(10) {
            let error_logs = self.error_logs();
            for line in error_logs.into_iter() {
                if line.contains(msg) {
                    found_error_message = true;
                    break;
                }
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        if !found_error_message {
            panic!(
                "Did not find expected error in router logs:\n\n{}\n\nFull log dump:\n\n{}",
                self.error_logs().join("\n"),
                self.logs.join("\n")
            );
        }
    }
    #[allow(dead_code)]
    pub fn assert_no_error_logs(&mut self) {
        let error_logs = self.error_logs();
        if !error_logs.is_empty() {
            panic!(
                "Found {} unexpected error(s) in router logs:\n\n{}\n\nFull log dump:\n\n{}",
                error_logs.len(),
                error_logs.join("\n"),
                self.logs.join("\n")
            );
        }
    }
    #[allow(dead_code)]
    pub fn assert_no_error_logs_with_exceptions(&mut self, exceptions: &[&str]) {
        let mut error_logs = self.error_logs();

        // remove any logs that contain our exceptions
        error_logs.retain(|line| !exceptions.iter().any(|exception| line.contains(exception)));
        if !error_logs.is_empty() {
            panic!(
                "Found {} unexpected error(s) in router logs (excluding {} exceptions):\n\n{}\n\nFull log dump:\n\n{}",
                error_logs.len(),
                exceptions.len(),
                error_logs.join("\n"),
                self.logs.join("\n")
            );
        }
    }

    /// Reads metrics from the live endpoint via `IntegrationTest::get_metrics_response` and prints
    /// them out line by line.
    ///
    /// Useful for debugging.
    #[allow(unused)]
    pub async fn print_metrics(&self) {
        if let Ok(metrics) = self
            .get_metrics_response()
            .await
            .expect("failed to fetch metrics")
            .text()
            .await
        {
            for line in metrics.split("\n") {
                println!("{line}");
            }
        }
    }

    #[allow(dead_code)]
    /// Checks the metrics contain the supplied string in prometheus format.
    /// To allow checking of metrics where the value is not stable the magic tag `<any>` can be used.
    /// For example:
    /// ```rust,ignore
    /// router.assert_metrics_contains(r#"apollo_router_pipelines{config_hash="<any>",schema_id="<any>",otel_scope_name="apollo/router"} 1"#, None)
    /// ```
    /// Will allow the metric to be checked even if the config hash and schema id are fluid.
    pub async fn assert_metrics_contains(&self, text: &str, duration: Option<Duration>) {
        let now = Instant::now();
        let mut last_metrics = String::new();
        let text = regex::escape(text).replace("<any>", ".+");
        let re = Regex::new(&format!("(?m)^{text}")).expect("Invalid regex");
        while now.elapsed() < duration.unwrap_or_else(|| Duration::from_secs(15)) {
            if let Ok(metrics) = self
                .get_metrics_response()
                .await
                .expect("failed to fetch metrics")
                .text()
                .await
            {
                if re.is_match(&metrics) {
                    return;
                }
                last_metrics = metrics;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("'{text}' not detected in metrics\n{last_metrics}");
    }

    /// Polls the Prometheus endpoint until every pattern in `texts` is found
    /// somewhere in the metrics output, or `duration` elapses.
    ///
    /// Each pattern is treated as a substring with two pieces of regex sugar:
    /// `<any>` is replaced with `.+` (one-or-more) and `<anyopt>` with `.*`
    /// (zero-or-more). The match is *not* line-anchored — the pattern can
    /// appear anywhere on a line. Prometheus re-orders labels
    /// alphabetically by name and the set of labels on a given metric grows
    /// over time as new dimensions are added, so anchoring to start-of-line
    /// forces every caller to know the full label list and its current
    /// ordering. Substring semantics let callers match the labels they care
    /// about without coupling to label-set evolution.
    ///
    /// Use `<anyopt>` (not `<any>`) when wildcarding *between* an opening
    /// label brace and a label you care about, because the label you care
    /// about may itself be the alphabetically-first label, in which case
    /// `<any>`'s `.+` would require a phantom character that isn't there.
    ///
    /// `.` in Rust regex does not match `\n` by default, so each pattern
    /// still has to fit on a single line of the Prometheus output — neither
    /// wildcard will silently absorb a newline.
    #[allow(dead_code)]
    pub async fn assert_metrics_contains_multiple(
        &self,
        texts: Vec<&str>,
        duration: Option<Duration>,
    ) {
        let patterns: Vec<(String, Regex)> = texts
            .into_iter()
            .map(|t| {
                let escaped = regex::escape(t)
                    .replace("<anyopt>", ".*")
                    .replace("<any>", ".+");
                let re = Regex::new(&escaped).expect("Invalid regex");
                (t.to_string(), re)
            })
            .collect();
        let now = Instant::now();
        let mut last_metrics = String::new();
        let mut remaining: Vec<&(String, Regex)> = patterns.iter().collect();
        while now.elapsed() < duration.unwrap_or_else(|| Duration::from_secs(15)) {
            if let Ok(metrics) = self
                .get_metrics_response()
                .await
                .expect("failed to fetch metrics")
                .text()
                .await
            {
                remaining.retain(|(_, re)| !re.is_match(&metrics));
                if remaining.is_empty() {
                    return;
                }
                last_metrics = metrics;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        let missing: Vec<&str> = remaining.iter().map(|(t, _)| t.as_str()).collect();
        panic!("'{missing:?}' not detected in metrics\n{last_metrics}");
    }

    #[allow(dead_code)]
    pub async fn assert_metrics_does_not_contain(&self, text: &str) {
        if let Ok(metrics) = self
            .get_metrics_response()
            .await
            .expect("failed to fetch metrics")
            .text()
            .await
            && metrics.contains(text)
        {
            panic!("'{text}' detected in metrics\n{metrics}");
        }
    }

    /// Assert that a metric is present and equal to zero.
    #[allow(dead_code)]
    pub async fn assert_metric_zero(&self, text: &str, duration: Option<Duration>) {
        let now = Instant::now();
        let mut last_metrics = String::new();

        let pattern = regex::escape(text);
        let pattern_exists = Regex::new(&format!("(?m)^{pattern}")).expect("Invalid regex");
        let matches_zero_re =
            Regex::new(&format!("(?m)^{}\\s+0(\\s|$)", pattern)).expect("Invalid regex");

        while now.elapsed() < duration.unwrap_or_else(|| Duration::from_secs(15)) {
            if let Ok(metrics) = self
                .get_metrics_response()
                .await
                .expect("failed to fetch metrics")
                .text()
                .await
            {
                // metric exists and matches zero
                if pattern_exists.is_match(&metrics) && matches_zero_re.is_match(&metrics) {
                    return;
                }
                last_metrics = metrics;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }

        if pattern_exists.is_match(&last_metrics) {
            panic!("'{text}' detected in metrics but was non-zero\n{last_metrics}");
        } else {
            panic!("'{text}' not detected in metrics\n{last_metrics}");
        }
    }

    /// Assert that some metric is non-zero. Useful for those metrics that are non-zero but whose
    /// values might change across integration test runs.
    ///
    /// example use: `.assert_metric_non_zero("some_metric_name{label="example"}", None)`
    ///
    /// Note: make sure you strip off the value at the end or you'll potentially get false
    /// negatives
    #[allow(dead_code)]
    pub async fn assert_metric_non_zero(&self, text: &str, duration: Option<Duration>) {
        let now = Instant::now();
        let mut last_metrics = String::new();

        let pattern = regex::escape(text);
        let pattern = format!(
            // disjunction between two patterns: the first (before the `|`) says to look for a value
            // starting with a digit between 1-9, matching however many, optionally with a decimal; the
            // second pattern matches values starting with 0 and then a decimal (both required), at least
            // on non-zero digit, and then however many (if any) other digits
            "(?m)^{}\\s+([1-9]\\d*(\\.\\d+)?|0\\.[0-9]*[1-9][0-9]*)",
            pattern
        );
        let re = Regex::new(&format!("(?m)^{}", pattern)).expect("Invalid regex");

        while now.elapsed() < duration.unwrap_or_else(|| Duration::from_secs(15)) {
            if let Ok(metrics) = self
                .get_metrics_response()
                .await
                .expect("failed to fetch metrics")
                .text()
                .await
            {
                if re.is_match(&metrics) {
                    return;
                }
                last_metrics = metrics;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("'{text}' not detected in metrics\n{last_metrics}");
    }

    #[allow(dead_code)]
    pub async fn assert_shutdown(&mut self) {
        // Budget must cover:
        //   1. The harness's injected `connection_shutdown_timeout` default
        //      (currently 5 s, set in `merge_overrides`), which bounds how
        //      long the router's per-connection tasks wait before forcibly
        //      closing a straggler connection.
        //   2. The longest intentionally-in-flight subgraph delay any
        //      integration test induces before calling `graceful_shutdown()`.
        //      `integration::lifecycle::test_graceful_shutdown` is the
        //      binding case at 2 s.
        //   3. CI scheduling slack between the router process draining its
        //      connections and the OS actually reaping the process.
        //
        // Previously 3 s. Raised to 10 s when the harness began injecting
        // a `connection_shutdown_timeout` default to prevent the 60 s
        // production default from hanging tests that hold HTTP/2 client
        // connections open past the response (see `merge_overrides`).
        let router = self.router.as_mut().expect("router must have been started");
        let now = Instant::now();
        while now.elapsed() < Duration::from_secs(10) {
            match router.try_wait() {
                Ok(Some(_)) => {
                    self.router = None;
                    return;
                }
                Ok(None) => tokio::time::sleep(Duration::from_millis(10)).await,
                _ => {}
            }
        }

        self.dump_stack_traces();
        panic!("unable to shutdown router, this probably means a hang and should be investigated");
    }

    #[allow(dead_code)]
    #[cfg(target_family = "unix")]
    pub async fn send_sighup(&mut self) {
        unsafe {
            libc::kill(self.pid(), libc::SIGHUP);
        }
    }

    #[cfg(target_os = "linux")]
    pub fn dump_stack_traces(&self) {
        if let Ok(trace) = rstack::TraceOptions::new()
            .symbols(true)
            .thread_names(true)
            .trace(self.pid() as u32)
        {
            println!("dumped stack traces");
            for thread in trace.threads() {
                println!(
                    "thread id: {}, name: {}",
                    thread.id(),
                    thread.name().unwrap_or("<unknown>")
                );

                for frame in thread.frames() {
                    println!(
                        "  {}",
                        frame.symbol().map(|s| s.name()).unwrap_or("<unknown>")
                    );
                }
            }
        } else {
            println!("failed to dump stack trace");
        }
    }
    #[cfg(not(target_os = "linux"))]
    pub fn dump_stack_traces(&self) {}

    #[allow(dead_code)]
    pub(crate) fn force_flush(&self) {
        let tracer_provider_client = self._tracer_provider_client.clone();
        let tracer_provider_subgraph = self._tracer_provider_subgraph.clone();
        if let Err(e) = tracer_provider_subgraph.force_flush() {
            eprintln!("failed to flush subgraph tracer: {e}");
        }

        if let Err(e) = tracer_provider_client.force_flush() {
            eprintln!("failed to flush client tracer: {e}");
        }
    }

    #[allow(dead_code)]
    pub async fn clear_redis_cache(&self) {
        let url = self.redis_url().expect("no redis urls");
        let config = RedisConfig::from_url(&url).unwrap();

        let client = RedisClient::new(config, None, None, None);
        let connection_task = client.connect();
        client
            .wait_for_connect()
            .await
            .expect("could not connect to redis");

        for key in self.scan(&client).await.expect("no keys") {
            client
                .del::<usize, _>(key)
                .await
                .expect("could not delete key");
        }

        client.quit().await.expect("could not quit redis");
        // calling quit ends the connection and event listener tasks
        let _ = connection_task.await;
    }

    /// Collect and return all keys found within `self.redis_namespace` in the provided client's
    /// connected redis instance.
    async fn scan(
        &self,
        client: &fred::clients::Client,
    ) -> Result<Vec<String>, fred::error::Error> {
        let pattern = format!("{}:*", self.redis_namespace);
        let mut scan = if client.is_clustered() {
            client.scan_cluster(pattern, None, None).boxed()
        } else {
            client.scan(pattern, None, None).boxed()
        };

        let mut keys = Vec::new();
        while let Some(result) = scan.next().await {
            if let Some(page) = result?.take_results() {
                for key in page {
                    let key = key.as_str().expect("key should be a string");
                    keys.push(key.to_string());
                }
            }
        }

        Ok(keys)
    }

    #[allow(dead_code)]
    pub async fn assert_redis_cache_contains(&self, key: &str) -> String {
        let url = self.redis_url().expect("no redis urls");
        let config = RedisConfig::from_url(&url).unwrap();
        let client = RedisClient::new(config, None, None, None);
        let connection_task = client.connect();
        client.wait_for_connect().await.unwrap();
        let redis_namespace = &self.redis_namespace;
        let namespaced_key = format!("{redis_namespace}:{key}");
        let s = match client.get(&namespaced_key).await {
            Ok(s) => s,
            Err(e) => {
                panic!(
                    "key {key} not found: {e}\n This may be caused by a number of things including federation version changes"
                );
            }
        };

        client.quit().await.unwrap();
        // calling quit ends the connection and event listener tasks
        let _ = connection_task.await;
        s
    }

    #[allow(dead_code)]
    pub async fn assert_redis_cache_contains_key_matching(&self, pattern: &str) {
        let url = self.redis_url().expect("no redis urls");
        let config = RedisConfig::from_url(&url).unwrap();
        let client = RedisClient::new(config, None, None, None);
        let connection_task = client.connect();
        client.wait_for_connect().await.unwrap();

        let keys = self
            .scan(&client)
            .await
            .expect("couldn't get keys from redis");
        let redis_namespace = &self.redis_namespace;

        let matching_key = keys.iter().find(|key| {
            let unnamespaced = key.replace(&format!("{redis_namespace}:"), "");
            unnamespaced.contains(pattern)
        });

        if matching_key.is_none() {
            panic!("no key matching pattern '{pattern}' found in Redis cache");
        }

        client.quit().await.unwrap();
        let _ = connection_task.await;
    }

    /// Return the first URL in `self.redis_urls`.
    ///
    /// This `Vec` will have been populated by the config provided to `IntegrationTest` upon
    /// initialization.
    fn redis_url(&self) -> Option<String> {
        Some(self.redis_urls.as_ref()?.iter().next()?.clone())
    }
}

impl Drop for IntegrationTest {
    fn drop(&mut self) {
        if let Some(child) = &mut self.router {
            let _ = child.start_kill();
        }
    }
}

/// Merge in overrides to a yaml config.
///
/// The test harness needs some options to be present for it to work, so this
/// function allows patching any config to include the needed values.
fn merge_overrides(
    yaml: &str,
    subgraph_overrides: &HashMap<String, String>,
    apollo_otlp_endpoint: &str,
    bind_addr: Option<SocketAddr>,
    redis_namespace: &str,
    port_replacements: Option<&HashMap<String, u16>>,
) -> Value {
    let bind_addr = bind_addr
        .map(|a| a.to_string())
        .unwrap_or_else(|| "127.0.0.1:0".into());

    // Apply port replacements to the YAML string first
    let mut yaml_with_ports = yaml.to_string();
    if let Some(port_replacements) = port_replacements {
        for (placeholder, port) in port_replacements {
            // Replace placeholder patterns like {{PLACEHOLDER_NAME}} with the actual port
            let placeholder_pattern = format!("{{{{{placeholder}}}}}");
            yaml_with_ports = yaml_with_ports.replace(&placeholder_pattern, &port.to_string());

            // Also replace patterns like :{{PLACEHOLDER_NAME}} with :port
            let port_pattern = format!(":{{{{{placeholder}}}}}");
            yaml_with_ports = yaml_with_ports.replace(&port_pattern, &format!(":{port}"));

            // Replace full address patterns like 127.0.0.1:{{PLACEHOLDER_NAME}}
            let addr_pattern = format!("127.0.0.1:{{{{{placeholder}}}}}");
            yaml_with_ports = yaml_with_ports.replace(&addr_pattern, &format!("127.0.0.1:{port}"));
        }
    }

    // Parse the config as yaml
    let mut config: Value = serde_yaml::from_str(&yaml_with_ports).unwrap();

    // Insert subgraph overrides, making sure to keep other overrides if present
    let overrides = subgraph_overrides
        .iter()
        .map(|(name, url)| (name.clone(), serde_json::Value::String(url.clone())));
    let overrides2 = overrides.clone();
    match config
        .as_object_mut()
        .and_then(|o| o.get_mut("override_subgraph_url"))
        .and_then(|o| o.as_object_mut())
    {
        None => {
            if let Some(o) = config.as_object_mut() {
                o.insert("override_subgraph_url".to_string(), overrides.collect());
            }
        }
        Some(override_url) => {
            override_url.extend(overrides);
        }
    }
    if let Some(sources) = config
        .as_object_mut()
        .and_then(|o| o.get_mut("connectors"))
        .and_then(|o| o.as_object_mut())
        .and_then(|o| o.get_mut("sources"))
        .and_then(|o| o.as_object_mut())
    {
        for (name, url) in overrides2 {
            let mut obj = serde_json::Map::new();
            obj.insert("override_url".to_string(), url.clone());
            sources.insert(format!("connectors.{name}"), Value::Object(obj));
        }
    }

    // Override the listening address always since we spawn the router on a
    // random port. However, don't override Unix socket paths.
    //
    // Also inject a bounded `connection_shutdown_timeout` default for every
    // integration test. The production default is 60 s (see
    // `default_connection_shutdown_timeout` in src/configuration/mod.rs),
    // which is safe for long-lived production connections but dangerous for
    // tests: `assert_shutdown` in this harness only allows a bounded
    // wall-clock window for the router process to exit after SIGTERM. If a
    // hyper HTTP/2 client keeps its pooled TCP connection open at the moment
    // `graceful_shutdown()` is called, the per-connection task in
    // `handle_connection!` (src/axum_factory/listeners.rs) flips into the
    // `connection_shutdown.cancelled()` branch and waits up to
    // `connection_shutdown_timeout` for the connection to actually terminate.
    // 60 s >> `assert_shutdown`'s budget -> assertion panics with
    // "unable to shutdown router, this probably means a hang".
    //
    // This race is latent in any test that makes an HTTP request and then
    // calls `graceful_shutdown()`. It first surfaced on 2026-04-16 against
    // `test_http2_max_header_list_size_exceeded` (see commit f4d6aa0c6).
    // Rather than patch each vulnerable fixture individually, inject a 5 s
    // default at the harness layer, paired with a widened `assert_shutdown`
    // budget (see that helper for the matching constant).
    //
    // The 5 s value is a trade-off:
    // - Must be long enough that intentionally-in-flight requests finish
    //   gracefully. `integration::lifecycle::test_graceful_shutdown` is the
    //   binding constraint: it issues a request with a 2 s subgraph delay
    //   and expects the response to arrive intact before the router exits.
    //   5 s covers that 2 s delay plus generous CI scheduling slack.
    // - Must be short enough to beat `assert_shutdown`'s budget with enough
    //   headroom that CI stall between "connection task exits" and
    //   "process exits" doesn't trigger a false positive. With
    //   `assert_shutdown` at 10 s and this at 5 s, there's 5 s of slack.
    //
    // Tests that explicitly set `supergraph.connection_shutdown_timeout` in
    // their YAML fixture (eg. integration::lifecycle's
    // small_connection_shutdown_timeout tests that exercise the feature
    // itself) keep their configured value.
    const HARNESS_CONNECTION_SHUTDOWN_TIMEOUT: &str = "5s";
    match config
        .as_object_mut()
        .and_then(|o| o.get_mut("supergraph"))
        .and_then(|o| o.as_object_mut())
    {
        None => {
            if let Some(o) = config.as_object_mut() {
                o.insert(
                    "supergraph".to_string(),
                    serde_json::json!({
                        "listen": bind_addr.to_string(),
                        "connection_shutdown_timeout": HARNESS_CONNECTION_SHUTDOWN_TIMEOUT,
                    }),
                );
            }
        }
        Some(supergraph_conf) => {
            // check if the listen address is a Unix socket path (ie, starts with /)
            let is_unix_socket = supergraph_conf
                .get("listen")
                .and_then(|v| v.as_str())
                .map(|s| s.starts_with('/'))
                .unwrap_or(false);

            // only override if it's not a Unix socket
            if !is_unix_socket {
                supergraph_conf.insert(
                    "listen".to_string(),
                    serde_json::Value::String(bind_addr.to_string()),
                );
            }

            // Only inject the shutdown timeout if the fixture hasn't set one.
            // Tests in integration::lifecycle deliberately configure this
            // setting to exercise its behavior and must not have their value
            // clobbered.
            if !supergraph_conf.contains_key("connection_shutdown_timeout") {
                supergraph_conf.insert(
                    "connection_shutdown_timeout".to_string(),
                    serde_json::Value::String(HARNESS_CONNECTION_SHUTDOWN_TIMEOUT.to_string()),
                );
            }
        }
    }

    // Override the metrics listening address always since we spawn the router on a
    // random port.
    if let Some(prom_config) = config
        .as_object_mut()
        .and_then(|o| o.get_mut("telemetry"))
        .and_then(|o| o.as_object_mut())
        .and_then(|o| o.get_mut("exporters"))
        .and_then(|o| o.as_object_mut())
        .and_then(|o| o.get_mut("metrics"))
        .and_then(|o| o.as_object_mut())
        .and_then(|o| o.get_mut("prometheus"))
        .and_then(|o| o.as_object_mut())
    {
        prom_config.insert(
            "listen".to_string(),
            serde_json::Value::String(bind_addr.to_string()),
        );
    }

    // Pin every Apollo Studio reporting endpoint to the per-test wiremock at
    // `apollo_otlp_endpoint`. This stops integration tests from making
    // outbound HTTPS requests to `usage-reporting.api.apollographql.com` —
    // which were both a hidden CI dependency on public-Internet reachability
    // and a source of non-determinism (counters that "should" increment in
    // tests only did so if the request landed within the assertion deadline).
    //
    // We override two distinct keys:
    //   * `experimental_otlp_endpoint` is consumed by the OTLP exporter
    //     (`apollo_otlp_exporter.rs`).
    //   * `endpoint` is consumed by the legacy Apollo-protocol exporter
    //     (`apollo_exporter.rs`).
    // Both default to `https://usage-reporting.api.apollographql.com/...`,
    // and the catch-all `POST → 200` route mounted on `apollo_otlp_server`
    // accepts whichever path the router posts to.
    //
    // If the user-supplied YAML has no `telemetry.apollo` block, we insert
    // one. That has no side-effects beyond pinning the endpoints, since
    // every other Apollo-block setting falls back to its serde default.
    let telemetry_obj = config
        .as_object_mut()
        .and_then(|o| o.get_mut("telemetry"))
        .and_then(|o| o.as_object_mut());
    if let Some(telemetry) = telemetry_obj {
        let apollo_entry = telemetry
            .entry("apollo".to_string())
            .or_insert_with(|| serde_json::Value::Object(Default::default()));
        if let Some(apollo_config) = apollo_entry.as_object_mut() {
            apollo_config.insert(
                "experimental_otlp_endpoint".to_string(),
                serde_json::Value::String(apollo_otlp_endpoint.to_string()),
            );
            apollo_config.insert(
                "endpoint".to_string(),
                serde_json::Value::String(apollo_otlp_endpoint.to_string()),
            );
        }
    }

    // Set health check listen address to avoid port conflicts
    config
        .as_object_mut()
        .expect("config should be an object")
        .insert(
            "health_check".to_string(),
            json!({"listen": bind_addr.to_string()}),
        );

    let insert_redis_namespace = |v: Option<&mut Value>| {
        if let Some(v) = v.and_then(|o| o.as_object_mut()) {
            v.insert("namespace".to_string(), redis_namespace.into());
        }
    };

    insert_redis_namespace(config.pointer_mut("/supergraph/query_planning/cache/redis"));
    insert_redis_namespace(config.pointer_mut("/apq/router/cache/redis"));
    insert_redis_namespace(config.pointer_mut("/preview_entity_cache/subgraph/all/redis"));
    insert_redis_namespace(config.pointer_mut("/response_cache/subgraph/all/redis"));
    for per_subgraph_path in [
        "/response_cache/subgraph/subgraphs",
        "/preview_entity_cache/subgraph/subgraphs",
    ] {
        if let Some(subgraphs) = config
            .pointer_mut(per_subgraph_path)
            .and_then(|o| o.as_object_mut())
        {
            for subgraph_config in subgraphs.values_mut() {
                insert_redis_namespace(subgraph_config.pointer_mut("/redis"));
            }
        }
    }

    config
}

/// Extract Redis URLs from config. This assumes that caches will share a redis instance; it just
/// returns the first URLs found from any known Redis config path.
fn get_redis_urls(config: &Value) -> Option<Vec<String>> {
    let convert_urls = |urls: &Vec<Value>| {
        urls.iter()
            .filter_map(|v| v.as_str().map(|s| s.to_string()))
            .collect()
    };

    let top_level_paths = [
        "/supergraph/query_planning/cache/redis/urls",
        "/apq/router/cache/redis/urls",
        "/preview_entity_cache/subgraph/all/redis/urls",
        "/response_cache/subgraph/all/redis/urls",
    ];
    for path in top_level_paths {
        if let Some(urls) = config.pointer(path).and_then(|o| o.as_array()) {
            return Some(convert_urls(urls));
        }
    }

    let per_subgraph_sections = [
        "/response_cache/subgraph/subgraphs",
        "/preview_entity_cache/subgraph/subgraphs",
    ];
    for section in per_subgraph_sections {
        if let Some(subgraphs) = config.pointer(section).and_then(|o| o.as_object()) {
            for subgraph_config in subgraphs.values() {
                if let Some(urls) = subgraph_config
                    .pointer("/redis/urls")
                    .and_then(|o| o.as_array())
                {
                    return Some(convert_urls(urls));
                }
            }
        }
    }

    None
}

#[allow(dead_code)]
pub fn graph_os_enabled() -> bool {
    matches!(
        (
            std::env::var("TEST_APOLLO_KEY"),
            std::env::var("TEST_APOLLO_GRAPH_REF"),
        ),
        (Ok(_), Ok(_))
    )
}

/// Automatic tracing initialization using ctor for integration tests
#[ctor::ctor(unsafe)]
fn init_integration_test_tracing() {
    // Initialize tracing for integration tests
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .or_else(|_| tracing_subscriber::EnvFilter::try_new("info,apollo_router=debug"))
        .unwrap();

    let _ = tracing_subscriber::registry()
        .with(
            tracing_subscriber::fmt::Layer::default()
                .with_target(false)
                .with_thread_ids(false)
                .with_thread_names(false)
                .compact()
                .with_filter(filter),
        )
        .try_init();
}
