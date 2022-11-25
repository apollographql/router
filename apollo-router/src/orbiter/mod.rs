use std::collections::HashMap;
use std::env;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use clap::CommandFactory;
use http::header::{CONTENT_TYPE, USER_AGENT};
use jsonschema::output::BasicOutput;
use jsonschema::paths::PathChunk;
use jsonschema::JSONSchema;
use lazy_static::lazy_static;
use serde::Serialize;
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use tower::BoxError;
use uuid::Uuid;

use crate::configuration::generate_config_schema;
use crate::Configuration;
// With regards to ELv2 licensing, this entire file is license key functionality
use crate::executable::Opt;
use crate::plugin::DynPlugin;
use crate::router_factory::RouterSuperServiceFactory;
use crate::spec::Schema;

lazy_static! {
    /// This session id is created once when the router starts. It persists between config reloads.
    static ref SESSION_ID: Uuid = Uuid::new_v4();
}

/// Platform represents the platform the CLI is being run from
#[derive(Debug, Serialize)]
struct Platform {
    /// the platform from which the command was run (i.e. linux, macOS, windows or even wsl)
    os: String,

    /// if we think this command is being run in CI
    continuous_integration: Option<ci_info::types::Vendor>,
}

/// A usage report for the router
#[derive(Serialize)]
struct UsageReport {
    /// A random ID that is generated on first startup of the Router. It is not persistent between restarts of the Router, but will be persistent for hot reloads
    session_id: Uuid,
    /// The version of the Router
    version: String,
    /// Information about the current architecture/platform
    platform: Platform,
    /// A hash of the supergraph schema
    supergraph_hash: String,
    /// The apollo key if specified
    apollo_key: Option<String>,
    /// The apollo graph ref is specified
    apollo_graph_ref: Option<String>,
    /// Information about what was being used
    usage: Map<String, Value>,
}

impl<T: RouterSuperServiceFactory> OrbiterRouterSuperServiceFactory<T> {
    pub(crate) fn new(delegate: T) -> OrbiterRouterSuperServiceFactory<T> {
        OrbiterRouterSuperServiceFactory { delegate }
    }
}

/// A service factory that will report some anonymous telemetry to Apollo. It can be disabled by users, but the data is useful for helping us to decide where to spend our efforts.
/// In future we should try and move this towards otel metrics, this will allow us to send the information direct to something that ingests OTLP.
/// The data sent looks something like this:
/// ```json
/// {
///   "session_id": "fbe09da3-ebdb-4863-8086-feb97464b8d7",
///   "version": "1.4.0", // The version of the router
///   "os": "linux",
///   "ci": null,
///   "supergraph_hash": "anebfoiwhefowiefj",
///   "apollo-key": "<the actualy key>|anonymous",
///   "apollo-graph-ref": "<the actual graph ref>|unmanaged"
///   "usage": {
///     "configuration.headers.all.request.propagate.named.redacted": 3
///     "configuration.headers.all.request.propagate.default.redacted": 1
///     "configuration.headers.all.request.len": 3
///     "configuration.headers.subgraphs.redacted.request.propagate.named.redacted": 2
///     "configuration.headers.subgraphs.redacted.request.len": 2
///     "configuration.headers.subgraphs.len": 1
///     "configuration.homepage.enabled.true": 1
///     "args.config-path.redacted": 1,
///     "args.hot-reload.true": 1,
///     //Many more keys. This is dynamic and will change over time.
///     //More...
///     //More...
///     //More...
///   }
/// }
/// ```
#[derive(Default)]
pub(crate) struct OrbiterRouterSuperServiceFactory<T: RouterSuperServiceFactory> {
    delegate: T,
}

#[async_trait]
impl<T: RouterSuperServiceFactory> RouterSuperServiceFactory
    for OrbiterRouterSuperServiceFactory<T>
{
    type RouterFactory = T::RouterFactory;

    async fn create<'a>(
        &'a mut self,
        configuration: Arc<Configuration>,
        schema: Arc<Schema>,
        previous_router: Option<&'a Self::RouterFactory>,
        extra_plugins: Option<Vec<(String, Box<dyn DynPlugin>)>>,
    ) -> Result<Self::RouterFactory, BoxError> {
        self.delegate
            .create(
                configuration.clone(),
                schema.clone(),
                previous_router,
                extra_plugins,
            )
            .await
            .map(|factory| {
                if env::var("APOLLO_TELEMETRY_DISABLED").unwrap_or_default() != "true" {
                    tokio::task::spawn_blocking(|| {
                        tracing::debug!("sending anonymous usage data to Apollo");
                        let report = create_report(configuration, schema);
                        if let Err(e) = send(report) {
                            tracing::debug!("failed to send anonymous usage: {}", e);
                        }
                    });
                }
                factory
            })
    }
}

