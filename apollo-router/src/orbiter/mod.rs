use std::collections::HashMap;
use std::collections::HashSet;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use clap::CommandFactory;
use http::header::CONTENT_TYPE;
use http::header::USER_AGENT;
use jsonpath_rust::JsonPathInst;
use mime::APPLICATION_JSON;
use once_cell::sync::OnceCell;
use serde::Serialize;
use serde_json::Map;
use serde_json::Value;
use tower::BoxError;
use uuid::Uuid;

use crate::configuration::generate_config_schema;
use crate::executable::Opt;
use crate::plugin::DynPlugin;
use crate::router_factory::RouterSuperServiceFactory;
use crate::router_factory::YamlRouterFactory;
use crate::services::router::service::RouterCreator;
use crate::services::HasSchema;
use crate::spec::Schema;
use crate::Configuration;

/// This session id is created once when the router starts. It persists between config reloads and supergraph schema changes.
static SESSION_ID: OnceCell<Uuid> = OnceCell::new();

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
    /// Information about what was being used
    usage: Map<String, Value>,
}

impl OrbiterRouterSuperServiceFactory {
    pub(crate) fn new(delegate: YamlRouterFactory) -> OrbiterRouterSuperServiceFactory {
        OrbiterRouterSuperServiceFactory { delegate }
    }
}

/// A service factory that will report some anonymous telemetry to Apollo. It can be disabled by users, but the data is useful for helping us to decide where to spend our efforts.
/// The data sent looks something like this:
/// ```json
/// {
///   "session_id": "fbe09da3-ebdb-4863-8086-feb97464b8d7",
///   "version": "1.4.0", // The version of the router
///   "os": "linux",
///   "ci": null,
///   "usage": {
///     "configuration.headers.all.request.propagate.named.<redacted>": 3
///     "configuration.headers.all.request.propagate.default.<redacted>": 1
///     "configuration.headers.all.request.len": 3
///     "configuration.headers.subgraphs.<redacted>.request.propagate.named.<redacted>": 2
///     "configuration.headers.subgraphs.<redacted>.request.len": 2
///     "configuration.headers.subgraphs.len": 1
///     "configuration.homepage.enabled.true": 1
///     "args.config-path.<redacted>": 1,
///     "args.hot-reload.true": 1,
///     //Many more keys. This is dynamic and will change over time.
///     //More...
///     //More...
///     //More...
///   }
/// }
/// ```
#[derive(Default)]
pub(crate) struct OrbiterRouterSuperServiceFactory {
    delegate: YamlRouterFactory,
}

#[async_trait]
impl RouterSuperServiceFactory for OrbiterRouterSuperServiceFactory {
    type RouterFactory = RouterCreator;

    async fn create<'a>(
        &'a mut self,
        is_telemetry_disabled: bool,
        configuration: Arc<Configuration>,
        schema: String,
        previous_router: Option<&'a Self::RouterFactory>,
        extra_plugins: Option<Vec<(String, Box<dyn DynPlugin>)>>,
    ) -> Result<Self::RouterFactory, BoxError> {
        self.delegate
            .create(
                is_telemetry_disabled,
                configuration.clone(),
                schema.clone(),
                previous_router,
                extra_plugins,
            )
            .await
            .inspect(|factory| {
                if !is_telemetry_disabled {
                    let schema = factory.supergraph_creator.schema();

                    tokio::task::spawn(async move {
                        tracing::debug!("sending anonymous usage data to Apollo");
                        let report = create_report(configuration, schema);
                        if let Err(e) = send(report).await {
                            tracing::debug!("failed to send usage report: {}", e);
                        }
                    });
                }
            })
    }
}

