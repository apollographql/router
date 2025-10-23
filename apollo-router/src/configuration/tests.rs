use std::collections::HashMap;
use std::fs;
use std::path::Path;
use std::path::PathBuf;

use http::Uri;
#[cfg(unix)]
use insta::assert_json_snapshot;
use regex::Regex;
use rust_embed::RustEmbed;
use schemars::generate::SchemaSettings;
use serde_json::json;
use walkdir::DirEntry;
use walkdir::WalkDir;

use super::schema::Mode;
use super::schema::validate_yaml_configuration;
use super::subgraph::SubgraphConfiguration;
use super::*;
use crate::configuration::cors::Policy;
use crate::error::SchemaError;

#[cfg(unix)]
#[test]
fn schema_generation() {
    let schema = generate_config_schema();
    insta::with_settings!({sort_maps => true}, {
        assert_json_snapshot!(&schema)
    });
    let json_schema =
        serde_json::to_string_pretty(&schema).expect("must be able to deserialize schema");
    assert!(
        json_schema.len() < 500 * 1024,
        "schema must be less than 500kb"
    );
}

#[test]
fn schema_is_valid() {
    let schema = generate_config_schema();
    jsonschema::draft7::meta::validate(schema.as_value()).expect("generated schema must be valid");
}

#[test]
fn routing_url_in_schema() {
    let schema = include_str!("../testdata/minimal_local_inventory_supergraph.graphql");
    let schema = crate::spec::Schema::parse(schema, &Default::default()).unwrap();

    let subgraphs: HashMap<&str, &Uri> = schema.subgraphs().map(|(k, v)| (k.as_str(), v)).collect();

    // if no configuration override, use the URL from the supergraph
    assert_eq!(
        subgraphs.get("accounts").unwrap().to_string(),
        "http://localhost:4001/graphql"
    );
    // if both configuration and schema specify a non empty URL, the configuration wins
    // this should show a warning in logs
    assert_eq!(
        subgraphs.get("inventory").unwrap().to_string(),
        "http://localhost:4002/graphql"
    );
    // if the configuration has a non empty routing URL, and the supergraph
    // has an empty one, the configuration wins
    assert_eq!(
        subgraphs.get("products").unwrap().to_string(),
        "http://localhost:4003/graphql"
    );

    assert_eq!(
        subgraphs.get("reviews").unwrap().to_string(),
        "http://localhost:4004/graphql"
    );
}

#[test]
fn missing_subgraph_url() {
    let schema_error =
        include_str!("../testdata/invalid_minimal_supergraph_missing_subgraph_url.graphql");
    let schema_error = crate::spec::Schema::parse(schema_error, &Default::default())
        .expect_err("Must have an error because we have one missing subgraph routing url");

    if let SchemaError::MissingSubgraphUrl(subgraph) = schema_error {
        assert_eq!(subgraph, "reviews");
    } else {
        panic!("expected missing subgraph URL for 'reviews', got: {schema_error:?}");
    }
}

#[test]
fn cors_defaults() {
    let cors = Cors::builder().build();
    let policies = cors.policies.unwrap();
    assert_eq!(policies.len(), 1);
    assert_eq!(
        policies[0].origins,
        Arc::from(["https://studio.apollographql.com".into()])
    );
    assert!(
        !cors.allow_any_origin,
        "Allow any origin should be disabled by default"
    );
    assert_eq!(
        cors.methods,
        Arc::from(["GET".into(), "POST".into(), "OPTIONS".into()])
    );
    assert!(cors.max_age.is_none());
}

#[test]
fn cors_single_origin_config() {
    let cors = Cors::builder()
        .max_age(std::time::Duration::from_secs(3600))
        .policies(vec![
            Policy::builder()
                .origins(vec!["https://trusted.com".into()])
                .allow_credentials(true)
                .allow_headers(vec!["content-type".into(), "authorization".into()])
                .expose_headers(vec!["x-custom-header".into()])
                .methods(vec!["GET".into(), "POST".into()])
                .build(),
        ])
        .build();
    let policies = cors.policies.unwrap();
    assert_eq!(policies.len(), 1);
    let oc = &policies[0];
    assert_eq!(oc.origins, Arc::from(["https://trusted.com".into()]));
    assert!(oc.allow_credentials.unwrap());
    assert_eq!(
        oc.allow_headers,
        Arc::from(["content-type".into(), "authorization".into()])
    );
    assert_eq!(oc.expose_headers, Arc::from(["x-custom-header".into()]));
    assert_eq!(oc.methods, Some(Arc::from(["GET".into(), "POST".into()])));
}

#[test]
fn bad_graphql_path_configuration_without_slash() {
    let error = Configuration::fake_builder()
        .supergraph(Supergraph::fake_builder().path("test").build())
        .build()
        .unwrap_err();
    assert_eq!(
        error.to_string(),
        String::from(
            "invalid 'server.graphql_path' configuration: 'test' is invalid, it must be an absolute path and start with '/', you should try with '/test'"
        )
    );
}