fn create_report(configuration: Arc<Configuration>, schema: Arc<Schema>) -> UsageReport {
    let mut configuration: Value =
        serde_yaml::from_str(&configuration.string).expect("configuration must be parseable");
    let os = get_os();
    let mut usage = HashMap::new();

    // We only report apollo plugins. This way we don't risk leaking sensitive data if the user has customized the router and added their own plugins.
    usage.insert(
        "configuration.plugins.len".to_string(),
        configuration
            .get("plugins")
            .map(|plugins| plugins.as_array())
            .flatten()
            .map(|plugins| plugins.len())
            .unwrap_or_default() as u64,
    );

    // Delete the plugins block so that we don't report on it.
    // A custom plugin may have configuration that is sensitive.
    configuration
        .as_object_mut()
        .expect("configuration should have been an object")
        .remove("plugins");

    // Visit the config
    visit_config(&mut usage, &configuration);

    // Check the command line options. This encapsulates both env and command line functionality
    // This won't work in tests so we have separate test code.
    #[cfg(not(test))]
    visit_args(&mut usage, env::args().collect());

    let mut hasher = Sha256::default();
    hasher.update(schema.string.as_bytes());
    let result = hasher.finalize();
    let supergraph_hash = base64::encode(result);

    UsageReport {
        session_id: *SESSION_ID,
        version: std::env!("CARGO_PKG_VERSION").to_string(),
        platform: Platform {
            os,
            continuous_integration: ci_info::get().vendor,
        },
        supergraph_hash,
        apollo_key: std::env::var("APOLLO_KEY").ok(),
        apollo_graph_ref: std::env::var("APOLLO_GRAPH_REF").ok(),
        usage: usage
            .into_iter()
            .map(|(k, v)| (k, Value::Number(v.into())))
            .collect(),
    }
}

fn visit_args(usage: &mut HashMap<String, u64>, args: Vec<String>) {
    let matches = Opt::command().get_matches_from(args);

    Opt::command().get_arguments().for_each(|a| {
        let defaults = a.get_default_values().to_vec();
        if let Some(values) = matches.get_raw(a.get_id().as_str()) {
            let values = values.collect::<Vec<_>>();

            // First check booleans, then only record if the value differed from the default
            if values == ["true"] || values == ["false"] {
                if values == ["true"] {
                    usage.insert(format!("args.{}.true", a.get_id()), 1);
                }
            } else if defaults != values {
                usage.insert(format!("args.{}.redacted", a.get_id()), 1);
            }
        }
    });
}

fn send(body: UsageReport) -> Result<String, BoxError> {
    tracing::debug!("anonymous usage: {}", serde_json::to_string_pretty(&body)?);

    #[cfg(not(test))]
    let url = "https://router.apollo.dev/telemetry";
    #[cfg(test)]
    let url = "http://localhost:8888/telemetry";

    Ok(reqwest::blocking::Client::new()
        .post(url)
        .header(USER_AGENT, "router")
        .header(CONTENT_TYPE, "application/json")
        .json(&serde_json::to_value(body)?)
        .timeout(Duration::from_secs(10))
        .send()?
        .text()?)
}

fn get_os() -> String {
    if wsl::is_wsl() {
        "wsl"
    } else {
        std::env::consts::OS
    }
    .to_string()
}

