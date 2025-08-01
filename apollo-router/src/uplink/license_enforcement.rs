// tonic does not derive `Eq` for the gRPC message types, which causes a warning from Clippy. The
// current suggestion is to explicitly allow the lint in the module that imports the protos.
// Read more: https://github.com/hyperium/tonic/issues/1056
#![allow(clippy::derive_partial_eq_without_eq)]

use std::collections::HashMap;
use std::collections::HashSet;
use std::fmt::Display;
use std::fmt::Formatter;
use std::str::FromStr;
use std::time::Duration;
use std::time::SystemTime;
use std::time::UNIX_EPOCH;

use apollo_compiler::schema::ExtendedType;
use buildstructor::Builder;
use displaydoc::Display;
use itertools::Itertools;
use jsonwebtoken::DecodingKey;
use jsonwebtoken::Validation;
use jsonwebtoken::decode;
use jsonwebtoken::jwk::JwkSet;
use once_cell::sync::OnceCell;
use regex::Regex;
use serde::Deserialize;
use serde::Deserializer;
use serde::Serialize;
use serde_json::Value;
use thiserror::Error;

use super::parsed_link_spec::ParsedLinkSpec;
use crate::Configuration;
use crate::plugins::authentication::jwks::convert_key_algorithm;
use crate::spec::LINK_DIRECTIVE_NAME;
use crate::spec::Schema;

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
    /// When to warn the user about an expiring license that must be renewed to avoid halting the
    /// router
    pub(crate) warn_at: SystemTime,
    #[serde(deserialize_with = "deserialize_epoch_seconds", rename = "haltAt")]
    /// When to halt the router because of an expired license
    pub(crate) halt_at: SystemTime,
    /// TPS limits. These may not exist in a License; if not, no limits apply
    #[serde(rename = "throughputLimit")]
    pub(crate) tps: Option<TpsLimit>,
    /// Set of allowed features. These may not exist in a License; if not, all features are enabled
    /// NB: This is temporary behavior and will be updated once all licenses contain an allowed_features claim.
    pub(crate) allowed_features: Option<Vec<AllowedFeature>>,
}

fn deserialize_epoch_seconds<'de, D>(deserializer: D) -> Result<SystemTime, D::Error>
where
    D: Deserializer<'de>,
{
    let seconds = i32::deserialize(deserializer)?;
    Ok(UNIX_EPOCH + Duration::from_secs(seconds as u64))
}

fn deserialize_ms_into_duration<'de, D>(deserializer: D) -> Result<Duration, D::Error>
where
    D: Deserializer<'de>,
{
    let seconds = i32::deserialize(deserializer)?;
    Ok(Duration::from_millis(seconds as u64))
}

#[derive(Debug)]
pub(crate) struct LicenseEnforcementReport {
    restricted_config_in_use: Vec<ConfigurationRestriction>,
    restricted_schema_in_use: Vec<SchemaViolation>,
}

impl LicenseEnforcementReport {
    pub(crate) fn uses_restricted_features(&self) -> bool {
        !self.restricted_config_in_use.is_empty() || !self.restricted_schema_in_use.is_empty()
    }

