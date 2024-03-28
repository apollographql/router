// tonic does not derive `Eq` for the gRPC message types, which causes a warning from Clippy. The
// current suggestion is to explicitly allow the lint in the module that imports the protos.
// Read more: https://github.com/hyperium/tonic/issues/1056
#![allow(clippy::derive_partial_eq_without_eq)]

use std::collections::HashMap;
use std::fmt::Display;
use std::fmt::Formatter;
use std::str::FromStr;
use std::time::Duration;
use std::time::SystemTime;
use std::time::UNIX_EPOCH;

use apollo_compiler::ast::Definition;
use apollo_compiler::schema::Directive;
use apollo_compiler::Node;
use buildstructor::Builder;
use displaydoc::Display;
use itertools::Itertools;
use jsonwebtoken::decode;
use jsonwebtoken::jwk::JwkSet;
use jsonwebtoken::DecodingKey;
use jsonwebtoken::Validation;
use once_cell::sync::OnceCell;
use regex::Regex;
use serde::Deserialize;
use serde::Deserializer;
use serde::Serialize;
use serde_json::Value;
use thiserror::Error;
use url::Url;

use crate::plugins::authentication::convert_key_algorithm;
use crate::spec::LINK_AS_ARGUMENT;
use crate::spec::LINK_DIRECTIVE_NAME;
use crate::spec::LINK_URL_ARGUMENT;
use crate::Configuration;

pub(crate) const LICENSE_EXPIRED_URL: &str = "https://go.apollo.dev/o/elp";
pub(crate) const LICENSE_EXPIRED_SHORT_MESSAGE: &str =
    "Apollo license expired https://go.apollo.dev/o/elp";

pub(crate) const APOLLO_ROUTER_LICENSE_EXPIRED: &str = "APOLLO_ROUTER_LICENSE_EXPIRED";

static JWKS: OnceCell<JwkSet> = OnceCell::new();

#[derive(Error, Display, Debug)]
pub enum Error {
    /// invalid license: {0}
    InvalidLicense(jsonwebtoken::errors::Error),
}

#[derive(Debug, Clone, Serialize, Deserialize, Eq, PartialEq, Hash)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub(crate) enum Audience {
    SelfHosted,
    Cloud,
    Offline,
}

#[derive(Debug, Clone, Serialize, Deserialize, Eq, PartialEq)]
#[serde(untagged)]
pub(crate) enum OneOrMany<T> {
    One(T),
    Many(Vec<T>),
}

#[derive(Debug, Clone, Serialize, Deserialize, Eq, PartialEq)]
pub(crate) struct Claims {
    pub(crate) iss: String,
    pub(crate) sub: String,
    pub(crate) aud: OneOrMany<Audience>,
    #[serde(deserialize_with = "deserialize_epoch_seconds", rename = "warnAt")]
    pub(crate) warn_at: SystemTime,
    #[serde(deserialize_with = "deserialize_epoch_seconds", rename = "haltAt")]
    pub(crate) halt_at: SystemTime,
}

fn deserialize_epoch_seconds<'de, D>(deserializer: D) -> Result<SystemTime, D::Error>
where
    D: Deserializer<'de>,
{
    let seconds = i32::deserialize(deserializer)?;
    Ok(UNIX_EPOCH + Duration::from_secs(seconds as u64))
}

#[derive(Debug)]
pub(crate) struct LicenseEnforcementReport {
    restricted_config_in_use: Vec<ConfigurationRestriction>,
    restricted_schema_in_use: Vec<SchemaViolation>,
}

#[derive(Debug)]
struct ParsedLinkSpec {
    spec_name: String,
    version: semver::Version,
    spec_url: String,
    imported_as: Option<String>,
    url: String,
}

