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
use serde::de::Visitor;
use serde_json::Value;
use strum::IntoEnumIterator;
use strum_macros::EnumIter;
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
    #[serde(rename = "allowedFeatures")]
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
    restricted_schema_startup_in_use: Vec<SchemaStartupRestriction>,
}

impl LicenseEnforcementReport {
    pub(crate) fn uses_restricted_features(&self) -> bool {
        !self.restricted_config_in_use.is_empty()
            || !self.restricted_schema_in_use.is_empty()
            || !self.restricted_schema_startup_in_use.is_empty()
    }

    pub(crate) fn build(
        configuration: &Configuration,
        schema: &Schema,
        license: &LicenseState,
        schema_state: &crate::uplink::schema::SchemaState,
    ) -> LicenseEnforcementReport {
        LicenseEnforcementReport {
            restricted_config_in_use: Self::validate_configuration(
                configuration,
                &Self::configuration_restrictions(license),
            ),
            restricted_schema_in_use: Self::validate_schema(
                schema,
                &Self::schema_restrictions(license),
            ),
            restricted_schema_startup_in_use: Self::validate_schema_startup(schema_state, license),
        }
    }

    pub(crate) fn restricted_features_in_use(&self) -> Vec<String> {
        let mut restricted_features_in_use = Vec::new();
        for restricted_config_in_use in self.restricted_config_in_use.clone() {
            restricted_features_in_use.push(restricted_config_in_use.name.clone());
        }
        for restricted_schema_in_use in self.restricted_schema_in_use.clone() {
            match restricted_schema_in_use {
                SchemaViolation::Spec { name, .. } => {
                    restricted_features_in_use.push(name.clone());
                }
                SchemaViolation::DirectiveArgument { name, .. } => {
                    restricted_features_in_use.push(name.clone());
                }
            }
        }
        for restricted_schema_startup_in_use in self.restricted_schema_startup_in_use.clone() {
            match restricted_schema_startup_in_use {
                SchemaStartupRestriction::ExternalRegistry { explanation } => {
                    restricted_features_in_use.push(explanation);
                }
            }
        }
        restricted_features_in_use
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

        for restriction in schema_restrictions {
            match restriction {
                SchemaRestriction::Spec {
                    spec_url,
                    name,
                    version_req,
                } => {
                    if let Some(link_spec) = link_specs.get(spec_url)
                        && version_req.matches(&link_spec.version)
                    {
                        schema_violations.push(SchemaViolation::Spec {
                            url: link_spec.url.to_string(),
                            name: name.to_string(),
                        });
                    }
                }
                SchemaRestriction::DirectiveArgument {
                    spec_url,
                    name,
                    version_req,
                    argument,
                    explanation,
                } => {
                    if let Some(link_spec) = link_specs.get(spec_url)
                        && version_req.matches(&link_spec.version)
                    {
                        let directive_name = link_spec.directive_name(name);
                        if schema
                            .supergraph_schema()
                            .types
                            .values()
                            .flat_map(|def| match def {
                                // To traverse additional directive locations, add match arms for the respective definition types required.
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
                SchemaRestriction::SpecInJoinDirective {
                    spec_url,
                    name,
                    version_req,
                } => {
                    if let Some(link_spec) = link_specs_in_join_directive.get(spec_url)
                        && version_req.matches(&link_spec.version)
                    {
                        schema_violations.push(SchemaViolation::Spec {
                            url: link_spec.url.to_string(),
                            name: name.to_string(),
                        });
                    }
                }
            }
        }

        schema_violations
    }

    fn validate_schema_startup(
        schema_state: &crate::uplink::schema::SchemaState,
        license: &LicenseState,
    ) -> Vec<SchemaStartupRestriction> {
        let mut schema_startup_violations = Vec::new();
        let allowed_features = license.get_allowed_features();

        // Check external registry usage if not allowed by license
        if !allowed_features.contains(&AllowedFeature::GraphArtifactExternalRegistry)
            && schema_state.is_external_registry
        {
            schema_startup_violations.push(SchemaStartupRestriction::ExternalRegistry {
                explanation: "External registries are only available with an enterprise license"
                    .to_string(),
            });
        }

        schema_startup_violations
    }

    fn configuration_restrictions(license: &LicenseState) -> Vec<ConfigurationRestriction> {
        let mut configuration_restrictions = vec![];

        let allowed_features = license.get_allowed_features();
        if !allowed_features.contains(&AllowedFeature::ApqCaching) {
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
        if !allowed_features.contains(&AllowedFeature::PersistedQueries) {
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
        }
        if !allowed_features.contains(&AllowedFeature::Coprocessors) {
            configuration_restrictions.push(
                ConfigurationRestriction::builder()
                    .path("$.coprocessor")
                    .name("Coprocessor plugin")
                    .build(),
            )
        }
        if !allowed_features.contains(&AllowedFeature::DistributedQueryPlanning) {
            configuration_restrictions.push(
                ConfigurationRestriction::builder()
                    .path("$.supergraph.query_planning.cache.redis")
                    .name("Query plan caching")
                    .build(),
            )
        }
        if !allowed_features.contains(&AllowedFeature::DemandControl) {
            configuration_restrictions.push(
                ConfigurationRestriction::builder()
                    .path("$.demand_control")
                    .name("Demand control plugin")
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
        // Per-operation limits are restricted but parser limits like `parser_max_recursion`
        // where the Router only configures apollo-rs are not.
        if !allowed_features.contains(&AllowedFeature::RequestLimits) {
            configuration_restrictions.extend(vec![
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
            ]);
        }

        configuration_restrictions
    }

    fn schema_restrictions(license: &LicenseState) -> Vec<SchemaRestriction> {
        let mut schema_restrictions = vec![];
        let allowed_features = license.get_allowed_features();

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

            write!(f, "Schema features:\n{restricted_schema}")?;
        }

        if !self.restricted_schema_startup_in_use.is_empty() {
            let restricted_schema_startup = self
                .restricted_schema_startup_in_use
                .iter()
                .map(|v| v.to_string())
                .join("\n\n");

            if !self.restricted_config_in_use.is_empty()
                || !self.restricted_schema_in_use.is_empty()
            {
                writeln!(f)?;
            }
            write!(
                f,
                "Schema startup restrictions:\n{restricted_schema_startup}"
            )?;
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

/// Allowed features for a License, representing what's available to a particular pricing tier
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Hash, EnumIter)]
#[serde(rename_all = "snake_case")]
pub enum AllowedFeature {
    /// Automated persisted queries
    Apq,
    /// APQ caching
    ApqCaching,
    /// Authentication plugin
    Authentication,
    /// Authorization directives
    Authorization,
    /// Batching support
    Batching,
    /// Coprocessor plugin
    Coprocessors,
    /// Demand control plugin
    DemandControl,
    /// Distributed query planning
    DistributedQueryPlanning,
    /// Subgraph entity caching
    EntityCaching,
    /// Experimental features in the router
    Experimental,
    /// Extended reference reporting
    ExtendedReferenceReporting,
    /// Graph artifact from external registry
    GraphArtifactExternalRegistry,
    /// Persisted queries safelisting
    PersistedQueries,
    /// Request limits - depth and breadth
    RequestLimits,
    /// Federated subscriptions
    Subscriptions,
    /// Traffic shaping
    TrafficShaping,
    /// This represents a feature found in the license that the router does not recognize
    Other(String),
}

impl From<&str> for AllowedFeature {
    fn from(feature: &str) -> Self {
        match feature {
            "apq" => Self::Apq,
            "apq_caching" => Self::ApqCaching,
            "authentication" => Self::Authentication,
            "authorization" => Self::Authorization,
            "batching" => Self::Batching,
            "coprocessors" => Self::Coprocessors,
            "demand_control" => Self::DemandControl,
            "distributed_query_planning" => Self::DistributedQueryPlanning,
            "entity_caching" => Self::EntityCaching,
            "experimental" => Self::Experimental,
            "extended_reference_reporting" => Self::ExtendedReferenceReporting,
            "persisted_queries" => Self::PersistedQueries,
            "request_limits" => Self::RequestLimits,
            "subscriptions" => Self::Subscriptions,
            "traffic_shaping" => Self::TrafficShaping,
            "graph_artifact_external_registry" => Self::GraphArtifactExternalRegistry,
            other => Self::Other(other.into()),
        }
    }
}

impl AllowedFeature {
    /// Creates an allowed feature from a plugin name
    pub fn from_plugin_name(plugin_name: &str) -> Option<AllowedFeature> {
        match plugin_name {
            "traffic_shaping" => Some(AllowedFeature::TrafficShaping),
            "limits" => Some(AllowedFeature::RequestLimits),
            "subscription" => Some(AllowedFeature::Subscriptions),
            "authorization" => Some(AllowedFeature::Authorization),
            "authentication" => Some(AllowedFeature::Authentication),
            "preview_entity_cache" => Some(AllowedFeature::EntityCaching),
            "demand_control" => Some(AllowedFeature::DemandControl),
            "coprocessor" => Some(AllowedFeature::Coprocessors),
            _other => None,
        }
    }
}

impl<'de> Deserialize<'de> for AllowedFeature {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct AllowedFeatureVisitor;

        impl<'de> Visitor<'de> for AllowedFeatureVisitor {
            type Value = AllowedFeature;

            fn expecting(&self, formatter: &mut Formatter) -> std::fmt::Result {
                formatter.write_str("a string representing an allowed feature")
            }

            fn visit_str<E>(self, value: &str) -> Result<AllowedFeature, E>
            where
                E: serde::de::Error,
            {
                Ok(AllowedFeature::from(value))
            }
        }

        deserializer.deserialize_str(AllowedFeatureVisitor)
    }
}

/// LicenseLimits represent what can be done with a router based on the claims in the License. You
/// might have a certain tier be limited in its capacity for transactions over a certain duration,
/// as an example
#[derive(Debug, Builder, Clone, Eq, PartialEq)]
pub struct LicenseLimits {
    /// Transaction Per Second limits. If none are found in the License's claims, there are no
    /// limits to apply
    pub(crate) tps: Option<TpsLimit>,
    /// The allowed features based on the allowed features present on the License's claims
    pub(crate) allowed_features: HashSet<AllowedFeature>,
}

impl Default for LicenseLimits {
    fn default() -> Self {
        Self {
            tps: None,
            allowed_features: HashSet::from_iter(AllowedFeature::iter()),
        }
    }
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

impl LicenseState {
    pub(crate) fn get_limits(&self) -> Option<&LicenseLimits> {
        match self {
            LicenseState::Licensed { limits }
            | LicenseState::LicensedWarn { limits }
            | LicenseState::LicensedHalt { limits } => limits.as_ref(),
            _ => None,
        }
    }

    pub(crate) fn get_allowed_features(&self) -> HashSet<AllowedFeature> {
        match self {
            LicenseState::Licensed { limits }
            | LicenseState::LicensedWarn { limits }
            | LicenseState::LicensedHalt { limits } => {
                match limits {
                    Some(limits) => limits.allowed_features.clone(),
                    // If the license has no limits and therefore no allowed_features claim,
                    // we're using a pricing plan that should have the feature enabled regardless.
                    // NB: This is temporary behavior and will be updated once all licenses contain
                    // an allowed_features claim.
                    None => HashSet::from_iter(AllowedFeature::iter()),
                }
            }
            // If we are using an expired license or an unlicesed router we return an empty feature set
            LicenseState::Unlicensed => HashSet::new(),
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
    // Note: this restriction is currently unused, but it's intention was to
    // traverse directives belonging to object types and their fields. It was used for
    // progressive overrides when they were gated to enterprise-only. Leaving it here for now
    // in case other directives become gated by subscription tier (there's at least one in the
    // works that's non-free)
    #[allow(dead_code)]
    DirectiveArgument {
        spec_url: String,
        name: String,
        version_req: semver::VersionReq,
        argument: String,
        explanation: String,
    },
    // Note: this restriction is currently unused.
    // It was used for connectors when they were gated to license-only. Leaving it here for now
    // in case other directives become gated by subscription tier
    #[allow(dead_code)]
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
                write!(f, "* @{name}\n  {url}")
            }
            SchemaViolation::DirectiveArgument {
                name,
                url,
                argument,
                explanation,
            } => {
                write!(f, "* @{name}.{argument}\n  {url}\n\n{explanation}")
            }
        }
    }
}

/// An individual check for schema startup restrictions (e.g., external registry usage)
#[derive(Debug, Clone)]
pub(crate) enum SchemaStartupRestriction {
    ExternalRegistry { explanation: String },
}

impl Display for SchemaStartupRestriction {
    fn fmt(&self, f: &mut Formatter) -> std::fmt::Result {
        match self {
            SchemaStartupRestriction::ExternalRegistry { explanation } => {
                write!(f, "* External registry usage\n  {explanation}")
            }
        }
    }
}

impl License {
    pub(crate) fn jwks() -> &'static JwkSet {
        JWKS.get_or_init(|| {
            // Strip the comments from the top of the file.
            let re = Regex::new("(?m)^//.*$").expect("regex must be valid");
            // We have a set of test JWTs that use this dummy JWKS endpoint. See the internal docs
            // of the router team for details on how to mint a dummy JWT for testing
            let jwks = if let Ok(jwks_path) = std::env::var("APOLLO_TEST_INTERNAL_UPLINK_JWKS") {
                tracing::debug!("using a dummy JWKS endpoint: {jwks_path:?}");
                let jwks = std::fs::read_to_string(jwks_path)
                    .expect("dummy JWKS endpoint couldn't be read into memory");
                re.replace(&jwks, "").into_owned()
            } else {
                re.replace(include_str!("license.jwks.json"), "")
                    .into_owned()
            };

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

    #[track_caller]
    fn check(
        router_yaml: &str,
        supergraph_schema: &str,
        license: LicenseState,
    ) -> LicenseEnforcementReport {
        let config = Configuration::from_str(router_yaml).expect("router config must be valid");
        let schema =
            Schema::parse(supergraph_schema, &config).expect("supergraph schema must be valid");
        let schema_state = crate::uplink::schema::SchemaState {
            sdl: supergraph_schema.to_string(),
            launch_id: None,
            is_external_registry: false,
        };

        LicenseEnforcementReport::build(&config, &schema, &license, &schema_state)
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
                    allowed_features: HashSet::from_iter(vec![]),
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
                    allowed_features: HashSet::from_iter(vec![
                        AllowedFeature::Authentication,
                        AllowedFeature::Authorization,
                        AllowedFeature::Batching,
                        AllowedFeature::DemandControl,
                        AllowedFeature::EntityCaching,
                        AllowedFeature::PersistedQueries,
                        AllowedFeature::ApqCaching,
                    ]),
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
    fn test_restricted_authorization_directives_via_schema_with_restricted_allowed_features() {
        // When auth is contained within the allowed features set
        // we should not find any schema violations in the report
        let report = check(
            include_str!("testdata/oss.router.yaml"),
            include_str!("testdata/authorization.graphql"),
            LicenseState::Licensed {
                limits: Some(LicenseLimits {
                    tps: None,
                    allowed_features: HashSet::from_iter(vec![
                        AllowedFeature::Authentication,
                        AllowedFeature::Authorization,
                    ]),
                }),
            },
        );
        assert!(
            report.restricted_schema_in_use.is_empty(),
            "should have not found restricted features"
        );

        // When auth is not contained within the allowed features set
        // we should find schema violations in the report
        let report = check(
            include_str!("testdata/oss.router.yaml"),
            include_str!("testdata/authorization.graphql"),
            LicenseState::Licensed {
                limits: Some(LicenseLimits {
                    tps: None,
                    allowed_features: HashSet::from_iter(vec![AllowedFeature::DemandControl]),
                }),
            },
        );
        assert!(
            !report.restricted_schema_in_use.is_empty(),
            "should have found restricted features"
        );
        assert_snapshot!(report.to_string());
    }

    // NB: this behavior will change once all licenses have an `allowed_features` claim
    #[test]
    fn test_restricted_authorization_directives_via_schema_with_default_license_limits() {
        let report = check(
            include_str!("testdata/oss.router.yaml"),
            include_str!("testdata/authorization.graphql"),
            LicenseState::Licensed {
                limits: Default::default(),
            },
        );

        assert!(
            report.restricted_schema_in_use.is_empty(),
            "should have not found restricted features"
        );
    }

    #[test]
    #[cfg(not(windows))] // http::uri::Uri parsing appears to reject unix:// on Windows
    fn unix_socket_available_to_oss() {
        let report = check(
            include_str!("testdata/oss.router.yaml"),
            include_str!("testdata/unix_socket.graphql"),
            LicenseState::default(),
        );

        assert!(
            report.restricted_schema_in_use.is_empty(),
            "shouldn't have found restricted features"
        );
    }

    #[test]
    fn schema_enforcement_allows_context_directive_for_oss() {
        let report = check(
            include_str!("testdata/oss.router.yaml"),
            include_str!("testdata/set_context.graphql"),
            LicenseState::default(),
        );

        assert!(
            report.restricted_schema_in_use.is_empty(),
            "shouldn't have found restricted features"
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
                    allowed_features: HashSet::new(),
                }),
            },
        );

        assert!(
            report.restricted_schema_in_use.is_empty(),
            "shouldn't have found restricted features"
        );
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
                allowed_features: Default::default(),
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
                allowed_features: Default::default(),
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
    fn progressive_override_available_to_oss() {
        let report = check(
            include_str!("testdata/oss.router.yaml"),
            include_str!("testdata/progressive_override.graphql"),
            LicenseState::default(),
        );

        // progressive override is available for oss
        assert!(
            report.restricted_schema_in_use.is_empty(),
            "shouldn't have found restricted features"
        );
    }

    #[test]
    fn set_context() {
        let report = check(
            include_str!("testdata/oss.router.yaml"),
            include_str!("testdata/set_context.graphql"),
            LicenseState::default(),
        );

        assert!(
            report.restricted_schema_in_use.is_empty(),
            "shouldn't have found restricted features"
        );
    }

    #[test]
    fn progressive_override_with_renamed_join_spec() {
        let report = check(
            include_str!("testdata/oss.router.yaml"),
            include_str!("testdata/progressive_override_renamed_join.graphql"),
            LicenseState::default(),
        );

        assert!(
            report.restricted_schema_in_use.is_empty(),
            "shouldn't have found restricted features"
        );
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
            report.restricted_schema_in_use.is_empty(),
            "shouldn't have found restricted features"
        );
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

    #[test]
    fn schema_enforcement_connectors() {
        let report = check(
            include_str!("testdata/oss.router.yaml"),
            include_str!("testdata/schema_enforcement_connectors.graphql"),
            LicenseState::default(),
        );

        assert!(
            report.restricted_schema_in_use.is_empty(),
            "shouldn't have found restricted connect feature"
        );
    }

    #[test]
    fn test_check_startup_schema_license_violations_external_registry_unlicensed() {
        use std::sync::Arc;

        use crate::spec::Schema;
        use crate::uplink::schema::SchemaState;

        // Test external registry with unlicensed state
        let schema_state = SchemaState {
            sdl: "type Query { hello: String }".to_string(),
            launch_id: None,
            is_external_registry: true,
        };

        let schema = Schema::parse_arc(
            Arc::new(schema_state.clone()),
            &crate::configuration::Configuration::builder()
                .build()
                .unwrap(),
        )
        .unwrap();
        let report = LicenseEnforcementReport::build(
            &crate::configuration::Configuration::builder()
                .build()
                .unwrap(),
            &schema,
            &LicenseState::Unlicensed,
            &schema_state,
        );

        // Should detect external registry violation
        assert!(report.uses_restricted_features());
        assert!(!report.restricted_schema_startup_in_use.is_empty());
    }

    #[test]
    fn test_check_startup_schema_license_violations_external_registry_licensed() {
        use std::collections::HashSet;
        use std::sync::Arc;

        use crate::spec::Schema;
        use crate::uplink::schema::SchemaState;

        // Test external registry with licensed state that includes the feature
        let schema_state = SchemaState {
            sdl: "type Query { hello: String }".to_string(),
            launch_id: None,
            is_external_registry: true,
        };

        let limits = LicenseLimits {
            tps: None,
            allowed_features: HashSet::from_iter(vec![
                AllowedFeature::GraphArtifactExternalRegistry,
            ]),
        };
        let license_state = LicenseState::Licensed {
            limits: Some(limits),
        };

        let schema = Schema::parse_arc(
            Arc::new(schema_state.clone()),
            &crate::configuration::Configuration::builder()
                .build()
                .unwrap(),
        )
        .unwrap();
        let report = LicenseEnforcementReport::build(
            &crate::configuration::Configuration::builder()
                .build()
                .unwrap(),
            &schema,
            &license_state,
            &schema_state,
        );

        // Should not detect external registry violation with proper license
        assert!(!report.uses_restricted_features());
        assert!(report.restricted_schema_startup_in_use.is_empty());
    }

    #[test]
    fn test_check_startup_schema_license_violations_apollo_registry_unlicensed() {
        use std::sync::Arc;

        use crate::spec::Schema;
        use crate::uplink::schema::SchemaState;

        // Test Apollo registry with unlicensed state (should be allowed)
        let schema_state = SchemaState {
            sdl: "type Query { hello: String }".to_string(),
            launch_id: None,
            is_external_registry: false, // Apollo registry is not external
        };

        let schema = Schema::parse_arc(
            Arc::new(schema_state.clone()),
            &crate::configuration::Configuration::builder()
                .build()
                .unwrap(),
        )
        .unwrap();
        let report = LicenseEnforcementReport::build(
            &crate::configuration::Configuration::builder()
                .build()
                .unwrap(),
            &schema,
            &LicenseState::Unlicensed,
            &schema_state,
        );

        // Should not detect external registry violation for Apollo registry
        assert!(!report.uses_restricted_features());
        assert!(report.restricted_schema_startup_in_use.is_empty());
    }
}