#[test]
fn bad_graphql_path_configuration_with_wildcard_as_prefix() {
    let error = Configuration::fake_builder()
        .supergraph(Supergraph::fake_builder().path("/*/test").build())
        .build()
        .unwrap_err();

    assert_eq!(
        error.to_string(),
        String::from(
            "invalid 'server.graphql_path' configuration: '/*/test' is invalid, if you need to set a path like '/*/graphql' then specify it as a path parameter with a name, for example '/:my_project_key/graphql'"
        )
    );
}

#[test]
fn unknown_fields() {
    let error = validate_yaml_configuration(
        r#"
supergraph:
  path: /
subgraphs:
  account: true
  "#,
        Expansion::default().unwrap(),
        Mode::NoUpgrade,
    )
    .expect_err("should have resulted in an error");
    assert_eq!(
        error.to_string(),
        String::from(
            r#"configuration had errors:
1. at line 4


  supergraph:
    path: /
┌ subgraphs:
|   account: true
└-----> Additional properties are not allowed ('subgraphs' was unexpected)

"#
        )
    );
}

#[test]
fn unknown_fields_at_root() {
    let error = validate_yaml_configuration(
        r#"
unknown:
  foo: true
  "#,
        Expansion::default().unwrap(),
        Mode::NoUpgrade,
    )
    .expect_err("should have resulted in an error");
    assert_eq!(
        error.to_string(),
        String::from(
            r#"configuration had errors:
1. at line 2


┌ unknown:
|   foo: true
└-----> Additional properties are not allowed ('unknown' was unexpected)

"#
        )
    );
}

#[test]
fn empty_config() {
    validate_yaml_configuration(
        r#"
  "#,
        Expansion::default().unwrap(),
        Mode::NoUpgrade,
    )
    .expect("should have been ok with an empty config");
}

#[test]
fn line_precise_config_errors() {
    let error = validate_yaml_configuration(
        r#"
plugins:
  non_existant:
    foo: "bar"

telemetry:
  another_non_existant: 3
  "#,
        Expansion::default().unwrap(),
        Mode::NoUpgrade,
    )
    .expect_err("should have resulted in an error");
    insta::assert_snapshot!(error.to_string());
}

#[test]
fn line_precise_config_errors_with_errors_after_first_field() {
    let error = validate_yaml_configuration(
        r#"
supergraph:
  # The socket address and port to listen on
  # Defaults to 127.0.0.1:4000
  listen: 127.0.0.1:4000
  bad: "donotwork"
  another_one: true
        "#,
        Expansion::default().unwrap(),
        Mode::NoUpgrade,
    )
    .expect_err("should have resulted in an error");
    insta::assert_snapshot!(error.to_string());
}

#[test]
fn line_precise_config_errors_bad_type() {
    let error = validate_yaml_configuration(
        r#"
supergraph:
  # The socket address and port to listen on
  # Defaults to 127.0.0.1:4000
  listen: true
        "#,
        Expansion::default().unwrap(),
        Mode::NoUpgrade,
    )
    .expect_err("should have resulted in an error");
    insta::assert_snapshot!(error.to_string());
}

#[test]
fn line_precise_config_errors_with_inline_sequence() {
    let error = validate_yaml_configuration(
        r#"
supergraph:
  # The socket address and port to listen on
  # Defaults to 127.0.0.1:4000
  listen: 127.0.0.1:4000
cors:
  allow_headers: [ Content-Type, 5 ]
        "#,
        Expansion::default().unwrap(),
        Mode::NoUpgrade,
    )
    .expect_err("should have resulted in an error");
    insta::assert_snapshot!(error.to_string());
}

#[test]
fn line_precise_config_errors_with_sequence() {
    let error = validate_yaml_configuration(
        r#"
supergraph:
  # The socket address and port to listen on
  # Defaults to 127.0.0.1:4000
  listen: 127.0.0.1:4000
cors:
  allow_headers:
    - Content-Type
    - 5
        "#,
        Expansion::default().unwrap(),
        Mode::NoUpgrade,
    )
    .expect_err("should have resulted in an error");
    insta::assert_snapshot!(error.to_string());
}

#[test]
fn it_does_not_allow_invalid_cors_headers() {
    let cfg = validate_yaml_configuration(
        r#"
cors:
  allow_credentials: true
  allow_headers: [ "*" ]
        "#,
        Expansion::default().unwrap(),
        Mode::NoUpgrade,
    )
    .expect("should not have resulted in an error");
    let error = cfg
        .cors
        .into_layer()
        .expect_err("should have resulted in an error");
    assert_eq!(
        error,
        "Invalid CORS configuration: Cannot combine `Access-Control-Allow-Credentials: true` with `Access-Control-Allow-Headers: *` in policy"
    );
}