impl ParsedLinkSpec {
    fn from_link_directive(
        link_directive: &Node<Directive>,
    ) -> Option<Result<ParsedLinkSpec, url::ParseError>> {
        link_directive
            .argument_by_name(LINK_URL_ARGUMENT)
            .and_then(|value| {
                let url_string = value.as_str();
                let parsed_url = Url::parse(url_string.unwrap_or_default()).ok()?;

                let mut segments = parsed_url.path_segments()?;
                let spec_name = segments.next()?.to_string();
                let spec_url = format!(
                    "{}://{}/{}",
                    parsed_url.scheme(),
                    parsed_url.host()?,
                    spec_name
                );
                let version_string = segments.next()?.strip_prefix('v')?;
                let parsed_version =
                    semver::Version::parse(format!("{}.0", &version_string).as_str()).ok()?;

                let imported_as = link_directive
                    .argument_by_name(LINK_AS_ARGUMENT)
                    .map(|as_arg| as_arg.as_str().unwrap_or_default().to_string());

                Some(Ok(ParsedLinkSpec {
                    spec_name,
                    spec_url,
                    version: parsed_version,
                    imported_as,
                    url: url_string?.to_string(),
                }))
            })
    }

    // Implements directive name construction logic for link directives.
    // 1. If the link directive has an `as` argument, use that as the prefix.
    // 2. If the link directive's spec name is the same as the default name, use the default name with no prefix.
    // 3. Otherwise, use the spec name as the prefix.
    fn directive_name(&self, default_name: &str) -> String {
        if let Some(imported_as) = &self.imported_as {
            format!("{}__{}", imported_as, default_name)
        } else if self.spec_name == default_name {
            default_name.to_string()
        } else {
            format!("{}__{}", self.spec_name, default_name)
        }
    }
}

impl LicenseEnforcementReport {
    pub(crate) fn uses_restricted_features(&self) -> bool {
        !self.restricted_config_in_use.is_empty() || !self.restricted_schema_in_use.is_empty()
    }

    pub(crate) fn build(
        configuration: &Configuration,
        schema: &apollo_compiler::ast::Document,
    ) -> LicenseEnforcementReport {
        LicenseEnforcementReport {
            restricted_config_in_use: Self::validate_configuration(
                configuration,
                &Self::configuration_restrictions(),
            ),
            restricted_schema_in_use: Self::validate_schema(schema, &Self::schema_restrictions()),
        }
    }

    fn validate_configuration(
        configuration: &Configuration,
        configuration_restrictions: &Vec<ConfigurationRestriction>,
    ) -> Vec<ConfigurationRestriction> {
        let mut selector = jsonpath_lib::selector(
            configuration
                .validated_yaml
                .as_ref()
                .unwrap_or(&Value::Null),
        );
        let mut configuration_violations = Vec::new();
        for restriction in configuration_restrictions {
            if let Some(value) = selector(&restriction.path)
                .expect("path on restriction was not valid")
                .first()
            {
                if let Some(restriction_value) = &restriction.value {
                    if *value == restriction_value {
                        configuration_violations.push(restriction.clone());
                    }
                } else {
                    configuration_violations.push(restriction.clone());
                }
            }
        }
        configuration_violations
    }

