use std::collections::BTreeMap;
use std::num::NonZeroUsize;
use std::time::Duration;

use schemars::JsonSchema;
use serde::Deserialize;

#[derive(Clone, Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct PolicyConfig {
    /// Enables native policy provider evaluation. When false, the configuration is retained but providers are not built or called.
    #[serde(default = "default_enabled")]
    pub(crate) enabled: bool,
    /// Named policy providers available for evaluating `@policy` labels.
    pub(super) providers: BTreeMap<String, ProviderConfig>,
    /// Routes policy labels to configured providers.
    pub(super) routing: RoutingConfig,
    /// Controls behavior when a provider cannot evaluate a policy.
    #[serde(default)]
    pub(super) failure: FailureConfig,
}

#[derive(Clone, Debug, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub(super) enum ProviderConfig {
    Opa(OpaConfig),
}

#[derive(Clone, Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct OpaConfig {
    /// Configures the OPA Data API decision queried by the router.
    pub(super) api: OpaApiConfig,
    /// Equivalent OPA service endpoints used for load balancing and retries.
    pub(super) endpoints: Vec<EndpointConfig>,
    /// Configures communication with the OPA service endpoints.
    #[serde(default)]
    pub(super) transport: TransportConfig,
    /// Selects request data included in the OPA policy input document.
    #[serde(default)]
    pub(super) input: InputConfig,
}

#[derive(Clone, Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct OpaApiConfig {
    /// OPA Data API decision path, without `/v1/data/`.
    pub(super) decision: String,
}

#[derive(Clone, Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct EndpointConfig {
    /// OPA base URL. Supports HTTP(S) and Unix sockets (`unix:///path.sock?path=/http-base`).
    pub(super) url: String,
}

#[derive(Clone, Debug, Default, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub(super) struct TransportConfig {
    /// HTTP client connection, DNS, HTTP/2, and pool settings for provider calls.
    #[serde(default)]
    pub(super) client: crate::configuration::shared::Client,
    /// Selects how requests are distributed across equivalent endpoints.
    pub(super) load_balancing: LoadBalancingConfig,
    /// Limits the total time available for a policy evaluation.
    pub(super) timeouts: TimeoutConfig,
    /// Controls retry attempts across equivalent endpoints.
    pub(super) retry: RetryConfig,
    /// Static HTTP headers included in every request to this provider.
    pub(super) headers: BTreeMap<String, String>,
}

#[derive(Clone, Debug, Default, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub(super) struct LoadBalancingConfig {
    /// Strategy used to select an endpoint for each initial request and retry.
    pub(super) strategy: LoadBalancingStrategy,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub(super) enum LoadBalancingStrategy {
    /// Select endpoints in sequence, skipping endpoints that are temporarily unhealthy.
    #[default]
    RoundRobin,
}

#[derive(Clone, Debug, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub(super) struct TimeoutConfig {
    /// Maximum duration for the complete evaluation, including input construction and all attempts.
    #[serde(deserialize_with = "humantime_serde::deserialize")]
    #[schemars(with = "String")]
    pub(super) total: Duration,
}

impl Default for TimeoutConfig {
    fn default() -> Self {
        Self {
            total: Duration::from_millis(250),
        }
    }
}

#[derive(Clone, Debug, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub(super) struct RetryConfig {
    /// Maximum total number of attempts, including the initial request.
    pub(super) max_attempts: NonZeroUsize,
}

impl Default for RetryConfig {
    fn default() -> Self {
        Self {
            max_attempts: NonZeroUsize::new(2).expect("two is non-zero"),
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub(super) struct InputConfig {
    /// Selects authenticated JWT claim names included in policy input. Empty by default.
    pub(super) claims: IncludeConfig,
    /// Selects request headers included in the policy input.
    pub(super) headers: IncludeConfig,
    /// Selects GraphQL variables included in the policy input.
    pub(super) variables: IncludeConfig,
    /// Selects router context values included in the policy input.
    pub(super) context: IncludeConfig,
}

#[derive(Clone, Debug, Default, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub(super) struct IncludeConfig {
    /// Explicit names to include. Values are never wildcard-expanded.
    pub(super) include: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct RoutingConfig {
    /// Provider used when no routing rule matches a policy label.
    pub(super) default: RouteTarget,
    /// Ordered policy-label routing rules.
    #[serde(default)]
    pub(super) rules: Vec<RouteRule>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct RouteRule {
    /// Exact labels and prefixes matched by this rule.
    pub(super) r#match: RouteMatch,
    /// Provider that evaluates labels matched by this rule.
    pub(super) target: RouteTarget,
}

#[derive(Clone, Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct RouteMatch {
    /// Policy labels that must match exactly.
    #[serde(default)]
    pub(super) exact: Vec<String>,
    /// Policy label prefixes matched using longest-prefix precedence.
    #[serde(default)]
    pub(super) prefix: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct RouteTarget {
    /// Name of the provider to call.
    pub(super) provider: String,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub(super) enum FailureMode {
    /// Reject the operation with a provider evaluation error.
    #[default]
    Reject,
    /// Continue authorization by denying every requested policy.
    Deny,
}

#[derive(Clone, Debug, Default, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub(super) struct FailureConfig {
    /// Provider failures either reject the request or deny all policies.
    pub(super) mode: FailureMode,
}

fn default_enabled() -> bool {
    true
}