    pub(crate) fn build(
        configuration: &Configuration,
        schema: &Schema,
        license: &LicenseState,
    ) -> LicenseEnforcementReport {
        LicenseEnforcementReport {
            restricted_config_in_use: Self::validate_configuration(
                configuration,
                &Self::configuration_restrictions(license),
            ),
            restricted_schema_in_use: Self::validate_schema(
                schema,
                &Self::schema_restrictions(license),
                license,
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

    fn validate_schema(
        schema: &Schema,
        schema_restrictions: &Vec<SchemaRestriction>,
        license: &LicenseState,
    ) -> Vec<SchemaViolation> {
        let link_specs = schema
            .supergraph_schema()
            .schema_definition
            .directives
            .get_all(LINK_DIRECTIVE_NAME)
            .filter_map(|link| {
                ParsedLinkSpec::from_link_directive(link).map(|maybe_spec| {
                    maybe_spec.ok().map(|spec| (spec.spec_url.to_owned(), spec))
                })?
            })
            .collect::<HashMap<_, _>>();

        let link_specs_in_join_directive = schema
            .supergraph_schema()
            .schema_definition
            .directives
            .get_all("join__directive")
            .filter(|join| {
                join.specified_argument_by_name("name")
                    .and_then(|name| name.as_str())
                    .map(|name| name == LINK_DIRECTIVE_NAME)
                    .unwrap_or_default()
            })
            .filter_map(|join| {
                join.specified_argument_by_name("args")
                    .and_then(|arg| arg.as_object())
            })
            .filter_map(|link| {
                ParsedLinkSpec::from_join_directive_args(link).map(|maybe_spec| {
                    maybe_spec.ok().map(|spec| (spec.spec_url.to_owned(), spec))
                })?
            })
            .collect::<HashMap<_, _>>();

        let mut schema_violations: Vec<SchemaViolation> = Vec::new();

        for (_subgraph_name, subgraph_url) in schema.subgraphs() {
            if subgraph_url.scheme_str() == Some("unix") {
                if let Some(features) = license.get_allowed_features() {
                    if !features.contains(&AllowedFeature::UnixSocketSupport) {
                        schema_violations.push(SchemaViolation::DirectiveArgument {
                    url: "https://specs.apollo.dev/join/v0.3".to_string(),
                    name: "join__Graph".to_string(),
                    argument: "url".to_string(),
                    explanation: "Unix socket support for subgraph requests is restricted to Enterprise users".to_string(),
                });
                    }
                }
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
                                .supergraph_schema()
                                .types
                                .values()
                                .flat_map(|def| match def {
                                    // To traverse additional directive locations, add match arms for the respective definition types required.
                                    // As of writing this, this is only implemented for finding usages of progressive override on object type fields, but it can be extended to other directive locations trivially.
                                    ExtendedType::Object(object_type_def) => {
                                        let directives_on_object = object_type_def
                                            .directives
                                            .get_all(&directive_name)
                                            .map(|component| &component.node);
                                        let directives_on_fields =
                                            object_type_def.fields.values().flat_map(|field| {
                                                field.directives.get_all(&directive_name)
                                            });

                                        directives_on_object
                                            .chain(directives_on_fields)
                                            .collect::<Vec<_>>()
                                    }
                                    _ => vec![],
                                })
                                .any(|directive| {
                                    directive.specified_argument_by_name(argument).is_some()
                                })
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
                SchemaRestriction::SpecInJoinDirective {
                    spec_url,
                    name,
                    version_req,
                } => {
                    if let Some(link_spec) = link_specs_in_join_directive.get(spec_url) {
                        if version_req.matches(&link_spec.version) {
                            schema_violations.push(SchemaViolation::Spec {
                                url: link_spec.url.to_string(),
                                name: name.to_string(),
                            });
                        }
                    }
                }
            }
        }

        schema_violations
    }

    fn configuration_restrictions(license: &LicenseState) -> Vec<ConfigurationRestriction> {
        let mut configuration_restrictions = vec![];
        // If the license has no allowed_features claim, we're using a pricing plan
        // that should have the feature enabled regardless - nothing further is added to
        // configuration_restrictions.
        // NB: This is temporary behavior and will be updated once all licenses contain
        // an allowed_features claim.

        // If the license has an allowed_features claim, we know we're using a pricing
        // plan with a subset of allowed features
        if let Some(allowed_features) = license.get_allowed_features() {
            // Check if the following features are in the licenses' allowed_features claim
            if !allowed_features.contains(&AllowedFeature::APQCaching) {
                configuration_restrictions.push(
                    ConfigurationRestriction::builder()
                        .path("$.apq.router.cache.redis")
                        .name("APQ caching")
                        .build(),
                )
            }
            if !allowed_features.contains(&AllowedFeature::Authentication) {
                configuration_restrictions.push(
                    ConfigurationRestriction::builder()
                        .path("$.authentication.router")
                        .name("Authentication plugin")
                        .build(),
                );
            }
            if !allowed_features.contains(&AllowedFeature::Authorization) {
                configuration_restrictions.push(
                    ConfigurationRestriction::builder()
                        .path("$.authorization.directives")
                        .name("Authorization directives")
                        .build(),
                );
            }
            if !allowed_features.contains(&AllowedFeature::Batching) {
                configuration_restrictions.push(
                    ConfigurationRestriction::builder()
                        .path("$.batching")
                        .name("Batching support")
                        .build(),
                );
            }
            if !allowed_features.contains(&AllowedFeature::EntityCaching) {
                configuration_restrictions.push(
                    ConfigurationRestriction::builder()
                        .path("$.preview_entity_cache.enabled")
                        .value(true)
                        .name("Subgraph entity caching")
                        .build(),
                );
            }
            if !allowed_features.contains(&AllowedFeature::FileUploads) {
                configuration_restrictions.push(
                    ConfigurationRestriction::builder()
                        .path("$.preview_file_uploads")
                        .name("File uploads plugin")
                        .build(),
                );
            }
            if !allowed_features.contains(&AllowedFeature::PersistedQueriesSafelisting) {
                configuration_restrictions.push(
                    ConfigurationRestriction::builder()
                        .path("$.persisted_queries")
                        .name("Persisted queries")
                        .build(),
                );
            }
            if !allowed_features.contains(&AllowedFeature::Subscriptions) {
                configuration_restrictions.push(
                    ConfigurationRestriction::builder()
                        .path("$.subscription.enabled")
                        .value(true)
                        .name("Federated subscriptions")
                        .build(),
                );
                if !allowed_features.contains(&AllowedFeature::Coprocessor) {
                    configuration_restrictions.push(
                        ConfigurationRestriction::builder()
                            .path("$.coprocessor")
                            .name("Coprocessor plugin")
                            .build(),
                    )
                };
                if !allowed_features.contains(&AllowedFeature::DistributedQueryPlanning) {
                    configuration_restrictions.push(
                        ConfigurationRestriction::builder()
                            .path("$.supergraph.query_planning.cache.redis")
                            .name("Query plan caching")
                            .build(),
                    )
                }
                // TODO-Ellie: Do we need anything more fine grained for demand control cost?
                // Question-For-Josh ^^
                if !allowed_features.contains(&AllowedFeature::DemandControl) {
                    configuration_restrictions.push(
                        ConfigurationRestriction::builder()
                            .path("$.demand_control")
                            .name("Demand control plugin")
                            .build(),
                    );
                }
                if !allowed_features.contains(&AllowedFeature::Events) {
                    // TODO-Ellie: is this the correct feature to use?

                    // Question-For-Josh ^^
                    configuration_restrictions.push(
                        ConfigurationRestriction::builder()
                            .path("$.telemetry..events")
                            .name("Advanced telemetry")
                            .build(),
                    );
                }
                if !allowed_features.contains(&AllowedFeature::Instruments) {
                    // TODO-Ellie: is this the correct feature to use?
                    // Question-For-Josh ^^
                    configuration_restrictions.push(
                        ConfigurationRestriction::builder()
                            .path("$.telemetry..instruments")
                            .name("Advanced telemetry")
                            .build(),
                    );
                }
                if !allowed_features.contains(&AllowedFeature::Experimental) {
                    configuration_restrictions.push(
                        ConfigurationRestriction::builder()
                            .path("$.plugins.['experimental.restricted'].enabled")
                            .value(true)
                            .name("Restricted")
                            .build(),
                    );
                }
                if !allowed_features.contains(&AllowedFeature::ExtendedReferenceReporting) {
                    configuration_restrictions.push(
                        ConfigurationRestriction::builder()
                            .path("$.telemetry.apollo.metrics_reference_mode")
                            .value("extended")
                            .name("Apollo metrics extended references")
                            .build(),
                    );
                }
                // Per-operation limits are restricted but parser limits like `parser_max_recursion`
                // where the Router only configures apollo-rs are not.
                if !allowed_features.contains(&AllowedFeature::RequestLimits) {
                    // TODO-Ellie: should these be separated out into different features?
                    configuration_restrictions.extend(vec![
                        ConfigurationRestriction::builder()
                            .path("$.limits.max_depth")
                            .name("Operation depth limiting")
                            .build(),
                        ConfigurationRestriction::builder()
                            .path("$.limits.max_root_fields")
                            .name("Operation root fields limiting")
                            .build(),
                        ConfigurationRestriction::builder()
                            .path("$.limits.max_height")
                            .name("Operation height limiting")
                            .build(),
                        ConfigurationRestriction::builder()
                            .path("$.limits.max_aliases")
                            .name("Operation aliases limiting")
                            .build(),
                    ])
                }
                if !allowed_features.contains(&AllowedFeature::AdvancedTelemetry) {
                    // TODO-Ellie: should these be separated out into different features?
                    // Question-For-Josh - should these be more fine grained?
                    configuration_restrictions.extend(vec![
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
                            .path("$.telemetry..graphql")
                            .name("Advanced telemetry")
                            .build(),
                    ])
                }
            }
        }
        configuration_restrictions
    }

    fn schema_restrictions(license: &LicenseState) -> Vec<SchemaRestriction> {
        let mut schema_restrictions = vec![];

        // If the license has no allowed_features claim, we're using a pricing plan
        // that should have the feature enabled regardless - nothing further is added to
        // configuration_restrictions.
        // NB: This is temporary behavior and will be updated once all licenses contain
        // an allowed_features claim.

        // If the license has an allowed_features claim, we know we're using a pricing
        // plan with a subset of allowed features
        // Check if the following features are in the licenses' allowed_features claim
        if let Some(allowed_features) = license.get_allowed_features() {
            // TODO-Ellie: should this be available for OSS? Currently it is not, with my changes
            // it is because connectors are in the list of OSS features
            if !allowed_features.contains(&AllowedFeature::RestConnectors) {
                schema_restrictions.push(SchemaRestriction::SpecInJoinDirective {
                    name: "connect".to_string(),
                    spec_url: "https://specs.apollo.dev/connect".to_string(),
                    version_req: semver::VersionReq {
                        comparators: vec![], // all versions
                    },
                })
            }
            if !allowed_features.contains(&AllowedFeature::Authentication) {
                schema_restrictions.push(SchemaRestriction::Spec {
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
                });
                // TODO-Ellie: does this belong with Authentication?
                schema_restrictions.push(SchemaRestriction::Spec {
                    name: "context".to_string(),
                    spec_url: "https://specs.apollo.dev/context".to_string(),
                    version_req: semver::VersionReq {
                        comparators: vec![semver::Comparator {
                            op: semver::Op::Exact,
                            major: 0,
                            minor: 1.into(),
                            patch: 0.into(),
                            pre: semver::Prerelease::EMPTY,
                        }],
                    },
                });
                // TODO-Ellie: does this belong with Authentication?
                schema_restrictions.push(SchemaRestriction::Spec {
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
                });
            }
            // TODO-Ellie: are these correct?
            if !allowed_features.contains(&AllowedFeature::FederationOverrideLabel) {
                schema_restrictions.push(SchemaRestriction::DirectiveArgument {
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
            });
            }
            // TODO-Ellie: are these correct?
            if !allowed_features.contains(&AllowedFeature::FederationOverrideLabel) {
                schema_restrictions.push(SchemaRestriction::DirectiveArgument {
                name: "field".to_string(),
                argument: "contextArguments".to_string(),
                spec_url: "https://specs.apollo.dev/join".to_string(),
                version_req: semver::VersionReq {
                    comparators: vec![semver::Comparator {
                        op: semver::Op::GreaterEq,
                        major: 0,
                        minor: 5.into(),
                        patch: 0.into(),
                        pre: semver::Prerelease::EMPTY,
                    }],
                },
                explanation: "The `contextArguments` argument on the join spec's @field directive is restricted to Enterprise users. This argument exists in your supergraph as a result of using the `@fromContext` directive in one or more of your subgraphs.".to_string()
        });
            }
        }
        schema_restrictions
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

/// Claims extracted from the License, including ways Apollo limits the router's usage. It must be constructed from a base64 encoded JWT
/// This API experimental and is subject to change outside of semver.
#[derive(Debug, Clone, Default)]
pub struct License {
    pub(crate) claims: Option<Claims>,
}

/// Transactions Per Second limits. We talk as though this will be in seconds, but the Duration
/// here is actually given to us in milliseconds via the License's JWT's claims
#[derive(Builder, Copy, Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct TpsLimit {
    pub(crate) capacity: usize,

    #[serde(
        deserialize_with = "deserialize_ms_into_duration",
        rename = "durationMs"
    )]
    pub(crate) interval: Duration,
}

// TODO-Ellie: review this list!
/// Allowed features for a License, representing what's available to a particular pricing tier
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Hash)]
pub enum AllowedFeature {
    // TODO-Eliie: should these be split up into separate features?
    // Question-For-Josh - should these be more fine grained?
    /// Router, supergraph, subgraph, and graphql advanced telemetry
    AdvancedTelemetry,
    // TODO-Ellie: remove?
    /// Automatic persistent queries
    APQ,
    /// APQ caching
    APQCaching,
    /// Authentication plugin
    Authentication,
    /// Authorization directives
    Authorization,
    /// Batching support
    Batching,
    /// Coprocessor plugin
    Coprocessor,
    // TODO-Ellie: DemandControl plugin versus DemandControlCost?
    // Question-For-Josh ^^
    /// Demand control plugin
    DemandControl,
    /// Distributed query planning
    DistributedQueryPlanning,
    /// Subgraph entity caching
    EntityCaching,
    // Question-For-Josh - what do we do with events?
    /// Router telemetry - events
    Events,
    // Question-For-Josh - are these oss?
    /// Experimental features in the router
    Experimental,
    /// Extended reference reporting
    ExtendedReferenceReporting,
    // TODO-Ellie: should this be here?
    // Question-For-Josh ^^
    /// overrideLabel argument on the join spec's @field directive
    FederationOverrideLabel,
    // TODO-Ellie: should this be here?
    // Question-For-Josh ^^
    /// contextArguments argument on the join spec's @field directive
    FederationContextArguments,
    /// File uploads plugin
    FileUploads,
    /// Forbid mutations plugin
    ForbidMutations,
    /// Router temeletry - instruments
    Instruments,
    // TODO-Ellie: remove?
    /// Override subgraph url plugin
    OverrideSubgraphUrl,
    /// Persisted queries safelisting
    PersistedQueriesSafelisting,
    /// Rest connectors
    RestConnectors,
    /// Request limits - depth and breadth
    RequestLimits,
    // TODO-Ellie: remove?
    /// Rhai
    Rhai,
    /// Federated subscriptions
    Subscriptions,
    // TODO-Ellie: remove?
    /// Traffic shaping plugin
    TrafficShaping,
    /// Unix socket support for subgraph requests
    UnixSocketSupport,
    /// This represents a feature found in the license that the router does not recognize
    Other(String),
}