    fn validate_schema(
        schema: &apollo_compiler::ast::Document,
        schema_restrictions: &Vec<SchemaRestriction>,
    ) -> Vec<SchemaViolation> {
        let link_specs = schema
            .definitions
            .iter()
            .filter_map(|def| def.as_schema_definition())
            .flat_map(|def| def.directives.get_all(LINK_DIRECTIVE_NAME))
            .filter_map(|link| {
                ParsedLinkSpec::from_link_directive(link).map(|maybe_spec| {
                    maybe_spec.ok().map(|spec| (spec.spec_url.to_owned(), spec))
                })?
            })
            .collect::<HashMap<_, _>>();

        let mut schema_violations: Vec<SchemaViolation> = Vec::new();

        for subgraph_url in schema
            .definitions
            .iter()
            .filter_map(|def| def.as_enum_type_definition())
            .filter(|def| def.name == "join__Graph")
            .flat_map(|def| def.values.iter())
            .flat_map(|val| val.directives.iter())
            .filter(|d| d.name == "join__graph")
            .filter_map(|dir| (dir.arguments.iter().find(|arg| arg.name == "url")))
            .filter_map(|arg| arg.value.as_str())
        {
            if subgraph_url.starts_with("unix://") {
                schema_violations.push(SchemaViolation::DirectiveArgument {
                    url: "https://specs.apollo.dev/join/v0.3".to_string(),
                    name: "join__Graph".to_string(),
                    argument: "url".to_string(),
                    explanation: "Unix socket support for subgraph requests is restricted to Enterprise users".to_string(),
                });
            }
        }

        for restriction in schema_restrictions {
            match restriction {
                SchemaRestriction::Spec {
                    spec_url,
                    name,
                    version_req,
                } => {
                    if let Some(link_spec) = link_specs.get(spec_url) {
                        if version_req.matches(&link_spec.version) {
                            schema_violations.push(SchemaViolation::Spec {
                                url: link_spec.url.to_string(),
                                name: name.to_string(),
                            });
                        }
                    }
                }
                SchemaRestriction::DirectiveArgument {
                    spec_url,
                    name,
                    version_req,
                    argument,
                    explanation,
                } => {
                    if let Some(link_spec) = link_specs.get(spec_url) {
                        if version_req.matches(&link_spec.version) {
                            let directive_name = link_spec.directive_name(name);
                            if schema
                                .definitions
                                .iter()
                                .flat_map(|def| match def {
                                    // To traverse additional directive locations, add match arms for the respective definition types required.
                                    // As of writing this, this is only implemented for finding usages of progressive override on object type fields, but it can be extended to other directive locations trivially.
                                    Definition::ObjectTypeDefinition(object_type_def) => {
                                        let directives_on_object =
                                            object_type_def.directives.get_all(&directive_name);
                                        let directives_on_fields =
                                            object_type_def.fields.iter().flat_map(|field| {
                                                field.directives.get_all(&directive_name)
                                            });

                                        directives_on_object
                                            .chain(directives_on_fields)
                                            .collect::<Vec<_>>()
                                    }
                                    _ => vec![],
                                })
                                .any(|directive| directive.argument_by_name(argument).is_some())
                            {
                                schema_violations.push(SchemaViolation::DirectiveArgument {
                                    url: link_spec.url.to_string(),
                                    name: directive_name.to_string(),
                                    argument: argument.to_string(),
                                    explanation: explanation.to_string(),
                                });
                            }
                        }
                    }
                }
            }
        }

        schema_violations
    }

    fn configuration_restrictions() -> Vec<ConfigurationRestriction> {
        vec![
            ConfigurationRestriction::builder()
                .path("$.plugins.['experimental.restricted'].enabled")
                .value(true)
                .name("Restricted")
                .build(),
            ConfigurationRestriction::builder()
                .path("$.authentication.router")
                .name("Authentication plugin")
                .build(),
            ConfigurationRestriction::builder()
                .path("$.authorization.directives")
                .name("Authorization directives")
                .build(),
            ConfigurationRestriction::builder()
                .path("$.coprocessor")
                .name("Coprocessor plugin")
                .build(),
            ConfigurationRestriction::builder()
                .path("$.supergraph.query_planning.cache.redis")
                .name("Query plan caching")
                .build(),
            ConfigurationRestriction::builder()
                .path("$.apq.router.cache.redis")
                .name("APQ caching")
                .build(),
            ConfigurationRestriction::builder()
                .path("$.preview_entity_cache.enabled")
                .value(true)
                .name("Subgraph entity caching")
                .build(),
            ConfigurationRestriction::builder()
                .path("$.subscription.enabled")
                .value(true)
                .name("Federated subscriptions")
                .build(),
            // Per-operation limits are restricted but parser limits like `parser_max_recursion`
            // where the Router only configures apollo-rs are not.
            ConfigurationRestriction::builder()
                .path("$.limits.max_depth")
                .name("Operation depth limiting")
                .build(),
            ConfigurationRestriction::builder()
                .path("$.limits.max_height")
                .name("Operation height limiting")
                .build(),
            ConfigurationRestriction::builder()
                .path("$.limits.max_root_fields")
                .name("Operation root fields limiting")
                .build(),
            ConfigurationRestriction::builder()
                .path("$.limits.max_aliases")
                .name("Operation aliases limiting")
                .build(),
            ConfigurationRestriction::builder()
                .path("$.persisted_queries")
                .name("Persisted queries")
                .build(),
            ConfigurationRestriction::builder()
                .path("$.telemetry..spans.router")
                .name("Advanced telemetry")
                .build(),
            ConfigurationRestriction::builder()
                .path("$.telemetry..spans.supergraph")
                .name("Advanced telemetry")
                .build(),
            ConfigurationRestriction::builder()
                .path("$.telemetry..spans.subgraph")
                .name("Advanced telemetry")
                .build(),
            ConfigurationRestriction::builder()
                .path("$.telemetry..events")
                .name("Advanced telemetry")
                .build(),
            ConfigurationRestriction::builder()
                .path("$.telemetry..instruments")
                .name("Advanced telemetry")
                .build(),
            ConfigurationRestriction::builder()
                .path("$.preview_file_uploads")
                .name("File uploads plugin")
                .build(),
            ConfigurationRestriction::builder()
                .path("$.batching")
                .name("Batching support")
                .build(),
            ConfigurationRestriction::builder()
                .path("$.experimental_demand_control")
                .name("Demand control plugin")
                .build(),
        ]
    }