fn visit_config(usage: &mut HashMap<String, u64>, config: &Value) {
    // We have to be careful not to expose names of headers, metadata or anything else sensitive.
    let raw_json_schema =
        serde_json::to_value(generate_config_schema()).expect("config schema is must be valid");
    let compiled_json_schema = JSONSchema::compile(
        &serde_json::to_value(&raw_json_schema).expect("config schema is must be valid"),
    )
    .expect("config schema must compile");

    // We can use jsonschema to give us annotations about the validated config. This means that we get
    // a pointer into the config document and also a pointer into the schema.
    // For this to work ALL config must have an annotation, e.g. documentation.
    // For each config path we need to sanitize the it for arrays and also custom names e.g. header names.
    // This corresponds to the json schema keywords of `items` and `additionalProperties`
    if let BasicOutput::Valid(output) = compiled_json_schema.apply(&config).basic() {
        for item in output {
            let instance_ptr = item.instance_location();
            let value = config
                .pointer(&instance_ptr.to_string())
                .expect("pointer must point to value");

            // Compose the redacted path.
            let mut path = Vec::new();
            let mut keyword_location = item.keyword_location().iter();
            while let Some(keyword) = keyword_location.next() {
                if let PathChunk::Property(property) = keyword {
                    // We hit a properties keyword, we can grab the next keyword as it'll be a property name.
                    path.push(property.to_string());
                }
                if keyword == &PathChunk::Keyword("additionalProperties") {
                    // This is free format properties. It's redacted
                    path.push("redacted".to_string());
                }
            }
            let path = path.join(".");

            if matches!(item.keyword_location().last(), Some(&PathChunk::Index(_))) {
                *usage
                    .entry(format!("configuration{}.len", path))
                    .or_default() += 1;
            }
            match value {
                Value::Bool(value) => {
                    *usage
                        .entry(format!("configuration{}.{}", path, value))
                        .or_default() += 1;
                }
                Value::Number(value) => {
                    *usage
                        .entry(format!("configuration{}.{}", path, value))
                        .or_default() += 1;
                }
                Value::String(_) => {
                    // Strings are never output
                    *usage
                        .entry(format!("configuration{}.redacted", path))
                        .or_default() += 1;
                }
                _ => {}
            }
        }
    } else {
        panic!("schema should have been valid");
    }
}

#[cfg(test)]
mod test {
    use std::collections::HashMap;
    use std::env;
    use std::sync::Arc;

    use crate::Configuration;
    use insta::assert_yaml_snapshot;

    use crate::orbiter::visit_config;
    use crate::orbiter::{create_report, visit_args};

    #[test]
    fn test_visit_args() {
        let mut usage = HashMap::new();
        visit_args(
            &mut usage,
            ["router", "--config", "a", "--hot-reload"]
                .into_iter()
                .map(|a| a.to_string())
                .collect(),
        );
        insta::with_settings!({sort_maps => true}, {
            assert_yaml_snapshot!(usage);
        });
    }

    #[test]
    fn test_visit_config() {
        let config = serde_yaml::from_str(include_str!("testdata/redaction.router.yaml"))
            .expect("yaml must be valid");
        let mut usage = HashMap::new();
        visit_config(&mut usage, &config);
        insta::with_settings!({sort_maps => true}, {
            assert_yaml_snapshot!(usage);
        });
    }

    #[test]
    fn test_create_report() {
        let config_string = include_str!("testdata/redaction.router.yaml").to_string();
        let mut config: Configuration =
            serde_yaml::from_str(&config_string).expect("yaml must be valid");
        config.string = Arc::new(config_string);
        let report = create_report(Arc::new(config), Arc::new(crate::spec::Schema::default()));
        insta::with_settings!({sort_maps => true}, {
                    assert_yaml_snapshot!(report, {
                ".session_id" => "[session_id]",
                ".apollo_key" => "[apollo_key]",
                ".apollo_graph_ref" => "[apollo_graph_ref]",
        });
                });
    }

    // TODO, enable once we are live.
    // #[test]
    // fn test_send() {
    //     let response = send(UsageReport {
    //         session_id: Uuid::from_str("433c123c-8dba-11ed-a1eb-0242ac120002").expect("uuid"),
    //         version: "session2".to_string(),
    //         platform: Platform {
    //             os: "test".to_string(),
    //             continuous_integration: Some(Vendor::CircleCI),
    //         },
    //         supergraph_hash: "supergraph_hash".to_string(),
    //         apollo_key: Some("apollo_key".to_string()),
    //         apollo_graph_ref: Some("apollo_graph_ref".to_string()),
    //         usage: Default::default(),
    //     })
    //     .expect("expected send to succeed");
    //
    //     assert_eq!(response, "Report received");
    // }
}