impl From<&str> for AllowedFeature {
    fn from(feature: &str) -> Self {
        match feature {
            "advanced_telemetry" => Self::AdvancedTelemetry,
            "apq" => Self::APQ,
            "apq_caching" => Self::APQCaching,
            "authentication" => Self::Authentication,
            "authorization" => Self::Authorization,
            "batching" => Self::Batching,
            "connectors" => Self::RestConnectors,
            "coprocessor" => Self::Coprocessor,
            "demand_control" => Self::DemandControl,
            "preview_entity_cache" => Self::EntityCaching,
            "events" => Self::Events,
            "experimental" | "experimental_mock_subgraphs" | "experimental_response_cache" => {
                Self::Experimental
            }
            "forbid_mutations" => Self::ForbidMutations,
            "extended_reference_reporting" => Self::ExtendedReferenceReporting,
            "preview_file_uploads" => Self::FileUploads,
            "instruments" => Self::Instruments,
            "limits" => Self::RequestLimits,
            "override_subgraph_url" => Self::OverrideSubgraphUrl,
            "persisted_queries" => Self::PersistedQueriesSafelisting,
            "query_planning_cache" => Self::DistributedQueryPlanning,
            "rhai" => Self::Rhai,
            "subscription" => Self::Subscriptions,
            "traffic_shaping" => Self::TrafficShaping,
            other => Self::Other(other.into()),
        }
    }
}