    fn schema_restrictions() -> Vec<SchemaRestriction> {
        vec![
            SchemaRestriction::Spec {
                name: "authenticated".to_string(),
                spec_url: "https://specs.apollo.dev/authenticated".to_string(),
                version_req: semver::VersionReq {
                    comparators: vec![semver::Comparator {
                        op: semver::Op::Exact,
                        major: 0,
                        minor: 1.into(),
                        patch: 0.into(),
                        pre: semver::Prerelease::EMPTY,
                    }],
                },
            },
            SchemaRestriction::Spec {
                name: "requiresScopes".to_string(),
                spec_url: "https://specs.apollo.dev/requiresScopes".to_string(),
                version_req: semver::VersionReq {
                    comparators: vec![semver::Comparator {
                        op: semver::Op::Exact,
                        major: 0,
                        minor: 1.into(),
                        patch: 0.into(),
                        pre: semver::Prerelease::EMPTY,
                    }],
                },
            },
            SchemaRestriction::DirectiveArgument {
                name: "field".to_string(),
                argument: "overrideLabel".to_string(),
                spec_url: "https://specs.apollo.dev/join".to_string(),
                version_req: semver::VersionReq {
                    comparators: vec![semver::Comparator {
                        op: semver::Op::GreaterEq,
                        major: 0,
                        minor: 4.into(),
                        patch: 0.into(),
                        pre: semver::Prerelease::EMPTY,
                    }],
                },
                explanation: "The `overrideLabel` argument on the join spec's @field directive is restricted to Enterprise users. This argument exists in your supergraph as a result of using the `@override` directive with the `label` argument in one or more of your subgraphs.".to_string()
            },
        ]
    }
}

impl Display for LicenseEnforcementReport {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        if !self.restricted_config_in_use.is_empty() {
            let restricted_config = self
                .restricted_config_in_use
                .iter()
                .map(|v| format!("* {}\n  {}", v.name, v.path.replace("$.", ".")))
                .join("\n\n");
            write!(f, "Configuration yaml:\n{restricted_config}")?;

            if !self.restricted_schema_in_use.is_empty() {
                writeln!(f)?;
            }
        }

        if !self.restricted_schema_in_use.is_empty() {
            let restricted_schema = self
                .restricted_schema_in_use
                .iter()
                .map(|v| v.to_string())
                .join("\n\n");

            write!(f, "Schema features:\n{restricted_schema}")?
        }

        Ok(())
    }
}

