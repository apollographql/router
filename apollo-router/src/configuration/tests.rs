use std::collections::HashMap;
use std::fs;
use std::marker::PhantomData;
use std::path::PathBuf;

use http::Uri;
#[cfg(unix)]
use insta::assert_json_snapshot;
use regex::Regex;
use rust_embed::RustEmbed;
#[cfg(unix)]
use schemars::gen::SchemaSettings;
use serde::de;
use serde::de::DeserializeOwned;
use serde::de::MapAccess;
use serde::de::Visitor;
use walkdir::DirEntry;
use walkdir::WalkDir;

use super::schema::validate_yaml_configuration;
use super::*;
use crate::error::SchemaError;

#[cfg(unix)]
#[test]
fn schema_generation() {
    let settings = SchemaSettings::draft2019_09().with(|s| {
        s.option_nullable = true;
        s.option_add_null_type = false;
        s.inline_subschemas = true;
    });
    let gen = settings.into_generator();
    let schema = gen.into_root_schema_for::<Configuration>();
    assert_json_snapshot!(&schema)
}

#[test]
fn routing_url_in_schema() {
    let schema = r#"
        schema
          @core(feature: "https://specs.apollo.dev/core/v0.1"),
          @core(feature: "https://specs.apollo.dev/join/v0.1")
        {
          query: Query
        }

        type Query {
          me: String
        }

        directive @core(feature: String!) repeatable on SCHEMA

        directive @join__graph(name: String!, url: String!) on ENUM_VALUE
        enum join__Graph {
          ACCOUNTS @join__graph(name: "accounts" url: "http://localhost:4001/graphql")
          INVENTORY @join__graph(name: "inventory" url: "http://localhost:4002/graphql")
          PRODUCTS @join__graph(name: "products" url: "http://localhost:4003/graphql")
          REVIEWS @join__graph(name: "reviews" url: "http://localhost:4004/graphql")
        }
        "#;
    let schema = crate::spec::Schema::parse(schema, &Default::default()).unwrap();

    let subgraphs: HashMap<&String, &Uri> = schema.subgraphs().collect();

    // if no configuration override, use the URL from the supergraph
    assert_eq!(
        subgraphs.get(&"accounts".to_string()).unwrap().to_string(),
        "http://localhost:4001/graphql"
    );
    // if both configuration and schema specify a non empty URL, the configuration wins
    // this should show a warning in logs
    assert_eq!(
        subgraphs.get(&"inventory".to_string()).unwrap().to_string(),
        "http://localhost:4002/graphql"
    );
    // if the configuration has a non empty routing URL, and the supergraph
    // has an empty one, the configuration wins
    assert_eq!(
        subgraphs.get(&"products".to_string()).unwrap().to_string(),
        "http://localhost:4003/graphql"
    );

    assert_eq!(
        subgraphs.get(&"reviews".to_string()).unwrap().to_string(),
        "http://localhost:4004/graphql"
    );
}

#[test]
fn missing_subgraph_url() {
    let schema_error = r#"
        schema
          @core(feature: "https://specs.apollo.dev/core/v0.1"),
          @core(feature: "https://specs.apollo.dev/join/v0.1")
        {
          query: Query
        }

        type Query {
          me: String
        }

        directive @core(feature: String!) repeatable on SCHEMA

        directive @join__graph(name: String!, url: String!) on ENUM_VALUE

        enum join__Graph {
          ACCOUNTS @join__graph(name: "accounts" url: "http://localhost:4001/graphql")
          INVENTORY @join__graph(name: "inventory" url: "http://localhost:4002/graphql")
          PRODUCTS @join__graph(name: "products" url: "http://localhost:4003/graphql")
          REVIEWS @join__graph(name: "reviews" url: "")
        }"#;
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

    assert_eq!(
        ["https://studio.apollographql.com"],
        cors.origins.as_slice()
    );
    assert!(
        !cors.allow_any_origin,
        "Allow any origin should be disabled by default"
    );
    assert!(cors.allow_headers.is_empty());

    assert!(
        cors.match_origins.is_none(),
        "No origin regex list should be present by default"
    );
}

#[test]
fn bad_graphql_path_configuration_without_slash() {
    let error = Configuration::fake_builder()
        .supergraph(Supergraph::fake_builder().path("test").build())
        .build()
        .unwrap_err();
    assert_eq!(error.to_string(), String::from("invalid 'server.graphql_path' configuration: 'test' is invalid, it must be an absolute path and start with '/', you should try with '/test'"));
}