fn create_report(configuration: Arc<Configuration>, _schema: Arc<Schema>) -> UsageReport {
    let mut configuration: Value = configuration
        .validated_yaml
        .clone()
        .unwrap_or_else(|| Value::Object(Default::default()));
    let os = get_os();
    let mut usage = HashMap::new();

    // We only report apollo plugins. This way we don't risk leaking sensitive data if the user has customized the router and added their own plugins.
    usage.insert(
        "configuration.plugins.len".to_string(),
        configuration
            .get("plugins")
            .and_then(|plugins| plugins.as_array())
            .map(|plugins| plugins.len())
            .unwrap_or_default() as u64,
    );

    // Make sure the config is an object, but don't fail if it wasn't
    if !configuration.is_object() {
        configuration = Value::Object(Default::default());
    }

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
    visit_args(&mut usage, std::env::args().collect());

    UsageReport {
        session_id: *SESSION_ID.get_or_init(Uuid::new_v4),
        version: std::env!("CARGO_PKG_VERSION").to_string(),
        platform: Platform {
            os,
            continuous_integration: ci_info::get().vendor,
        },
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
                usage.insert(format!("args.{}.<redacted>", a.get_id()), 1);
            }
        }
    });
}

async fn send(body: UsageReport) -> Result<String, BoxError> {
    tracing::debug!(
        "transmitting anonymous analytics: {}",
        serde_json::to_string_pretty(&body)?
    );

    #[cfg(not(test))]
    let url = "https://router.apollo.dev/telemetry";
    #[cfg(test)]
    let url = "http://localhost:8888/telemetry";

    Ok(reqwest::Client::new()
        .post(url)
        .header(USER_AGENT, "router")
        .header(CONTENT_TYPE, APPLICATION_JSON.essence_str())
        .json(&serde_json::to_value(body)?)
        .timeout(Duration::from_secs(10))
        .send()
        .await?
        .text()
        .await?)
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
        serde_json::to_value(generate_config_schema()).expect("config schema must be valid");
    // We can't use json schema to redact the config as we don't have the annotations.
    // Instead, we get the set of properties from the schema and anything that doesn't match a property is redacted.
    let path = JsonPathInst::from_str("$..properties").expect("properties path must be valid");
    let slice = path.find_slice(&raw_json_schema);
    let schema_properties: HashSet<String> = slice
        .iter()
        .filter_map(|v| v.as_object())
        .flat_map(|o| o.keys())
        .map(|s| s.to_string())
        .collect();

    // Now for each leaf in the config we get the path and redact anything that isn't in the schema.
    visit_value(&schema_properties, usage, config, "");
}

fn visit_value(
    schema_properties: &HashSet<String>,
    usage: &mut HashMap<String, u64>,
    value: &Value,
    path: &str,
) {
    match value {
        Value::Bool(value) => {
            *usage
                .entry(format!("configuration.{path}.{value}"))
                .or_default() += 1;
        }
        Value::Number(value) => {
            *usage
                .entry(format!("configuration.{path}.{value}"))
                .or_default() += 1;
        }
        Value::String(_) => {
            // Strings are never output
            *usage
                .entry(format!("configuration.{path}.<redacted>"))
                .or_default() += 1;
        }
        Value::Object(o) => {
            for (key, value) in o {
                let key = if schema_properties.contains(key) {
                    key
                } else {
                    "<redacted>"
                };

                if path.is_empty() {
                    visit_value(schema_properties, usage, value, key);
                } else {
                    visit_value(schema_properties, usage, value, &format!("{path}.{key}"));
                    *usage
                        .entry(format!("configuration.{path}.{key}.len"))
                        .or_default() += 1;
                }
            }
        }
        Value::Array(a) => {
            for value in a {
                visit_value(schema_properties, usage, value, path);
            }
            *usage
                .entry(format!("configuration.{path}.array.len"))
                .or_default() += a.len() as u64;
        }
        Value::Null => {}
    }
}

#[cfg(test)]
mod test {
    use std::collections::HashMap;
    use std::env;
    use std::str::FromStr;
    use std::sync::Arc;

    use insta::assert_yaml_snapshot;
    use serde_json::json;
    use serde_json::Value;