/// License controls availability of certain features of the Router. It must be constructed from a base64 encoded JWT
/// This API experimental and is subject to change outside of semver.
#[derive(Debug, Clone, Default)]
pub struct License {
    pub(crate) claims: Option<Claims>,
}

/// Licenses are converted into a stream of license states by the expander
#[derive(Debug, Copy, Clone, Eq, PartialEq, Default, Display)]
pub(crate) enum LicenseState {
    /// licensed
    Licensed,
    /// warn
    LicensedWarn,
    /// halt
    LicensedHalt,

    /// unlicensed
    #[default]
    Unlicensed,
}

impl Display for License {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        if let Some(claims) = &self.claims {
            write!(
                f,
                "{}",
                serde_json::to_string(claims)
                    .unwrap_or_else(|_| "claim serialization error".to_string())
            )
        } else {
            write!(f, "no license")
        }
    }
}

impl FromStr for License {
    type Err = Error;

    fn from_str(jwt: &str) -> Result<Self, Self::Err> {
        Self::jwks()
            .keys
            .iter()
            .map(|jwk| {
                // Set up the validation for the JWT.
                // We don't require exp as we are only interested in haltAt and warnAt
                let mut validation = Validation::new(
                    convert_key_algorithm(
                        jwk.common
                            .key_algorithm
                            .expect("alg is required on all keys in router.jwks.json"),
                    )
                    .expect("only signing algorithms are used"),
                );
                validation.validate_exp = false;
                validation.set_required_spec_claims(&["iss", "sub", "aud", "warnAt", "haltAt"]);
                validation.set_issuer(&["https://www.apollographql.com/"]);
                validation.set_audience(&["CLOUD", "SELF_HOSTED", "OFFLINE"]);

                decode::<Claims>(
                    jwt.trim(),
                    &DecodingKey::from_jwk(jwk).expect("router.jwks.json must be valid"),
                    &validation,
                )
                .map_err(Error::InvalidLicense)
                .map(|r| License {
                    claims: Some(r.claims),
                })
            })
            .find_or_last(|r| r.is_ok())
            .transpose()
            .map(|e| {
                let e = e.unwrap_or_default();
                tracing::debug!("decoded license {jwt}->{e}");
                e
            })
    }
}

/// An individual check for the router.yaml.
#[derive(Builder, Clone, Debug, Serialize, Deserialize)]
pub(crate) struct ConfigurationRestriction {
    name: String,
    path: String,
    value: Option<Value>,
}

// An individual check for the supergraph schema
// #[derive(Builder, Clone, Debug, Serialize, Deserialize)]
// pub(crate) struct SchemaRestriction {
//     name: String,
//     url: String,
// }

/// An individual check for the supergraph schema
#[derive(Clone, Debug)]
pub(crate) enum SchemaRestriction {
    Spec {
        spec_url: String,
        name: String,
        version_req: semver::VersionReq,
    },
    // Note: this restriction is currently only traverses directives belonging
    // to object types and their fields. See note in `schema_restrictions` loop
    // for where to update if this restriction is to be enforced on other
    // directives.
    DirectiveArgument {
        spec_url: String,
        name: String,
        version_req: semver::VersionReq,
        argument: String,
        explanation: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) enum SchemaViolation {
    Spec {
        url: String,
        name: String,
    },
    DirectiveArgument {
        url: String,
        name: String,
        argument: String,
        explanation: String,
    },
}

impl Display for SchemaViolation {
    fn fmt(&self, f: &mut Formatter) -> std::fmt::Result {
        match self {
            SchemaViolation::Spec { name, url } => {
                write!(f, "* @{}\n  {}", name, url)
            }
            SchemaViolation::DirectiveArgument {
                name,
                url,
                argument,
                explanation,
            } => {
                write!(f, "* @{}.{}\n  {}\n\n{}", name, argument, url, explanation)
            }
        }
    }
}

impl License {
    pub(crate) fn jwks() -> &'static JwkSet {
        JWKS.get_or_init(|| {
            // Strip the comments from the top of the file.
            let re = Regex::new("(?m)^//.*$").expect("regex must be valid");
            let jwks = re.replace(include_str!("license.jwks.json"), "");
            serde_json::from_str::<JwkSet>(&jwks).expect("router jwks must be valid")
        })
    }
}