#[test]
fn bad_graphql_path_configuration_with_wildcard_as_prefix() {
    let error = Configuration::fake_builder()
        .supergraph(Supergraph::fake_builder().path("/*/test").build())
        .build()
        .unwrap_err();

    assert_eq!(error.to_string(), String::from("invalid 'server.graphql_path' configuration: '/*/test' is invalid, if you need to set a path like '/*/graphql' then specify it as a path parameter with a name, for example '/:my_project_key/graphql'"));
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
    assert_eq!(error, "Invalid CORS configuration: Cannot combine `Access-Control-Allow-Credentials: true` with `Access-Control-Allow-Headers: *`");
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
    assert_eq!(error, "Invalid CORS configuration: Cannot combine `Access-Control-Allow-Credentials: true` with `Access-Control-Allow-Methods: *`");
}

#[test]
fn it_does_not_allow_invalid_cors_origins() {
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
    assert_eq!(error, "Invalid CORS configuration: Cannot combine `Access-Control-Allow-Credentials: true` with `Access-Control-Allow-Origin: *`");
}

#[test]
fn validate_project_config_files() {
    std::env::set_var("JAEGER_USERNAME", "username");
    std::env::set_var("JAEGER_PASSWORD", "pass");
    std::env::set_var("TEST_CONFIG_ENDPOINT", "http://example.com");
    std::env::set_var("TEST_CONFIG_COLLECTOR_ENDPOINT", "http://example.com");

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

    fn it(path: &str) -> impl Iterator<Item = DirEntry> {
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
                if let Err(e) = validate_yaml_configuration(
                    &yaml,
                    Expansion::default().unwrap(),
                    Mode::NoUpgrade,
                ) {
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
    std::env::set_var("TEST_CONFIG_NUMERIC_ENV_UNIQUE", "5");
    let error = validate_yaml_configuration(
        r#"
supergraph:
  introspection: ${env.TEST_CONFIG_NUMERIC_ENV_UNIQUE:-true}
        "#,
        Expansion::default().unwrap(),
        Mode::NoUpgrade,
    )
    .expect_err("Must have an error because we expect a boolean");
    insta::assert_snapshot!(error.to_string());
}

#[test]
fn line_precise_config_errors_with_inline_sequence_env_expansion() {
    std::env::set_var("TEST_CONFIG_NUMERIC_ENV_UNIQUE", "5");
    let error = validate_yaml_configuration(
        r#"
supergraph:
  # The socket address and port to listen on
  # Defaults to 127.0.0.1:4000
  listen: 127.0.0.1:4000
cors:
  allow_headers: [ Content-Type, "${env.TEST_CONFIG_NUMERIC_ENV_UNIQUE}" ]
        "#,
        Expansion::default().unwrap(),
        Mode::NoUpgrade,
    )
    .expect_err("should have resulted in an error");
    insta::assert_snapshot!(error.to_string());
}

#[test]
fn line_precise_config_errors_with_sequence_env_expansion() {
    std::env::set_var("env.TEST_CONFIG_NUMERIC_ENV_UNIQUE", "5");

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
        Expansion::default().unwrap(),
        Mode::NoUpgrade,
    )
    .expect_err("should have resulted in an error");
    insta::assert_snapshot!(error.to_string());
}

#[test]
fn line_precise_config_errors_with_errors_after_first_field_env_expansion() {
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
    std::env::set_var("TEST_CONFIG_NEEDS_PREFIX", "true");
    validate_yaml_configuration(
        r#"
supergraph:
  introspection: ${env.NEEDS_PREFIX}
        "#,
        Expansion::builder()
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
                    panic!("migrated configuration had validation errors:\n{e}\n\noriginal configuration:\n{input}\n\nmigrated configuration:\n{new_config}")
                }
            }
        }
    }
}

#[test]
fn all_properties_are_documented() {
    let schema = serde_json::to_value(&generate_config_schema())
        .expect("must be able to convert the schema to json");

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
                    for (k, v) in properties {
                        let path = format!("{path}.{k}");
                        if v.as_object().and_then(|o| o.get("description")).is_none() {
                            errors.push(format!("{path} was missing a description"));
                        }
                        visit_schema(&path, v, errors)
                    }
                } else {
                    visit_schema(path, v, errors)
                }
            }
        }
        _ => {}
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
struct TestSubgraphOverride {
    value: Option<u8>,
    subgraph: Subgraph<PluginConfig>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize, JsonSchema)]
struct PluginConfig {
    #[serde(default = "set_true")]
    a: bool,
    #[serde(default)]
    b: u8,
}

fn set_true() -> bool {
    true
}
#[derive(Debug, Clone, Serialize, JsonSchema)]
struct Subgraph<T>
where
    T: std::fmt::Debug + Default + Clone + Serialize + JsonSchema,
{
    /// options applying to all subgraphs
    #[serde(default)]
    pub(crate) all: T,
    /// per subgraph options
    #[serde(default)]
    pub(crate) subgraphs: HashMap<String, T>,
}

impl<T> Subgraph<T>
where
    T: std::fmt::Debug + Default + Clone + Serialize + JsonSchema,
{
    #[allow(dead_code)]
    fn get(&self, subgraph_name: &str) -> &T {
        self.subgraphs.get(subgraph_name).unwrap_or(&self.all)
    }
}

