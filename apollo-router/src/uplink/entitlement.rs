// With regards to ELv2 licensing, this entire file is license key functionality

// tonic does not derive `Eq` for the gRPC message types, which causes a warning from Clippy. The
// current suggestion is to explicitly allow the lint in the module that imports the protos.
// Read more: https://github.com/hyperium/tonic/issues/1056
#![allow(clippy::derive_partial_eq_without_eq)]

use std::fmt::Display;
use std::fmt::Formatter;
use std::str::FromStr;
use std::time::SystemTime;

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
use serde::Serialize;
use serde_json::Value;
use thiserror::Error;

use crate::spec::Schema;
use crate::Configuration;

static JWKS: OnceCell<JwkSet> = OnceCell::new();

#[derive(Error, Display, Debug)]
pub enum Error {
    /// invalid entitlement: {0}
    InvalidEntitlement(jsonwebtoken::errors::Error),

    /// entitlement violations: {0}
    EntitlementViolations(EntitlementReport),
}

#[derive(Eq, PartialEq)]
pub(crate) enum RouterState {
    Startup,
    Running,
}

#[derive(Debug, Eq, PartialEq)]
pub(crate) enum EntitlementState {
    Oss,
    Entitled,
    Warning,
    Halt,
}

#[derive(Eq, PartialEq)]
pub(crate) enum Action {
    PreventStartup,
    PreventReload,
    Warn,
    Halt,
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
    #[serde(with = "serde_millis", rename = "warnAt")]
    pub(crate) warn_at: SystemTime,
    #[serde(with = "serde_millis", rename = "haltAt")]
    pub(crate) halt_at: SystemTime,
}

impl Claims {
    fn entitlement_state(&self) -> EntitlementState {
        let now = SystemTime::now();
        if self.halt_at < now {
            EntitlementState::Halt
        } else if self.warn_at < now {
            EntitlementState::Warning
        } else {
            EntitlementState::Entitled
        }
    }
}

#[derive(Debug)]
pub struct EntitlementReport {
    entitlement_state: EntitlementState,
    configuration_violations: Vec<ConfigurationRestriction>,
}

impl Display for EntitlementReport {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        let formatted_violations = self
            .configuration_violations
            .iter()
            .map(|v| format!("* {} at {}", v.name, v.path))
            .join("\n");

        write!(
            f,
            "TODO PREAMBLE\n\nViolations:\n{formatted_violations}\n\nTODO POSTAMBLE"
        )
    }
}

impl EntitlementReport {
    fn action(&self, state: RouterState) -> Action {
        match (state, &self.entitlement_state) {
            (RouterState::Startup, EntitlementState::Oss) => Action::PreventStartup,
            (RouterState::Running, EntitlementState::Oss) => Action::PreventReload,
            (_, EntitlementState::Warning) => Action::Warn,
            (_, EntitlementState::Halt) => Action::Halt,
            (_, EntitlementState::Entitled) => {
                // This can never happen as Entitlement::check does not create a report for entitled users.
                panic!("entitlement report should not have been created")
            }
        }
    }
}

/// Entitlement controls availability of certain features of the Router. It must be constructed from a base64 encoded JWT
/// This API experimental and is subject to change outside of semver.
#[derive(Debug, Default, Clone)]
pub struct Entitlement {
    pub(crate) claims: Option<Claims>,
    pub(crate) configuration_restrictions: Vec<ConfigurationRestriction>,
}

impl Display for Entitlement {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        if let Some(claims) = &self.claims {
            write!(
                f,
                "{}",
                serde_json::to_string(claims)
                    .unwrap_or_else(|_| "claim serialization error".to_string())
            )
        } else {
            write!(f, "no entitlement")
        }
    }
}

impl FromStr for Entitlement {
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
                    jwt,
                    &DecodingKey::from_jwk(jwk).expect("router.jwks.json must be valid"),
                    &validation,
                )
                .map_err(Error::InvalidEntitlement)
                .map(|r| Entitlement {
                    claims: Some(r.claims),
                    configuration_restrictions: Self::configuration_restrictions(),
                })
            })
            .find_or_last(|r| r.is_ok())
            .transpose()
            .map(|e| e.unwrap_or_default())
    }
}