#[cfg(test)]
mod test {
    use std::str::FromStr;
    use std::time::Duration;
    use std::time::UNIX_EPOCH;

    use insta::assert_snapshot;
    use serde_json::json;

    use crate::spec::Schema;
    use crate::uplink::license_enforcement::Audience;
    use crate::uplink::license_enforcement::Claims;
    use crate::uplink::license_enforcement::License;
    use crate::uplink::license_enforcement::LicenseEnforcementReport;
    use crate::uplink::license_enforcement::OneOrMany;
    use crate::Configuration;

    fn check(router_yaml: &str, supergraph_schema: &str) -> LicenseEnforcementReport {
        let config = Configuration::from_str(router_yaml).expect("router config must be valid");
        let schema = Schema::parse_ast(supergraph_schema).expect("supergraph schema must be valid");
        LicenseEnforcementReport::build(&config, &schema)
    }

    #[test]
    fn test_oss() {
        let report = check(
            include_str!("testdata/oss.router.yaml"),
            include_str!("testdata/oss.graphql"),
        );

        assert!(
            report.restricted_config_in_use.is_empty(),
            "should not have found restricted features"
        );
    }

    #[test]
    fn test_restricted_features_via_config() {
        let report = check(
            include_str!("testdata/restricted.router.yaml"),
            include_str!("testdata/oss.graphql"),
        );

        assert!(
            !report.restricted_config_in_use.is_empty(),
            "should have found restricted features"
        );
        assert_snapshot!(report.to_string());
    }

    #[test]
    fn test_restricted_authorization_directives_via_schema() {
        let report = check(
            include_str!("testdata/oss.router.yaml"),
            include_str!("testdata/authorization.graphql"),
        );

        assert!(
            !report.restricted_schema_in_use.is_empty(),
            "should have found restricted features"
        );
        assert_snapshot!(report.to_string());
    }

    #[test]
    fn test_restricted_unix_socket_via_schema() {
        let report = check(
            include_str!("testdata/oss.router.yaml"),
            include_str!("testdata/unix_socket.graphql"),
        );

        assert!(
            !report.restricted_schema_in_use.is_empty(),
            "should have found restricted features"
        );
        assert_snapshot!(report.to_string());
    }

    #[test]
    fn test_license_parse() {
        let license = License::from_str("eyJhbGciOiJFZERTQSJ9.eyJpc3MiOiJodHRwczovL3d3dy5hcG9sbG9ncmFwaHFsLmNvbS8iLCJzdWIiOiJhcG9sbG8iLCJhdWQiOiJTRUxGX0hPU1RFRCIsIndhcm5BdCI6MTY3NjgwODAwMCwiaGFsdEF0IjoxNjc4MDE3NjAwfQ.tXexfjZ2SQeqSwkWQ7zD4XBoxS_Hc5x7tSNJ3ln-BCL_GH7i3U9hsIgdRQTczCAjA_jjk34w39DeSV0nTc5WBw").expect("must be able to decode JWT"); // gitleaks:allow
        assert_eq!(
            license.claims,
            Some(Claims {
                iss: "https://www.apollographql.com/".to_string(),
                sub: "apollo".to_string(),
                aud: OneOrMany::One(Audience::SelfHosted),
                warn_at: UNIX_EPOCH + Duration::from_secs(1676808000),
                halt_at: UNIX_EPOCH + Duration::from_secs(1678017600),
            }),
        );
    }