impl<'de, T> Deserialize<'de> for Subgraph<T>
where
    T: DeserializeOwned,
    T: std::fmt::Debug + Default + Clone + Serialize + JsonSchema,
{
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        deserializer.deserialize_map(SubgraphVisitor { t: PhantomData })
    }
}

struct SubgraphVisitor<T> {
    t: PhantomData<T>,
}

impl<'de, T> Visitor<'de> for SubgraphVisitor<T>
where
    T: DeserializeOwned,
    T: std::fmt::Debug + Default + Clone + Serialize + JsonSchema,
{
    type Value = Subgraph<T>;

    fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
        formatter.write_str("struct Subgraph")
    }

    fn visit_map<V>(self, mut map: V) -> Result<Subgraph<T>, V::Error>
    where
        V: MapAccess<'de>,
    {
        let mut all: Option<serde_yaml::Mapping> = None;
        let mut parsed_subgraphs: Option<HashMap<String, serde_yaml::Mapping>> = None;
        while let Some(key) = map.next_key()? {
            match key {
                Field::All => {
                    if all.is_some() {
                        return Err(de::Error::duplicate_field("all"));
                    }
                    all = Some(map.next_value()?);
                }
                Field::Subgraphs => {
                    if parsed_subgraphs.is_some() {
                        return Err(de::Error::duplicate_field("subgraphs"));
                    }
                    parsed_subgraphs = Some(map.next_value()?);
                }
            }
        }

        let mut subgraphs = HashMap::new();
        if let Some(subs) = parsed_subgraphs {
            for (subgraph_name, parsed_value) in subs {
                // if `all` was set, use the fields it set, then overwrite with the subgraph
                // specific values
                let value = if let Some(mut value) = all.clone() {
                    for (k, v) in parsed_value {
                        value.insert(k, v);
                    }

                    value
                } else {
                    parsed_value
                };

                let config = serde_yaml::from_value(serde_yaml::Value::Mapping(value))
                    .map_err(de::Error::custom)?;
                subgraphs.insert(subgraph_name, config);
            }
        }

        let all = serde_yaml::from_value(serde_yaml::Value::Mapping(all.unwrap_or_default()))
            .map_err(de::Error::custom)?;

        Ok(Subgraph { all, subgraphs })
    }
}

enum Field {
    All,
    Subgraphs,
}

impl<'de> Deserialize<'de> for Field {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        deserializer.deserialize_identifier(FieldVisitor)
    }
}

struct FieldVisitor;

impl<'de> Visitor<'de> for FieldVisitor {
    type Value = Field;

    fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
        formatter.write_str("`secs` or `nanos`")
    }

    fn visit_str<E>(self, value: &str) -> Result<Field, E>
    where
        E: de::Error,
    {
        match value {
            "all" => Ok(Field::All),
            "subgraphs" => Ok(Field::Subgraphs),
            _ => Err(de::Error::unknown_field(value, FIELDS)),
        }
    }
}

const FIELDS: &[&str] = &["all", "subgraphs"];

#[test]
fn test_configuration_validate_and_sanitize() {
    let conf = Configuration::builder()
        .supergraph(Supergraph::builder().path("/g*").build())
        .build()
        .unwrap()
        .validate()
        .unwrap();
    assert_eq!(&conf.supergraph.sanitized_path(), "/g:supergraph_route");

    let conf = Configuration::builder()
        .supergraph(Supergraph::builder().path("/graphql/g*").build())
        .build()
        .unwrap()
        .validate()
        .unwrap();
    assert_eq!(
        &conf.supergraph.sanitized_path(),
        "/graphql/g:supergraph_route"
    );

    let conf = Configuration::builder()
        .supergraph(Supergraph::builder().path("/*").build())
        .build()
        .unwrap()
        .validate()
        .unwrap();
    assert_eq!(&conf.supergraph.sanitized_path(), "/*router_extra_path");

    let conf = Configuration::builder()
        .supergraph(Supergraph::builder().path("/test").build())
        .build()
        .unwrap()
        .validate()
        .unwrap();
    assert_eq!(&conf.supergraph.sanitized_path(), "/test");

    assert!(Configuration::builder()
        .supergraph(Supergraph::builder().path("/*/whatever").build())
        .build()
        .is_err());
}

#[test]
fn test_subgraph_override() {
    let settings = SchemaSettings::draft2019_09().with(|s| {
        s.option_nullable = true;
        s.option_add_null_type = false;
        s.inline_subschemas = true;
    });
    let gen = settings.into_generator();
    let schema = gen.into_root_schema_for::<TestSubgraphOverride>();
    println!("{}", serde_json::to_string_pretty(&schema).unwrap());
    //panic!()
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
                    "b": 0
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
                    "b": 0
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
    panic!()
}