/// An individual check for the router.yaml.
#[derive(Builder, Clone, Debug, Serialize, Deserialize)]
pub(crate) struct ConfigurationRestriction {
    name: String,
    path: String,
    value: Value,
}

impl Entitlement {
    pub fn jwks() -> &'static JwkSet {
        JWKS.get_or_init(|| {
            // Strip the comments from the top of the file.
            let re = Regex::new("(?m)^//.*$").expect("regex must be valid");
            let jwks = re.replace(include_str!("router.jwks.json"), "");
            serde_json::from_str::<JwkSet>(&jwks).expect("router jwks must be valid")
        })
    }

    pub(crate) fn check(
        &self,
        configuration: &Configuration,
        _schema: &Schema,
    ) -> Result<(), EntitlementReport> {
        let configuration_violations = self.validate_configuration(configuration);
        let entitlement_state = self
            .claims
            .as_ref()
            .map(|claims| claims.entitlement_state())
            .unwrap_or(EntitlementState::Oss);
        if configuration_violations.is_empty()
            || matches!(entitlement_state, EntitlementState::Entitled)
        {
            Ok(())
        } else {
            Err(EntitlementReport {
                entitlement_state,
                configuration_violations,
            })
        }
    }

    fn validate_configuration(
        &self,
        configuration: &Configuration,
    ) -> Vec<ConfigurationRestriction> {
        let mut selector = jsonpath_lib::selector(
            configuration
                .validated_yaml
                .as_ref()
                .unwrap_or(&Value::Null),
        );
        let mut configuration_violations = Vec::new();
        for restriction in &self.configuration_restrictions {
            if let Some(value) = selector(&restriction.path)
                .expect("path on restriction was not valid")
                .first()
            {
                if **value == restriction.value {
                    configuration_violations.push(restriction.clone());
                }
            }
        }
        configuration_violations
    }

    fn configuration_restrictions() -> Vec<ConfigurationRestriction> {
        vec![]
    }
}

#[cfg(test)]
mod test {
    use std::str::FromStr;
    use std::time::Duration;
    use std::time::SystemTime;
    use std::time::UNIX_EPOCH;

    use insta::assert_snapshot;
    use serde_json::json;

    use crate::spec::Schema;
    use crate::uplink::entitlement::Action;
    use crate::uplink::entitlement::Audience;
    use crate::uplink::entitlement::Claims;
    use crate::uplink::entitlement::ConfigurationRestriction;
    use crate::uplink::entitlement::Entitlement;
    use crate::uplink::entitlement::EntitlementReport;
    use crate::uplink::entitlement::OneOrMany;
    use crate::uplink::entitlement::RouterState;
    use crate::Configuration;

    // For testing we restrict healthcheck
    fn configuration_restrictions() -> Vec<ConfigurationRestriction> {
        vec![
            ConfigurationRestriction::builder()
                .name("Healthcheck")
                .path("$.health_check.enabled")
                .value(true)
                .build(),
            ConfigurationRestriction::builder()
                .name("Homepage")
                .path("$.homepage.enabled")
                .value(true)
                .build(),
        ]
    }

    fn test_claim(warn_delta: i32, halt_delta: i32) -> Claims {
        let now = SystemTime::now();
        Claims {
            iss: "".to_string(),
            sub: "".to_string(),
            aud: OneOrMany::One(Audience::Cloud),
            warn_at: if warn_delta < 0 {
                now - Duration::from_secs(warn_delta.unsigned_abs() as u64)
            } else {
                now + Duration::from_secs(warn_delta as u64)
            },
            halt_at: if halt_delta < 0 {
                now - Duration::from_secs(halt_delta.unsigned_abs() as u64)
            } else {
                now + Duration::from_secs(halt_delta as u64)
            },
        }
    }

