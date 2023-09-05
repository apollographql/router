// tonic does not derive `Eq` for the gRPC message types, which causes a warning from Clippy. The
// current suggestion is to explicitly allow the lint in the module that imports the protos.
// Read more: https://github.com/hyperium/tonic/issues/1056
#![allow(clippy::derive_partial_eq_without_eq)]

use std::fmt::Display;
use std::fmt::Formatter;
use std::str::FromStr;
use std::time::Duration;
use std::time::SystemTime;
use std::time::UNIX_EPOCH;

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

use crate::Configuration;

pub(crate) const LICENSE_EXPIRED_URL: &str = "https://go.apollo.dev/o/elp";
pub(crate) const LICENSE_EXPIRED_SHORT_MESSAGE: &str =
    "Apollo license expired https://go.apollo.dev/o/elp";

static JWKS: OnceCell<JwkSet> = OnceCell::new();

#[derive(Error, Display, Debug)]
pub enum Error {
    /// invalid license: {0}
    InvalidLicense(jsonwebtoken::errors::Error),
}

#[derive(Debug, Clone, Serialize, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub(crate) enum Audience {
    SelfHosted,
    Cloud,
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
}

impl LicenseEnforcementReport {
    pub(crate) fn uses_restricted_features(&self) -> bool {
        !self.restricted_config_in_use.is_empty()
    }

    pub(crate) fn build(configuration: &Configuration) -> LicenseEnforcementReport {
        LicenseEnforcementReport {
            restricted_config_in_use: Self::validate_configuration(
                configuration,
                &Self::configuration_restrictions(),
            ),
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
                .path("$.authorization.preview_directives")
                .name("Authorization directives")
                .build(),
            ConfigurationRestriction::builder()
                .path("$.coprocessor")
                .name("Coprocessor plugin")
                .build(),
            ConfigurationRestriction::builder()
                .path("$.supergraph.query_planning.experimental_cache.redis")
                .name("Query plan caching")
                .build(),
            ConfigurationRestriction::builder()
                .path("$.apq.router.cache.redis")
                .name("APQ caching")
                .build(),
            ConfigurationRestriction::builder()
                .path("$.traffic_shaping.experimental_cache")
                .name("Subgraph caching")
                .build(),
            ConfigurationRestriction::builder()
                .path("$.traffic_shaping..experimental_entity_caching")
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
                .path("$.preview_persisted_queries")
                .name("Persisted queries")
                .build(),
        ]
    }
}

impl Display for LicenseEnforcementReport {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        let restricted_config = self
            .restricted_config_in_use
            .iter()
            .map(|v| format!("* {}\n  {}", v.name, v.path.replace("$.", ".")))
            .join("\n\n");

        write!(f, "Configuration yaml:\n{restricted_config}")
    }
}

/// License controls availability of certain features of the Router. It must be constructed from a base64 encoded JWT
/// This API experimental and is subject to change outside of semver.
#[derive(Debug, Clone, Default)]
pub struct License {
    pub(crate) claims: Option<Claims>,
}

/// Licenses are converted into a stream of license states by the expander
#[derive(Debug, Copy, Clone, Eq, PartialEq, Default)]
pub(crate) enum LicenseState {
    Licensed,
    LicensedWarn,
    LicensedHalt,
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
                    jwk.common
                        .algorithm
                        .expect("alg is required on all keys in router.jwks.json"),
                );
                validation.validate_exp = false;
                validation.set_required_spec_claims(&["iss", "sub", "aud", "warnAt", "haltAt"]);
                validation.set_issuer(&["https://www.apollographql.com/"]);
                validation.set_audience(&["CLOUD", "SELF_HOSTED"]);

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

    use crate::uplink::license_enforcement::Audience;
    use crate::uplink::license_enforcement::Claims;
    use crate::uplink::license_enforcement::License;
    use crate::uplink::license_enforcement::LicenseEnforcementReport;
    use crate::uplink::license_enforcement::OneOrMany;
    use crate::Configuration;

    fn check(router_yaml: &str) -> LicenseEnforcementReport {
        let config = Configuration::from_str(router_yaml).expect("router config must be valid");

        LicenseEnforcementReport::build(&config)
    }

    #[test]
    fn test_oss() {
        let report = check(include_str!("testdata/oss.router.yaml"));

        assert!(
            report.restricted_config_in_use.is_empty(),
            "should not have found restricted features"
        );
    }

    #[test]
    fn test_restricted_features_via_config() {
        let report = check(include_str!("testdata/restricted.router.yaml"));

        assert!(
            !report.restricted_config_in_use.is_empty(),
            "should have found restricted features"
        );
        assert_snapshot!(report.to_string());
    }

    #[test]
    fn test_license_parse() {
        let license = License::from_str("eyJhbGciOiJFZERTQSJ9.eyJpc3MiOiJodHRwczovL3d3dy5hcG9sbG9ncmFwaHFsLmNvbS8iLCJzdWIiOiJhcG9sbG8iLCJhdWQiOiJTRUxGX0hPU1RFRCIsIndhcm5BdCI6MTY3NjgwODAwMCwiaGFsdEF0IjoxNjc4MDE3NjAwfQ.tXexfjZ2SQeqSwkWQ7zD4XBoxS_Hc5x7tSNJ3ln-BCL_GH7i3U9hsIgdRQTczCAjA_jjk34w39DeSV0nTc5WBw").expect("must be able to decode JWT");
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
        let license = License::from_str("   eyJhbGciOiJFZERTQSJ9.eyJpc3MiOiJodHRwczovL3d3dy5hcG9sbG9ncmFwaHFsLmNvbS8iLCJzdWIiOiJhcG9sbG8iLCJhdWQiOiJTRUxGX0hPU1RFRCIsIndhcm5BdCI6MTY3NjgwODAwMCwiaGFsdEF0IjoxNjc4MDE3NjAwfQ.tXexfjZ2SQeqSwkWQ7zD4XBoxS_Hc5x7tSNJ3ln-BCL_GH7i3U9hsIgdRQTczCAjA_jjk34w39DeSV0nTc5WBw\n ").expect("must be able to decode JWT");
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
    }
}