    use crate::orbiter::create_report;
    use crate::orbiter::visit_args;
    use crate::orbiter::visit_config;
    use crate::Configuration;

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
        usage.remove("args.anonymous_telemetry_disabled.true");
        usage.remove("args.apollo_graph_ref.<redacted>");
        usage.remove("args.apollo_key.<redacted>");
        insta::with_settings!({sort_maps => true}, {
            assert_yaml_snapshot!(usage);
        });
    }

    // The following two tests are ignored because since allowing refs in schema we can no longer
    // examine the annotations for redaction.
    // https://github.com/Stranger6667/jsonschema-rs/issues/403
    // We should remove the orbiter code and move to otel for both anonymous and non-anonymous telemetry.
    #[test]
    fn test_visit_config() {
        let config = Configuration::from_str(include_str!("testdata/redaction.router.yaml"))
            .expect("yaml must be valid");
        let mut usage = HashMap::new();
        visit_config(
            &mut usage,
            config
                .validated_yaml
                .as_ref()
                .expect("config should have had validated_yaml"),
        );
        insta::with_settings!({sort_maps => true}, {
            assert_yaml_snapshot!(usage);
        });
    }

    #[test]
    fn test_visit_config_that_needed_upgrade() {
        let config: Configuration =
            Configuration::from_str("supergraph:\n  preview_defer_support: true")
                .expect("config must be valid");
        let mut usage = HashMap::new();
        visit_config(
            &mut usage,
            config
                .validated_yaml
                .as_ref()
                .expect("config should have had validated_yaml"),
        );
        insta::with_settings!({sort_maps => true}, {
            assert_yaml_snapshot!(usage);
        });
    }

    #[test]
    fn test_create_report() {
        let config = Configuration::from_str(include_str!("testdata/redaction.router.yaml"))
            .expect("config must be valid");
        let schema_string = include_str!("../testdata/minimal_supergraph.graphql");
        let schema = crate::spec::Schema::parse(schema_string, &Default::default()).unwrap();
        let report = create_report(Arc::new(config), Arc::new(schema));
        insta::with_settings!({sort_maps => true}, {
                    assert_yaml_snapshot!(report, {
                ".version" => "[version]",
                ".session_id" => "[session_id]",
                ".platform.os" => "[os]",
                ".platform.continuous_integration" => "[ci]",
            });
        });
    }

    #[test]
    fn test_create_report_incorrect_type_validated_yaml() {
        let mut config = Configuration::from_str(include_str!("testdata/redaction.router.yaml"))
            .expect("config must be valid");
        config.validated_yaml = Some(Value::Null);
        let schema_string = include_str!("../testdata/minimal_supergraph.graphql");
        let schema = crate::spec::Schema::parse(schema_string, &Default::default()).unwrap();
        let report = create_report(Arc::new(config), Arc::new(schema));
        insta::with_settings!({sort_maps => true}, {
                    assert_yaml_snapshot!(report, {
                ".version" => "[version]",
                ".session_id" => "[session_id]",
                ".platform.os" => "[os]",
                ".platform.continuous_integration" => "[ci]",
            });
        });
    }

    #[test]
    fn test_create_report_invalid_validated_yaml() {
        let mut config = Configuration::from_str(include_str!("testdata/redaction.router.yaml"))
            .expect("config must be valid");
        config.validated_yaml = Some(json!({"garbage": "garbage"}));
        let schema_string = include_str!("../testdata/minimal_supergraph.graphql");
        let schema = crate::spec::Schema::parse(schema_string, &Default::default()).unwrap();
        let report = create_report(Arc::new(config), Arc::new(schema));
        insta::with_settings!({sort_maps => true}, {
                    assert_yaml_snapshot!(report, {
                ".version" => "[version]",
                ".session_id" => "[session_id]",
                ".platform.os" => "[os]",
                ".platform.continuous_integration" => "[ci]",
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
    //         usage: Default::default(),
    //     })
    //     .expect("expected send to succeed");
    //
    //     assert_eq!(response, "Report received");
    // }
}