    #[test]
    fn test_oss() {
        let report = check(
            Entitlement {
                claims: None,
                configuration_restrictions: configuration_restrictions(),
            },
            include_str!("testdata/oss.router.yaml"),
            include_str!("testdata/oss.graphql"),
        );

        report.expect("should have been entitled");
    }

    fn check(
        entitlement: Entitlement,
        router_yaml: &str,
        supergraph_schema: &str,
    ) -> Result<(), EntitlementReport> {
        let config = Configuration::from_str(router_yaml).expect("router config must be valid");
        let schema =
            Schema::parse(supergraph_schema, &config).expect("supergraph schema must be valid");

        entitlement.check(&config, &schema)
    }

    #[test]
    fn test_oss_restricted_features_via_config() {
        let report = check(
            Entitlement {
                claims: None,
                configuration_restrictions: configuration_restrictions(),
            },
            include_str!("testdata/restricted.router.yaml"),
            include_str!("testdata/oss.graphql"),
        );

        let err = report.expect_err("should have got error");
        assert_snapshot!(err.to_string());
        assert!(err.action(RouterState::Startup) == Action::PreventStartup);
        assert!(err.action(RouterState::Running) == Action::PreventReload);
    }

    #[test]
    fn test_restricted_features_via_config_warning() {
        let report = check(
            Entitlement {
                claims: Some(test_claim(-1, 1)),
                configuration_restrictions: configuration_restrictions(),
            },
            include_str!("testdata/restricted.router.yaml"),
            include_str!("testdata/oss.graphql"),
        );

        let err = report.expect_err("should have got error");
        assert_snapshot!(err.to_string());
        assert!(err.action(RouterState::Startup) == Action::Warn);
        assert!(err.action(RouterState::Running) == Action::Warn);
    }

    #[test]
    fn test_restricted_features_via_config_halt() {
        let report = check(
            Entitlement {
                claims: Some(test_claim(-1, -1)),
                configuration_restrictions: configuration_restrictions(),
            },
            include_str!("testdata/restricted.router.yaml"),
            include_str!("testdata/oss.graphql"),
        );

        let err = report.expect_err("should have got error");
        assert_snapshot!(err.to_string());
        assert!(err.action(RouterState::Startup) == Action::Halt);
        assert!(err.action(RouterState::Running) == Action::Halt);
    }

    #[test]
    fn test_restricted_features_via_config_ok() {
        let report = check(
            Entitlement {
                claims: Some(test_claim(1, 1)),
                configuration_restrictions: configuration_restrictions(),
            },
            include_str!("testdata/restricted.router.yaml"),
            include_str!("testdata/oss.graphql"),
        );

        report.expect("should have been entitled");
    }

    #[test]
    fn test_entitlement_parse() {
        let entitlement = Entitlement::from_str("eyJhbGciOiJFZERTQSJ9.eyJpc3MiOiJodHRwczovL3d3dy5hcG9sbG9ncmFwaHFsLmNvbS8iLCJzdWIiOiJhcG9sbG8iLCJhdWQiOiJTRUxGX0hPU1RFRCIsIndhcm5BdCI6MTY3NjgwODAwMCwiaGFsdEF0IjoxNjc4MDE3NjAwfQ.tXexfjZ2SQeqSwkWQ7zD4XBoxS_Hc5x7tSNJ3ln-BCL_GH7i3U9hsIgdRQTczCAjA_jjk34w39DeSV0nTc5WBw").expect("must be able to decode JWT");
        assert_eq!(
            entitlement.claims,
            Some(Claims {
                iss: "https://www.apollographql.com/".to_string(),
                sub: "apollo".to_string(),
                aud: OneOrMany::One(Audience::SelfHosted),
                warn_at: UNIX_EPOCH + Duration::from_millis(1676808000),
                halt_at: UNIX_EPOCH + Duration::from_millis(1678017600),
            }),
        );
    }

    #[test]
    fn test_entitlement_parse_fail() {
        Entitlement::from_str("invalid").expect_err("jwt must fail parse");
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
