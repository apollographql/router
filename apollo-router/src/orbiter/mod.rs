use std::collections::HashMap;
use std::env;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use clap::CommandFactory;
use http::header::CONTENT_TYPE;
use http::header::USER_AGENT;
use jsonschema::output::BasicOutput;
use jsonschema::paths::PathChunk;
use jsonschema::JSONSchema;
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
use crate::services::router_service::RouterCreator;
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
        configuration: Arc<Configuration>,
        schema: String,
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
                    let schema = factory.supergraph_creator.schema();

                    tokio::task::spawn(async move {
                        tracing::debug!("sending anonymous usage data to Apollo");
                        let report = create_report(configuration, schema);
                        if let Err(e) = send(report).await {
                            tracing::debug!("failed to send usage report: {}", e);
                        }
                    });
                }
                factory
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
    visit_args(&mut usage, env::args().collect());

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
    let compiled_json_schema = JSONSchema::compile(
        &serde_json::to_value(&raw_json_schema).expect("config schema must be valid"),
    )
    .expect("config schema must compile");

    // We can use jsonschema to give us annotations about the validated config. This means that we get
    // a pointer into the config document and also a pointer into the schema.
    // For this to work ALL config must have an annotation, e.g. documentation.
    // For each config path we need to sanitize the it for arrays and also custom names e.g. header names.
    // This corresponds to the json schema keywords of `items` and `additionalProperties`
    if let BasicOutput::Valid(output) = compiled_json_schema.apply(config).basic() {
        for item in output {
            let instance_ptr = item.instance_location();
            let value = config
                .pointer(&instance_ptr.to_string())
                .expect("pointer must point to value");

            // Compose the redacted path.
            let mut path = Vec::new();
            for chunk in item.keyword_location() {
                if let PathChunk::Property(property) = chunk {
                    // We hit a properties keyword, we can grab the next keyword as it'll be a property name.
                    path.push(property.to_string());
                }
                if &PathChunk::Keyword("additionalProperties") == chunk {
                    // This is free format properties. It's redacted
                    path.push("<redacted>".to_string());
                }
            }

            let path = path.join(".");
            if matches!(item.keyword_location().last(), Some(&PathChunk::Index(_))) {
                *usage
                    .entry(format!("configuration.{path}.len"))
                    .or_default() += 1;
            }
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
                    if matches!(
                        item.keyword_location().last(),
                        Some(&PathChunk::Property(_))
                    ) {
                        let schema_node = raw_json_schema
                            .pointer(&item.keyword_location().to_string())
                            .expect("schema node must resolve");
                        if let Some(Value::Bool(true)) = schema_node.get("additionalProperties") {
                            *usage
                                .entry(format!("configuration.{path}.len"))
                                .or_default() += o.len() as u64;
                        }
                    }
                }
                _ => {}
            }
        }
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
        let schema = crate::spec::Schema::parse(schema_string, &config).unwrap();
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
        let schema = crate::spec::Schema::parse(schema_string, &config).unwrap();
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
        let schema = crate::spec::Schema::parse(schema_string, &config).unwrap();
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