    #[test]
    fn test_license_parse_with_whitespace() {
        let license = License::from_str("   eyJhbGciOiJFZERTQSJ9.eyJpc3MiOiJodHRwczovL3d3dy5hcG9sbG9ncmFwaHFsLmNvbS8iLCJzdWIiOiJhcG9sbG8iLCJhdWQiOiJTRUxGX0hPU1RFRCIsIndhcm5BdCI6MTY3NjgwODAwMCwiaGFsdEF0IjoxNjc4MDE3NjAwfQ.tXexfjZ2SQeqSwkWQ7zD4XBoxS_Hc5x7tSNJ3ln-BCL_GH7i3U9hsIgdRQTczCAjA_jjk34w39DeSV0nTc5WBw\n ").expect("must be able to decode JWT"); // gitleaks:allow
        assert_eq!(
            license.claims,
            Some(Claims {
                iss: "https://www.apollographql.com/".to_string(),
                sub: "apollo".to_string(),
                aud: OneOrMany::One(Audience::SelfHosted),
                warn_at: UNIX_EPOCH + Duration::from_secs(1676808000),
                halt_at: UNIX_EPOCH + Duration::from_secs(1678017600),
            }),
        );
    }

    #[test]
    fn test_license_parse_fail() {
        License::from_str("invalid").expect_err("jwt must fail parse");
    }

    #[test]
    fn claims_serde() {
        serde_json::from_value::<Claims>(json!({
            "iss": "Issuer",
            "sub": "Subject",
            "aud": "CLOUD",
            "warnAt": 122,
            "haltAt": 123,
        }))
        .expect("json must deserialize");

        serde_json::from_value::<Claims>(json!({
            "iss": "Issuer",
            "sub": "Subject",
            "aud": ["CLOUD", "SELF_HOSTED"],
            "warnAt": 122,
            "haltAt": 123,
        }))
        .expect("json must deserialize");

        serde_json::from_value::<Claims>(json!({
            "iss": "Issuer",
            "sub": "Subject",
            "aud": "OFFLINE",
            "warnAt": 122,
            "haltAt": 123,
        }))
        .expect("json must deserialize");
    }

    #[test]
    fn progressive_override() {
        let report = check(
            include_str!("testdata/oss.router.yaml"),
            include_str!("testdata/progressive_override.graphql"),
        );

        assert!(
            !report.restricted_schema_in_use.is_empty(),
            "should have found restricted features"
        );
        assert_snapshot!(report.to_string());
    }

    #[test]
    fn progressive_override_with_renamed_join_spec() {
        let report = check(
            include_str!("testdata/oss.router.yaml"),
            include_str!("testdata/progressive_override_renamed_join.graphql"),
        );

        assert!(
            !report.restricted_schema_in_use.is_empty(),
            "should have found restricted features"
        );
        assert_snapshot!(report.to_string());
    }

    #[test]
    fn schema_enforcement_spec_version_in_range() {
        let report = check(
            include_str!("testdata/oss.router.yaml"),
            include_str!("testdata/schema_enforcement_spec_version_in_range.graphql"),
        );

        assert!(
            !report.restricted_schema_in_use.is_empty(),
            "should have found restricted features"
        );
        assert_snapshot!(report.to_string());
    }

    #[test]
    fn schema_enforcement_spec_version_out_of_range() {
        let report = check(
            include_str!("testdata/oss.router.yaml"),
            include_str!("testdata/schema_enforcement_spec_version_out_of_range.graphql"),
        );

        assert!(
            report.restricted_schema_in_use.is_empty(),
            "shouldn't have found restricted features"
        );
    }

    #[test]
    fn schema_enforcement_directive_arg_version_in_range() {
        let report = check(
            include_str!("testdata/oss.router.yaml"),
            include_str!("testdata/schema_enforcement_directive_arg_version_in_range.graphql"),
        );

        assert!(
            !report.restricted_schema_in_use.is_empty(),
            "should have found restricted features"
        );
        assert_snapshot!(report.to_string());
    }

    #[test]
    fn schema_enforcement_directive_arg_version_out_of_range() {
        let report = check(
            include_str!("testdata/oss.router.yaml"),
            include_str!("testdata/schema_enforcement_directive_arg_version_out_of_range.graphql"),
        );

        assert!(
            report.restricted_schema_in_use.is_empty(),
            "shouldn't have found restricted features"
        );
    }
}