#[test]
fn it_does_not_allow_invalid_cors_methods() {
    let cfg = validate_yaml_configuration(
        r#"
cors:
  allow_credentials: true
  methods: [ GET, "*" ]
        "#,
        Expansion::default().unwrap(),
        Mode::NoUpgrade,
    )
    .expect("should not have resulted in an error");
    let error = cfg
        .cors
        .into_layer()
        .expect_err("should have resulted in an error");
    assert_eq!(
        error,
        "Invalid CORS configuration: Cannot combine `Access-Control-Allow-Credentials: true` with `Access-Control-Allow-Methods: *` in policy"
    );
}

#[test]
fn cors_does_not_allow_invalid_cors_origins() {
    let cfg = validate_yaml_configuration(
        r#"
cors:
  allow_credentials: true
  allow_any_origin: true
        "#,
        Expansion::default().unwrap(),
        Mode::NoUpgrade,
    )
    .expect("should not have resulted in an error");
    let error = cfg
        .cors
        .into_layer()
        .expect_err("should have resulted in an error");
    assert_eq!(
        error,
        "Invalid CORS configuration: Cannot combine `Access-Control-Allow-Credentials: true` with `allow_any_origin: true`"
    );
}

#[test]
fn cors_doesnt_allow_origins_wildcard() {
    let cfg = validate_yaml_configuration(
        r#"
cors:
  policies:
    - origins: ["*"]
        "#,
        Expansion::default().unwrap(),
        Mode::NoUpgrade,
    )
    .expect("should not have resulted in an error");
    let error = cfg
        .cors
        .into_layer()
        .expect_err("should have resulted in an error");
    assert_eq!(
        error,
        "Invalid CORS configuration: use `allow_any_origin: true` to set `Access-Control-Allow-Origin: *`"
    );
}