/// LicenseLimits represent what can be done with a router based on the claims in the License. You
/// might have a certain tier be limited in its capacity for transactions over a certain duration,
/// as an example
#[derive(Debug, Builder, Clone, Default, Eq, PartialEq)]
pub struct LicenseLimits {
    /// Transaction Per Second limits. If none are found in the License's claims, there are no
    /// limits to apply
    pub(crate) tps: Option<TpsLimit>,
    /// The allowed features based on the allowed features present on the License's claims
    pub allowed_features: Option<HashSet<AllowedFeature>>,
}

/// Licenses are converted into a stream of license states by the expander
#[derive(Debug, Clone, Eq, PartialEq, Default, Display)]
pub enum LicenseState {
    /// licensed
    Licensed { limits: Option<LicenseLimits> },
    /// warn
    LicensedWarn { limits: Option<LicenseLimits> },
    /// halt
    LicensedHalt { limits: Option<LicenseLimits> },

    /// unlicensed
    #[default]
    Unlicensed,
}

// TODO-ELiie: review this
const OSS_FEATURES: [AllowedFeature; 3] = [
    // TODO-Ellie: Can we just get rid of features like apq that correspond to oss plugins?
    // AllowedFeature::APQ,
    // TODO-Ellie: Can we just get rid of features like traffic shaping that correspond to oss plugins?
    // AllowedFeature::TrafficShaping, // plugin
    // Question-For-Josh: connectors are not currently oss
    AllowedFeature::RestConnectors,
    // Question-For-Josh: should file uploads be available to everyone - currently it is not
    AllowedFeature::FileUploads,
    AllowedFeature::Events,
    // TODO-Ellie: Can we just get rid of features like rhai that correspond to oss plugins?
    // AllowedFeature::Rhai, // plugin
    // Question-For-Josh: both of these features do not have config thats gated currently
    // AllowedFeature::OverrideSubgraphUrl,
    // AllowedFeature::ForbidMutations,
];