#[test]
fn validate_project_config_files() {
    #[cfg(not(unix))]
    let filename_matcher = Regex::from_str("((.+[.])?router\\.yaml)|(.+\\.mdx)").unwrap();
    #[cfg(unix)]
    let filename_matcher = Regex::from_str("((.+[.])?router(_unix)?\\.yaml)|(.+\\.mdx)").unwrap();
    #[cfg(not(unix))]
    let embedded_yaml_matcher =
        Regex::from_str(r#"(?ms)```yaml title="router.yaml"(.+?)```"#).unwrap();
    #[cfg(unix)]
    let embedded_yaml_matcher =
        Regex::from_str(r#"(?ms)```yaml title="router(_unix)?.yaml"(.+?)```"#).unwrap();

    fn it(path: &str) -> impl Iterator<Item = DirEntry> + use<> {
        WalkDir::new(path).into_iter().filter_map(|e| e.ok())
    }

    for entry in it(".")
        .chain(it("../examples"))
        .chain(it("../docs"))
        .chain(it("../dockerfiles"))
    {
        if entry
            .path()
            .with_file_name(".skipconfigvalidation")
            .exists()
        {
            continue;
        }
        #[cfg(not(feature = "telemetry_next"))]
        if entry.path().to_string_lossy().contains("telemetry_next") {
            continue;
        }

        let name = entry.file_name().to_string_lossy();
        if filename_matcher.is_match(&name) {
            let config = fs::read_to_string(entry.path()).expect("failed to read file");
            let yamls = if name.ends_with(".mdx") {
                #[cfg(unix)]
                let index = 2usize;
                #[cfg(not(unix))]
                let index = 1usize;
                // Extract yaml from docs
                embedded_yaml_matcher
                    .captures_iter(&config)
                    .map(|i| i.get(index).unwrap().as_str().into())
                    .collect()
            } else {
                vec![config]
            };

            for yaml in yamls {
                let expansion = Expansion::default_builder()
                    .mocked_env_var("DATADOG_AGENT_HOST", "http://example.com")
                    .mocked_env_var("JAEGER_HOST", "http://example.com")
                    .mocked_env_var("JAEGER_USERNAME", "username")
                    .mocked_env_var("JAEGER_PASSWORD", "pass")
                    .mocked_env_var("REDIS_PASSWORD", "pass")
                    .mocked_env_var("ZIPKIN_HOST", "http://example.com")
                    .mocked_env_var("TEST_CONFIG_ENDPOINT", "http://example.com")
                    .mocked_env_var("TEST_CONFIG_COLLECTOR_ENDPOINT", "http://example.com")
                    .mocked_env_var("PARSER_MAX_RECURSION", "500")
                    .mocked_env_var("AWS_ROLE_ARN", "arn:aws:iam::12345678:role/SomeRole")
                    .mocked_env_var("INVALIDATION_SHARED_KEY", "invalidation")
                    .mocked_env_var(
                        "INVALIDATION_SHARED_KEY_PRODUCTS",
                        "invalidation-for-products",
                    )
                    .build()
                    .unwrap();

                if let Err(e) = validate_yaml_configuration(&yaml, expansion, Mode::NoUpgrade) {
                    panic!(
                        "{} configuration error: \n{}",
                        entry.path().to_string_lossy(),
                        e
                    )
                }
            }
        }
    }
}

#[test]
fn it_does_not_leak_env_variable_values() {
    let expansion = Expansion::default_builder()
        .mocked_env_var("TEST_CONFIG_NUMERIC_ENV_UNIQUE", "5")
        .build()
        .unwrap();
    let error = validate_yaml_configuration(
        r#"
supergraph:
  introspection: ${env.TEST_CONFIG_NUMERIC_ENV_UNIQUE:-true}
        "#,
        expansion,
        Mode::NoUpgrade,
    )
    .expect_err("Must have an error because we expect a boolean");
    insta::assert_snapshot!(error.to_string());
}

#[test]
fn line_precise_config_errors_with_inline_sequence_env_expansion() {
    let expansion = Expansion::default_builder()
        .mocked_env_var("TEST_CONFIG_NUMERIC_ENV_UNIQUE", "5")
        .build()
        .unwrap();
    let error = validate_yaml_configuration(
        r#"
supergraph:
  # The socket address and port to listen on
  # Defaults to 127.0.0.1:4000
  listen: 127.0.0.1:4000
cors:
  allow_headers: [ Content-Type, "${env.TEST_CONFIG_NUMERIC_ENV_UNIQUE}" ]
        "#,
        expansion,
        Mode::NoUpgrade,
    )
    .expect_err("should have resulted in an error");
    insta::assert_snapshot!(error.to_string());
}

#[test]
fn line_precise_config_errors_with_sequence_env_expansion() {
    let expansion = Expansion::default_builder()
        .mocked_env_var("env.TEST_CONFIG_NUMERIC_ENV_UNIQUE", "5")
        .build()
        .unwrap();

    let error = validate_yaml_configuration(
        r#"
supergraph:
  # The socket address and port to listen on
  # Defaults to 127.0.0.1:4000
  listen: 127.0.0.1:4000
cors:
  allow_headers:
    - Content-Type
    - "${env.TEST_CONFIG_NUMERIC_ENV_UNIQUE:-true}"
        "#,
        expansion,
        Mode::NoUpgrade,
    )
    .expect_err("should have resulted in an error");
    insta::assert_snapshot!(error.to_string());
}

#[test]
fn line_precise_config_errors_with_errors_after_first_field_env_expansion() {
    #[allow(clippy::literal_string_with_formatting_args)]
    let error = validate_yaml_configuration(
        r#"
supergraph:
  # The socket address and port to listen on
  # Defaults to 127.0.0.1:4000
  listen: 127.0.0.1:4000
  ${TEST_CONFIG_NUMERIC_ENV_UNIQUE:-true}: 5
  another_one: foo
        "#,
        Expansion::default().unwrap(),
        Mode::NoUpgrade,
    )
    .expect_err("should have resulted in an error");
    insta::assert_snapshot!(error.to_string());
}

#[test]
fn expansion_failure_missing_variable() {
    let error = validate_yaml_configuration(
        r#"
supergraph:
  introspection: ${env.TEST_CONFIG_UNKNOWN_WITH_NO_DEFAULT}
        "#,
        Expansion::default().unwrap(),
        Mode::NoUpgrade,
    )
    .expect_err("must have an error because the env variable is unknown");
    insta::assert_snapshot!(error.to_string());
}

#[test]
fn expansion_failure_unknown_mode() {
    let error = validate_yaml_configuration(
        r#"
supergraph:
  introspection: ${unknown.TEST_CONFIG_UNKNOWN_WITH_NO_DEFAULT}
        "#,
        Expansion::builder()
            .prefix("TEST_CONFIG")
            .supported_mode("env")
            .build(),
        Mode::NoUpgrade,
    )
    .expect_err("must have an error because the mode is unknown");
    insta::assert_snapshot!(error.to_string());
}

#[test]
fn expansion_prefixing() {
    validate_yaml_configuration(
        r#"
supergraph:
  introspection: ${env.NEEDS_PREFIX}
        "#,
        Expansion::builder()
            .mocked_env_var("TEST_CONFIG_NEEDS_PREFIX", "true")
            .prefix("TEST_CONFIG")
            .supported_mode("env")
            .build(),
        Mode::NoUpgrade,
    )
    .expect("must have expanded successfully");
}

#[test]
fn expansion_from_file() {
    let mut path = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    path.push("src");
    path.push("configuration");
    path.push("testdata");
    path.push("true.txt");
    let config = validate_yaml_configuration(
        &format!(
            r#"
supergraph:
  introspection: ${{file.{}}}
        "#,
            path.to_string_lossy()
        ),
        Expansion::builder().supported_mode("file").build(),
        Mode::NoUpgrade,
    )
    .expect("must have expanded successfully");

    assert!(config.supergraph.introspection);
}

#[derive(RustEmbed)]
#[folder = "src/configuration/testdata/migrations"]
struct Asset;

#[test]
fn upgrade_old_configuration() {
    for file_name in Asset::iter() {
        if file_name.ends_with(".yaml") {
            let source = Asset::get(&file_name).expect("test file must exist");
            let input = std::str::from_utf8(&source.data)
                .expect("expected utf8")
                .to_string();
            let new_config = crate::configuration::upgrade::upgrade_configuration(
                &serde_yaml::from_str(&input).expect("config must be valid yaml"),
                true,
                upgrade::UpgradeMode::Major,
            )
            .expect("configuration could not be updated");
            let new_config =
                serde_yaml::to_string(&new_config).expect("must be able to serialize config");

            let result = validate_yaml_configuration(
                &new_config,
                Expansion::builder().build(),
                Mode::NoUpgrade,
            );

            match result {
                Ok(_) => {
                    insta::with_settings!({snapshot_suffix => file_name}, {
                        insta::assert_snapshot!(new_config)
                    });
                }
                Err(e) => {
                    panic!(
                        "migrated configuration had validation errors:\n{e}\n\noriginal configuration:\n{input}\n\nmigrated configuration:\n{new_config}"
                    )
                }
            }
        }
    }
}

#[derive(RustEmbed)]
#[folder = "src/configuration/testdata/migrations/minor"]
struct AssetMinor;

#[test]
fn upgrade_old_minor_configuration() {
    for file_name in AssetMinor::iter() {
        if file_name.ends_with(".yaml") {
            let source = AssetMinor::get(&file_name).expect("test file must exist");
            let input = std::str::from_utf8(&source.data)
                .expect("expected utf8")
                .to_string();
            let new_config = crate::configuration::upgrade::upgrade_configuration(
                &serde_yaml::from_str(&input).expect("config must be valid yaml"),
                true,
                upgrade::UpgradeMode::Minor,
            )
            .expect("configuration could not be updated");
            let new_config =
                serde_yaml::to_string(&new_config).expect("must be able to serialize config");

            let result = validate_yaml_configuration(
                &new_config,
                Expansion::builder().build(),
                Mode::NoUpgrade,
            );

            if let Err(err) = result {
                panic!("minor upgrade should not raise errors, but it did for {file_name}: {err:?}")
            }
        }
    }
}

#[test]
fn all_properties_are_documented() {
    // Not using `generate_config_schema` here because of custom configuration.
    // By inlining all sub-schemas we don't have to resolve references.
    let generator = SchemaSettings::draft07()
        .with(|s| {
            s.inline_subschemas = true;
        })
        .into_generator();

    let schema = generator.into_root_schema_for::<Configuration>();
    let schema = serde_json::to_value(schema).expect("must be able to convert the schema to json");

    let mut errors = Vec::new();
    visit_schema("", &schema, &mut errors);
    if !errors.is_empty() {
        panic!(
            "There were errors in the configuration schema: {}",
            errors.join("\n")
        )
    }
}

#[test]
fn default_config_has_defaults() {
    insta::assert_yaml_snapshot!(Configuration::default().validated_yaml);
}

#[rstest::rstest]
#[case("")]
#[case("plugins:")]
fn unusual_configs_validate(#[case] input: &str) {
    validate_yaml_configuration(input, Expansion::builder().build(), Mode::NoUpgrade)
        .expect("should be valid configuration");
}

fn visit_schema(path: &str, schema: &Value, errors: &mut Vec<String>) {
    match schema {
        Value::Array(arr) => {
            for element in arr {
                visit_schema(path, element, errors)
            }
        }
        Value::Object(o) => {
            for (k, v) in o {
                if k.as_str() == "properties" {
                    let properties = v.as_object().expect("properties must be an object");
                    if properties.len() == 1 {
                        // This is probably an enum property
                        continue;
                    }
                    for (k, v) in properties {
                        let path = format!("{path}.{k}");
                        if v.as_object().and_then(|o| o.get("description")).is_none() {
                            // Enum type does not get a description
                            if k != "type" {
                                errors.push(format!("{path} was missing a description"));
                            }
                        }
                        visit_schema(&path, v, errors)
                    }
                } else {
                    let path = format!("{path}.{k}");
                    visit_schema(&path, v, errors)
                }
            }
        }
        _ => {}
    }
}

#[test]
fn test_configuration_validate_and_sanitize() {
    let conf = Configuration::builder()
        .supergraph(Supergraph::builder().path("/g*").build())
        .build()
        .unwrap()
        .validate()
        .unwrap();
    assert_eq!(&conf.supergraph.sanitized_path(), "/g{supergraph_route}");

    let conf = Configuration::builder()
        .supergraph(Supergraph::builder().path("/graphql/g*").build())
        .build()
        .unwrap()
        .validate()
        .unwrap();
    assert_eq!(
        &conf.supergraph.sanitized_path(),
        "/graphql/g{supergraph_route}"
    );

    let conf = Configuration::builder()
        .supergraph(Supergraph::builder().path("/{*rest}").build())
        .build()
        .unwrap()
        .validate()
        .unwrap();
    assert_eq!(&conf.supergraph.sanitized_path(), "/{*rest}");

    let conf = Configuration::builder()
        .supergraph(Supergraph::builder().path("/test").build())
        .build()
        .unwrap()
        .validate()
        .unwrap();
    assert_eq!(&conf.supergraph.sanitized_path(), "/test");

    assert!(
        Configuration::builder()
            .supergraph(Supergraph::builder().path("/*/whatever").build())
            .build()
            .is_err()
    );
}

#[test]
fn load_tls() {
    let mut cert_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    cert_path.push("src");
    cert_path.push("configuration");
    cert_path.push("testdata");
    cert_path.push("server.crt");
    let cert_path = cert_path.to_string_lossy();

    let mut key_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    key_path.push("src");
    key_path.push("configuration");
    key_path.push("testdata");
    key_path.push("server.key");
    let key_path = key_path.to_string_lossy();

    let cfg = validate_yaml_configuration(
        &format!(
            r#"
tls:
  supergraph:
    certificate: ${{file.{cert_path}}}
    certificate_chain: ${{file.{cert_path}}}
    key: ${{file.{key_path}}}
"#,
        ),
        Expansion::builder().supported_mode("file").build(),
        Mode::NoUpgrade,
    )
    .expect("should not have resulted in an error");
    cfg.tls.supergraph.unwrap().tls_config().unwrap();
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
struct TestSubgraphOverride {
    value: Option<u8>,
    subgraph: SubgraphConfiguration<PluginConfig>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(default)]
struct PluginConfig {
    a: bool,
    b: u8,
}

impl Default for PluginConfig {
    fn default() -> Self {
        Self { a: true, b: 0 }
    }
}

#[test]
fn test_subgraph_override() {
    let settings = SchemaSettings::draft2019_09().with(|s| {
        s.inline_subschemas = true;
    });
    let generator = settings.into_generator();
    let schema = generator.into_root_schema_for::<TestSubgraphOverride>();
    insta::assert_json_snapshot!(schema);
}

#[test]
fn test_subgraph_override_json() {
    let first = json!({
        "subgraph": {
            "all": {
                "a": false
            },
            "subgraphs": {
                "products": {
                    "a": true
                }
            }
        }
    });

    let data: TestSubgraphOverride = serde_json::from_value(first).unwrap();
    assert!(!data.subgraph.all.a);
    assert!(data.subgraph.subgraphs.get("products").unwrap().a);

    let second = json!({
        "subgraph": {
            "all": {
                "a": false
            },
            "subgraphs": {
                "products": {
                    "b": 1
                }
            }
        }
    });

    let data: TestSubgraphOverride = serde_json::from_value(second).unwrap();
    assert!(!data.subgraph.all.a);
    // since products did not set the `a` field, it should take the override value from `all`
    assert!(!data.subgraph.subgraphs.get("products").unwrap().a);

    // the default value from `all` should work even if it is parsed after
    let third = json!({
        "subgraph": {
            "subgraphs": {
                "products": {
                    "b": 1
                }
            },
            "all": {
                "a": false
            }
        }
    });

    let data: TestSubgraphOverride = serde_json::from_value(third).unwrap();
    assert!(!data.subgraph.all.a);
    // since products did not set the `a` field, it should take the override value from `all`
    assert!(!data.subgraph.subgraphs.get("products").unwrap().a);
}

#[test]
fn test_deserialize_derive_default() {
    // There are two types of serde defaulting:
    //
    // * container
    // * field
    //
    // Container level defaulting will use an instance of the default implementation and take missing fields from it.
    // Field level defaulting uses either the default implementation or optionally user supplied function to initialize missing fields.
    //
    // When using field level defaulting it is essential that the Default implementation of a struct exactly match the serde annotations.
    //
    // This test checks to ensure that field level defaulting is not used in conjunction with a Default implementation

    // Walk every source file and check that #[derive(Default)] is not used.
    let mut path = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    path.push("src");
    fn it(path: &Path) -> impl Iterator<Item = DirEntry> + use<> {
        WalkDir::new(path).into_iter().filter_map(|e| e.ok())
    }

    // Check for derive where Deserialize is used
    let deserialize_regex =
        Regex::new(r"^\s*#[\s\n]*\[derive\s*\((.*,)?\s*Deserialize\s*(,.*)?\)\s*\]\s*$").unwrap();
    let default_regex =
        Regex::new(r"^\s*#[\s\n]*\[derive\s*\((.*,)?\s*Default\s*(,.*)?\)\s*\]\s*$").unwrap();

    let mut errors = Vec::new();
    for source_file in it(&path).filter(|e| e.file_name().to_string_lossy().ends_with(".rs")) {
        // Read the source file into a vec of lines
        let source = fs::read_to_string(source_file.path()).expect("failed to read file");
        let lines: Vec<&str> = source.lines().collect();
        for (line_number, line) in lines.iter().enumerate() {
            if deserialize_regex.is_match(line) {
                // Get the struct name
                if let Some(struct_name) = find_struct_name(&lines, line_number) {
                    let manual_implementation = format!("impl Default for {struct_name} ");

                    let has_field_level_defaults =
                        has_field_level_serde_defaults(&lines, line_number);
                    let has_manual_default_impl =
                        lines.iter().any(|f| f.contains(&manual_implementation));
                    let has_derive_default_impl = default_regex.is_match(line);

                    if (has_manual_default_impl || has_derive_default_impl)
                        && has_field_level_defaults
                    {
                        errors.push(format!(
                                    "{}:{} struct {} has field level #[serde(default=\"...\")] and also implements Default. Either use #[derive(serde_derive_default::Default)] at the container OR move the defaults into the Default implementation and use #[serde(default)] at the container level",
                                    source_file
                                        .path()
                                        .strip_prefix(path.parent().unwrap().parent().unwrap())
                                        .unwrap()
                                        .to_string_lossy(),
                                    line_number + 1,
                                    struct_name));
                    }
                }
            }
        }
    }

    if !errors.is_empty() {
        panic!("Serde errors found:\n{}", errors.join("\n"));
    }
}

#[test]
fn it_defaults_health_check_configuration() {
    let conf = Configuration::default();
    let addr: ListenAddr = SocketAddr::from_str("127.0.0.1:8088").unwrap().into();

    assert_eq!(conf.health_check.listen, addr);
    assert_eq!(&conf.health_check.path, "/health");

    // Defaults to enabled: true
    assert!(conf.health_check.enabled);
}

#[test]
fn it_sets_custom_health_check_path() {
    let conf = Configuration::builder()
        .health_check(HealthCheck::new(
            None,
            None,
            Some("/healthz".to_string()),
            Default::default(),
        ))
        .build()
        .unwrap();

    assert_eq!(&conf.health_check.path, "/healthz");
}

#[test]
fn it_adds_slash_to_custom_health_check_path_if_missing() {
    let conf = Configuration::builder()
        // NB the missing `/`
        .health_check(HealthCheck::new(
            None,
            None,
            Some("healthz".to_string()),
            Default::default(),
        ))
        .build()
        .unwrap();

    assert_eq!(&conf.health_check.path, "/healthz");
}

#[test]
fn it_processes_batching_subgraph_all_enabled_correctly() {
    let json_config = json!({
        "enabled": true,
        "mode": "batch_http_link",
        "subgraph": {
            "all": {
                "enabled": true
            }
        }
    });

    let config: Batching = serde_json::from_value(json_config).unwrap();

    assert!(config.batch_include("anything"));
}

#[test]
fn it_processes_batching_subgraph_all_disabled_correctly() {
    let json_config = json!({
        "enabled": true,
        "mode": "batch_http_link",
        "subgraph": {
            "all": {
                "enabled": false
            }
        }
    });

    let config: Batching = serde_json::from_value(json_config).unwrap();

    assert!(!config.batch_include("anything"));
}

#[test]
fn it_processes_batching_subgraph_accounts_enabled_correctly() {
    let json_config = json!({
        "enabled": true,
        "mode": "batch_http_link",
        "subgraph": {
            "all": {
                "enabled": false
            },
            "subgraphs": {
                "accounts": {
                    "enabled": true
                }
            }
        }
    });

    let config: Batching = serde_json::from_value(json_config).unwrap();

    assert!(!config.batch_include("anything"));
    assert!(config.batch_include("accounts"));
}

#[test]
fn it_processes_batching_subgraph_accounts_disabled_correctly() {
    let json_config = json!({
        "enabled": true,
        "mode": "batch_http_link",
        "subgraph": {
            "all": {
                "enabled": false
            },
            "subgraphs": {
                "accounts": {
                    "enabled": false
                }
            }
        }
    });

    let config: Batching = serde_json::from_value(json_config).unwrap();

    assert!(!config.batch_include("anything"));
    assert!(!config.batch_include("accounts"));
}

#[test]
fn it_processes_batching_subgraph_accounts_override_disabled_correctly() {
    let json_config = json!({
        "enabled": true,
        "mode": "batch_http_link",
        "subgraph": {
            "all": {
                "enabled": true
            },
            "subgraphs": {
                "accounts": {
                    "enabled": false
                }
            }
        }
    });

    let config: Batching = serde_json::from_value(json_config).unwrap();

    assert!(config.batch_include("anything"));
    assert!(!config.batch_include("accounts"));
}

#[test]
fn it_processes_batching_subgraph_accounts_override_enabled_correctly() {
    let json_config = json!({
        "enabled": true,
        "mode": "batch_http_link",
        "subgraph": {
            "all": {
                "enabled": false
            },
            "subgraphs": {
                "accounts": {
                    "enabled": true
                }
            }
        }
    });

    let config: Batching = serde_json::from_value(json_config).unwrap();

    assert!(!config.batch_include("anything"));
    assert!(config.batch_include("accounts"));
}

#[test]
fn it_processes_unspecified_maximum_batch_limit_correctly() {
    let json_config = json!({
        "enabled": true,
        "mode": "batch_http_link",
    });

    let config: Batching = serde_json::from_value(json_config).unwrap();

    assert_eq!(config.maximum_size, None);
}

#[test]
fn it_processes_specified_maximum_batch_limit_correctly() {
    let json_config = json!({
        "enabled": true,
        "mode": "batch_http_link",
        "maximum_size": 10
    });

    let config: Batching = serde_json::from_value(json_config).unwrap();

    assert_eq!(config.maximum_size, Some(10));
}

#[test]
fn it_includes_default_header_read_timeout_when_server_config_omitted() {
    let json_config = json!({});

    let config: Configuration = serde_json::from_value(json_config).unwrap();

    assert_eq!(
        config.server.http.header_read_timeout,
        Duration::from_secs(10)
    );
}

#[test]
fn it_processes_specified_server_config_correctly() {
    let json_config = json!({
        "server": {
            "http": {
                "header_read_timeout": "30s"
            }
        }
    });

    let config: Configuration = serde_json::from_value(json_config).unwrap();

    assert_eq!(
        config.server.http.header_read_timeout,
        Duration::from_secs(30)
    );
}

fn has_field_level_serde_defaults(lines: &[&str], line_number: usize) -> bool {
    let serde_field_default = Regex::new(
        r#"^\s*#[\s\n]*\[serde\s*\((.*,)?\s*default\s*=\s*"[a-zA-Z0-9_:]+"\s*(,.*)?\)\s*\]\s*$"#,
    )
    .unwrap();
    lines
        .iter()
        .skip(line_number + 1)
        .take(500)
        .take_while(|line| !line.contains('}'))
        .any(|line| serde_field_default.is_match(line))
}

fn find_struct_name(lines: &[&str], line_number: usize) -> Option<String> {
    let struct_enum_union_regex =
        Regex::new(r"^.*(struct|enum|union)\s([a-zA-Z0-9_]+).*$").unwrap();

    lines
        .iter()
        .skip(line_number + 1)
        .take(5)
        .filter_map(|line| {
            struct_enum_union_regex.captures(line).and_then(|c| {
                if c.get(1).unwrap().as_str() == "struct" {
                    Some(c.get(2).unwrap().as_str().to_string())
                } else {
                    None
                }
            })
        })
        .next()
}

#[test]
fn it_prevents_enablement_of_both_subgraph_caching_plugins() {
    let make_config = |response_cache_enabled, entity_cache_enabled| {
        let mut config = json!({});
        if let Some(enabled) = response_cache_enabled {
            config.as_object_mut().unwrap().insert(
                "preview_response_cache".to_string(),
                json!({"enabled": enabled}),
            );
        }
        if let Some(enabled) = entity_cache_enabled {
            config.as_object_mut().unwrap().insert(
                "preview_entity_cache".to_string(),
                json!({"enabled": enabled}),
            );
        }
        config
    };

    let _: Configuration =
        serde_json::from_value(make_config(None, None)).expect("neither plugin configured");

    let _: Configuration = serde_json::from_value(make_config(Some(true), None))
        .expect("response cache plugin configured");

    let _: Configuration = serde_json::from_value(make_config(Some(true), Some(false)))
        .expect("response cache plugin configured");

    let _: Configuration = serde_json::from_value(make_config(None, Some(true)))
        .expect("entity cache plugin configured");

    let _: Configuration = serde_json::from_value(make_config(Some(false), Some(true)))
        .expect("entity cache plugin configured");

    let config_result: Result<Configuration, _> =
        serde_json::from_value(make_config(Some(true), Some(true)));
    config_result.expect_err("both plugins configured");
}