impl LicenseState {
    pub(crate) fn get_limits(&self) -> Option<&LicenseLimits> {
        match self {
            LicenseState::Licensed { limits }
            | LicenseState::LicensedWarn { limits }
            | LicenseState::LicensedHalt { limits } => limits.as_ref(),
            _ => None,
        }
    }

    pub(crate) fn get_allowed_features(&self) -> Option<HashSet<AllowedFeature>> {
        match self {
            LicenseState::Licensed { limits }
            | LicenseState::LicensedWarn { limits }
            | LicenseState::LicensedHalt { limits } => match limits {
                Some(limits) => limits.allowed_features.clone(),
                // NB: this may change once all licenses have an `allowed_features` claim
                None => None,
            },
            LicenseState::Unlicensed => Some(HashSet::from_iter(OSS_FEATURES)),
        }
    }

    pub(crate) fn get_name(&self) -> &'static str {
        match self {
            Self::Licensed { limits: _ } => "Licensed",
            Self::LicensedWarn { limits: _ } => "LicensedWarn",
            Self::LicensedHalt { limits: _ } => "LicensedHalt",
            Self::Unlicensed => "Unlicensed",
        }
    }
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

    SpecInJoinDirective {
        spec_url: String,
        name: String,
        version_req: semver::VersionReq,
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
    use std::collections::HashSet;
    use std::str::FromStr;
    use std::time::Duration;
    use std::time::UNIX_EPOCH;

    use insta::assert_snapshot;
    use serde_json::json;

    use crate::AllowedFeature;
    use crate::Configuration;
    use crate::spec::Schema;
    use crate::uplink::license_enforcement::Audience;
    use crate::uplink::license_enforcement::Claims;
    use crate::uplink::license_enforcement::License;
    use crate::uplink::license_enforcement::LicenseEnforcementReport;
    use crate::uplink::license_enforcement::LicenseLimits;
    use crate::uplink::license_enforcement::LicenseState;
    use crate::uplink::license_enforcement::OneOrMany;
    use crate::uplink::license_enforcement::SchemaViolation;

    #[track_caller]
    fn check(
        router_yaml: &str,
        supergraph_schema: &str,
        license: LicenseState,
    ) -> LicenseEnforcementReport {
        let config = Configuration::from_str(router_yaml).expect("router config must be valid");
        let schema =
            Schema::parse(supergraph_schema, &config).expect("supergraph schema must be valid");

        LicenseEnforcementReport::build(&config, &schema, &license)
    }

    #[test]
    fn test_oss() {
        let report = check(
            include_str!("testdata/oss.router.yaml"),
            include_str!("testdata/oss.graphql"),
            LicenseState::default(),
        );

        assert!(
            report.restricted_config_in_use.is_empty(),
            "should not have found restricted features"
        );
    }

    #[test]
    fn test_restricted_features_via_config_unlicensed() {
        let report = check(
            include_str!("testdata/restricted.router.yaml"),
            include_str!("testdata/oss.graphql"),
            LicenseState::default(),
        );

        assert!(
            !report.restricted_config_in_use.is_empty(),
            "should have found restricted features"
        );
        assert_snapshot!(report.to_string());
    }

    #[test]
    fn test_restricted_features_via_config_allowed_features_empty() {
        let report = check(
            include_str!("testdata/restricted.router.yaml"),
            include_str!("testdata/oss.graphql"),
            LicenseState::Licensed {
                limits: Some(LicenseLimits {
                    tps: None,
                    allowed_features: Some(HashSet::from_iter(vec![])),
                }),
            },
        );

        assert!(
            !report.restricted_config_in_use.is_empty(),
            "should have found restricted features"
        );
        assert_snapshot!(report.to_string());
    }

    #[test]
    fn test_restricted_features_via_config_with_allowed_features() {
        // The config includes subscriptions but the license's
        // allowed_features claim does not include subscriptions
        let report = check(
            include_str!("testdata/restricted.router.yaml"),
            include_str!("testdata/oss.graphql"),
            LicenseState::Licensed {
                limits: Some(LicenseLimits {
                    tps: None,
                    allowed_features: Some(HashSet::from_iter(vec![
                        AllowedFeature::Authentication,
                        AllowedFeature::Authorization,
                        AllowedFeature::Batching,
                        AllowedFeature::DemandControl,
                        AllowedFeature::EntityCaching,
                        AllowedFeature::FileUploads,
                        AllowedFeature::PersistedQueriesSafelisting,
                        AllowedFeature::APQCaching,
                    ])),
                }),
            },
        );

        assert!(
            !report.restricted_config_in_use.is_empty(),
            "should have found restricted features"
        );
        assert_snapshot!(report.to_string());
    }

    #[test]
    fn test_restricted_authorization_directives_via_schema_unlicensed() {
        let report = check(
            include_str!("testdata/oss.router.yaml"),
            include_str!("testdata/authorization.graphql"),
            LicenseState::default(),
        );

        assert!(
            !report.restricted_schema_in_use.is_empty(),
            "should have found restricted features"
        );
        assert_snapshot!(report.to_string());
    }

    #[test]
    fn test_restricted_authorization_directives_via_schema_with_allowed_features_containing_feature()
     {
        let report = check(
            include_str!("testdata/oss.router.yaml"),
            include_str!("testdata/authorization.graphql"),
            LicenseState::Licensed {
                limits: Some(LicenseLimits {
                    tps: None,
                    allowed_features: Some(HashSet::from_iter(vec![
                        AllowedFeature::Authentication,
                        AllowedFeature::Authorization,
                    ])),
                }),
            },
        );

        assert!(
            report.restricted_schema_in_use.is_empty(),
            "should have not found restricted features"
        );
    }

    #[test]
    #[cfg(not(windows))] // http::uri::Uri parsing appears to reject unix:// on Windows
    fn test_restricted_unix_socket_via_schema_unlicensed() {
        let report = check(
            include_str!("testdata/oss.router.yaml"),
            include_str!("testdata/unix_socket.graphql"),
            LicenseState::default(),
        );

        assert!(
            !report.restricted_schema_in_use.is_empty(),
            "should have found restricted features"
        );
        assert_snapshot!(report.to_string());
    }

    #[test]
    #[cfg(not(windows))] // http::uri::Uri parsing appears to reject unix:// on Windows
    fn test_restricted_unix_socket_via_schema_allowed_features_none() {
        let report = check(
            include_str!("testdata/oss.router.yaml"),
            include_str!("testdata/unix_socket.graphql"),
            LicenseState::Licensed {
                limits: Some(LicenseLimits {
                    tps: None,
                    allowed_features: None,
                }),
            },
        );

        assert!(
            report.restricted_schema_in_use.is_empty(),
            "should not have found restricted features"
        );
    }

    #[test]
    #[cfg(not(windows))] // http::uri::Uri parsing appears to reject unix:// on Windows
    fn test_restricted_unix_socket_via_schema_when_allowed_features_contains_feature() {
        let report = check(
            include_str!("testdata/oss.router.yaml"),
            include_str!("testdata/unix_socket.graphql"),
            LicenseState::Licensed {
                limits: Some(LicenseLimits {
                    tps: None,
                    allowed_features: Some(HashSet::from_iter(vec![
                        AllowedFeature::UnixSocketSupport,
                        AllowedFeature::Batching,
                    ])),
                }),
            },
        );
        assert!(
            report.restricted_schema_in_use.is_empty(),
            "should not have found restricted features"
        );
    }

    #[test]
    #[cfg(not(windows))] // http::uri::Uri parsing appears to reject unix:// on Windows
    fn test_restricted_unix_socket_via_schema_when_allowed_features_empty() {
        let report = check(
            include_str!("testdata/oss.router.yaml"),
            include_str!("testdata/unix_socket.graphql"),
            LicenseState::Licensed {
                limits: Some(LicenseLimits {
                    tps: None,
                    allowed_features: Some(HashSet::new()),
                }),
            },
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
                tps: Default::default(),
                allowed_features: None
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
                tps: Default::default(),
                allowed_features: None
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

        serde_json::from_value::<Claims>(json!({
            "iss": "Issuer",
            "sub": "Subject",
            "aud": "OFFLINE",
            "warnAt": 122,
            "haltAt": 123,
            "allowedFeatures": ["SUBSCRIPTIONS", "ENTITY_CACHING"]
        }))
        .expect("json must deserialize");
    }

    #[test]
    fn progressive_override() {
        let report = check(
            include_str!("testdata/oss.router.yaml"),
            include_str!("testdata/progressive_override.graphql"),
            LicenseState::default(),
        );

        assert!(
            !report.restricted_schema_in_use.is_empty(),
            "should have found restricted features"
        );
        assert_snapshot!(report.to_string());
    }

    #[test]
    fn set_context() {
        let report = check(
            include_str!("testdata/oss.router.yaml"),
            include_str!("testdata/set_context.graphql"),
            LicenseState::default(),
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
            LicenseState::default(),
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
            LicenseState::default(),
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
            LicenseState::default(),
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
            LicenseState::default(),
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
            LicenseState::default(),
        );

        assert!(
            report.restricted_schema_in_use.is_empty(),
            "shouldn't have found restricted features"
        );
    }

    // TODO-Ellie: figure this out - connectors are supposed to be OSS but this
    // existing test says they are not supposed to be
    #[test]
    fn schema_enforcement_connectors() {
        let report = check(
            include_str!("testdata/oss.router.yaml"),
            include_str!("testdata/schema_enforcement_connectors.graphql"),
            LicenseState::default(),
        );

        assert_eq!(
            1,
            report.restricted_schema_in_use.len(),
            "should have found restricted connect feature"
        );
        if let SchemaViolation::Spec { url, name } = &report.restricted_schema_in_use[0] {
            assert_eq!("https://specs.apollo.dev/connect/v0.1", url);
            assert_eq!("connect", name);
        } else {
            panic!("should have reported connect feature violation")
        }
    }

    // TODO-Elllie: is this the correct behavior for connectors?
    #[test]
    fn schema_enforcement_with_allowed_features_containing_connectors() {
        /*
         * GIVEN
         *  - a valid license whose `allowed_features` claim contains connectors
         *  - a valid config
         *  - a valid schema
         * */
        let license_with_feature = LicenseState::Licensed {
            limits: Some(LicenseLimits {
                tps: None,
                allowed_features: Some(HashSet::from_iter(vec![AllowedFeature::RestConnectors])),
            }),
        };
        /*
         * WHEN
         *  - the license enforcement report is built
         * */
        let report = check(
            include_str!("testdata/oss.router.yaml"),
            include_str!("testdata/schema_enforcement_connectors.graphql"),
            license_with_feature,
        );

        /*
         * THEN
         *  - since the feature is part of the `allowed_features` set
         *    the feature should not be contained within the report
         * */
        assert_eq!(
            0,
            report.restricted_schema_in_use.len(),
            "should have not found any restricted schema"
        );
    }

    #[test]
    fn schema_enforcement_with_allowed_features_not_containing_connectors() {
        /*
         * GIVEN
         *  - a valid license whose `allowed_features` claim does not contain connectors
         *  - a valid config
         *  - a valid schema
         * */
        let license_without_feature = LicenseState::Licensed {
            limits: Some(LicenseLimits {
                tps: None,
                allowed_features: Some(HashSet::from_iter(vec![AllowedFeature::Subscriptions])),
            }),
        };
        /*
         * WHEN
         *  - the license enforcement report is built
         * */
        let report = check(
            include_str!("testdata/oss.router.yaml"),
            include_str!("testdata/schema_enforcement_connectors.graphql"),
            license_without_feature,
        );

        /*
         * THEN
         *  - since connectors is not part of the `allowed_features` set
         *    the feature should not be contained within the report
         * */
        assert_eq!(
            1,
            report.restricted_schema_in_use.len(),
            "should have found restricted connect feature"
        );
        if let SchemaViolation::Spec { url, name } = &report.restricted_schema_in_use[0] {
            assert_eq!("https://specs.apollo.dev/connect/v0.1", url);
            assert_eq!("connect", name);
        } else {
            panic!("should have reported connect feature violation")
        }
    }

    // TODO-Ellie: is this correct behavior?
    #[test]
    fn schema_enforcement_with_allowed_features_containing_directive_arguments() {
        /*
         * GIVEN
         *  - a valid license whose `allowed_features` claim includes the overrideLabel directive argument
         *  - a valid config
         *  - a valid schema
         * */
        let license_with_feature = LicenseState::Licensed {
            limits: Some(LicenseLimits {
                tps: None,
                allowed_features: Some(HashSet::from_iter(vec![
                    AllowedFeature::DemandControl,
                    AllowedFeature::FederationOverrideLabel,
                ])),
            }),
        };
        /*
         * WHEN
         *  - the license enforcement report is built
         * */
        let report = check(
            include_str!("testdata/oss.router.yaml"),
            include_str!("testdata/schema_enforcement_directive_arg_version_in_range.graphql"),
            license_with_feature,
        );

        /*
         * THEN
         *  - since the feature is part of the `allowed_features` set
         *    the feature should not be contained within the report
         * */
        assert_eq!(
            0,
            report.restricted_schema_in_use.len(),
            "should have not found any restricted schema"
        );
    }

    // TODO-Ellie: is this correct behavior for authentication?
    #[test]
    fn schema_enforcement_with_allowed_features_not_containing_directive_arguments() {
        /*
         * GIVEN
         *  - a valid license whose `allowed_features` claim does not permit the overrideLabel directive argument
         *  - a valid config
         *  - a valid schema
         * */
        let license_without_feature = LicenseState::Licensed {
            limits: Some(LicenseLimits {
                tps: None,
                allowed_features: Some(HashSet::from_iter(vec![AllowedFeature::Subscriptions])),
            }),
        };
        /*
         * WHEN
         *  - the license enforcement report is built
         * */
        let report = check(
            include_str!("testdata/oss.router.yaml"),
            include_str!("testdata/schema_enforcement_directive_arg_version_in_range.graphql"),
            license_without_feature,
        );

        /*
         * THEN
         *  - the feature should be contained within the report
         * */
        assert_eq!(
            1,
            report.restricted_schema_in_use.len(),
            "should have found restricted directive argument"
        );
        if let SchemaViolation::DirectiveArgument { url, name, .. } =
            &report.restricted_schema_in_use[0]
        {
            assert_eq!("https://specs.apollo.dev/join/v0.4", url,);
            assert_eq!("join__field", name);
        } else {
            panic!("should have reported directive argument violation")
        }
    }

    #[test]
    fn schema_enforcement_with_allowed_features_containing_authentication() {
        /*
         * GIVEN
         *  - a valid license whose `allowed_features` claim includes authentication
         *  - a valid config
         *  - a valid schema
         * */
        let license_with_feature = LicenseState::Licensed {
            limits: Some(LicenseLimits {
                tps: None,
                allowed_features: Some(HashSet::from_iter(vec![
                    AllowedFeature::Subscriptions,
                    AllowedFeature::Authentication,
                    AllowedFeature::FederationContextArguments,
                ])),
            }),
        };
        /*
         * WHEN
         *  - the license enforcement report is built
         * */
        let report = check(
            include_str!("testdata/oss.router.yaml"),
            include_str!("testdata/authorization.graphql"),
            license_with_feature,
        );

        /*
         * THEN
         *  - since the feature is part of the `allowed_features` set
         *    the feature should not be contained within the report
         * */
        assert_eq!(
            0,
            report.restricted_schema_in_use.len(),
            "should have not found any restricted schema"
        );
    }

    // TODO-Ellie: is this correct behavior for authentication?
    #[test]
    fn schema_enforcement_with_allowed_features_not_containing_authentication() {
        /*
         * GIVEN
         *  - a valid license whose `allowed_features` claim does not permit authentication
         *  - a valid config
         *  - a valid schema
         * */
        let license_without_feature = LicenseState::Licensed {
            limits: Some(LicenseLimits {
                tps: None,
                allowed_features: Some(HashSet::from_iter(vec![])),
            }),
        };
        /*
         * WHEN
         *  - the license enforcement report is built
         * */
        let report = check(
            include_str!("testdata/oss.router.yaml"),
            include_str!("testdata/authorization.graphql"),
            license_without_feature,
        );

        /*
         * THEN
         *  - the feature used in the schema should be contained within the report:
         *    requiresScopes and context
         * */
        assert_eq!(
            2,
            report.restricted_schema_in_use.len(),
            "should have found restricted features"
        );

        if let SchemaViolation::Spec { url, name, .. } = &report.restricted_schema_in_use[0] {
            assert_eq!("https://specs.apollo.dev/authenticated/v0.1", url,);
            assert_eq!("authenticated", name);
        } else {
            panic!("should have found 2 violations")
        }
        if let SchemaViolation::Spec { url, name, .. } = &report.restricted_schema_in_use[1] {
            assert_eq!("https://specs.apollo.dev/requiresScopes/v0.1", url,);
            assert_eq!("requiresScopes", name);
        } else {
            panic!("should have found 2 violations")
        }
    }
}
