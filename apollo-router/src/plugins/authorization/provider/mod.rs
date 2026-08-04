//! Native policy providers for `@policy`.

use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::fmt;

use futures::future::try_join_all;
use tower::BoxError;

use crate::Context;
use crate::plugins::authorization::REQUIRED_POLICIES_KEY;
use crate::services::supergraph;

mod config;
mod opa;
mod service;
#[cfg(test)]
mod tests;

pub(crate) use config::PolicyConfig;
use config::*;
use opa::OpaProvider;
pub(crate) use service::PolicyProviderLayer;

#[derive(Clone, Debug, Default)]
pub(crate) struct ProviderPolicyDecisions(BTreeMap<String, bool>);

impl ProviderPolicyDecisions {
    fn required(context: &Context) -> BTreeSet<String> {
        context
            .get_json_value(REQUIRED_POLICIES_KEY)
            .and_then(|value| {
                value.as_object().map(|policies| {
                    policies
                        .keys()
                        .map(|policy| policy.as_str().to_string())
                        .collect()
                })
            })
            .unwrap_or_default()
    }

    fn deny_all(policies: BTreeSet<String>) -> Self {
        Self(policies.into_iter().map(|policy| (policy, false)).collect())
    }

    fn store(&self, context: &Context) -> Result<(), BoxError> {
        context.insert_json_value(REQUIRED_POLICIES_KEY, serde_json_bytes::to_value(&self.0)?);
        context
            .extensions()
            .with_lock(|lock| lock.insert(self.clone()));
        Ok(())
    }

    pub(crate) fn allowed_policies(&self) -> Vec<String> {
        self.0
            .iter()
            .filter_map(|(policy, allowed)| allowed.then_some(policy.clone()))
            .collect()
    }
}

pub(crate) struct PolicyProviderRegistry {
    providers: BTreeMap<String, PolicyProvider>,
    routing: RoutingTable,
    failure_mode: FailureMode,
}

enum PolicyProvider {
    Opa(OpaProvider),
}

struct RoutingTable {
    default_provider: String,
    exact_routes: BTreeMap<String, String>,
    prefix_routes: Vec<(String, String)>,
}

struct ProviderEvaluationError {
    provider_name: String,
    source: BoxError,
}

impl fmt::Display for ProviderEvaluationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.source.fmt(formatter)
    }
}

impl fmt::Debug for ProviderEvaluationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, formatter)
    }
}

impl std::error::Error for ProviderEvaluationError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(self.source.as_ref())
    }
}

impl PolicyProviderRegistry {
    pub(crate) fn new(config: PolicyConfig) -> Result<Self, BoxError> {
        if config.providers.is_empty() {
            return Err("authorization.policy.providers must not be empty".into());
        }
        let routing = RoutingTable::new(&config.routing, &config.providers)?;

        let mut providers = BTreeMap::new();
        for (name, provider) in config.providers {
            providers.insert(name.clone(), PolicyProvider::new(name, provider)?);
        }

        Ok(Self {
            providers,
            routing,
            failure_mode: config.failure.mode,
        })
    }

    pub(crate) async fn evaluate(
        &self,
        request: &supergraph::Request,
        policies: BTreeSet<String>,
    ) -> Result<ProviderPolicyDecisions, BoxError> {
        let mut grouped: BTreeMap<&str, BTreeSet<String>> = BTreeMap::new();
        for policy in policies {
            grouped
                .entry(self.routing.provider_for(&policy))
                .or_default()
                .insert(policy);
        }

        let calls = grouped
            .into_iter()
            .map(|(provider_name, policies)| async move {
                self.providers[provider_name]
                    .evaluate(request, policies)
                    .await
                    .map_err(|source| {
                        Box::new(ProviderEvaluationError {
                            provider_name: provider_name.to_string(),
                            source,
                        }) as BoxError
                    })
            });

        let mut decisions = BTreeMap::new();
        for provider_decisions in try_join_all(calls).await? {
            decisions.extend(provider_decisions);
        }
        Ok(ProviderPolicyDecisions(decisions))
    }

    fn failure_mode(&self) -> FailureMode {
        self.failure_mode
    }

    fn provider_name_for_error(error: &BoxError) -> &str {
        error
            .downcast_ref::<ProviderEvaluationError>()
            .map(|error| error.provider_name.as_str())
            .unwrap_or("unknown")
    }
}

impl PolicyProvider {
    fn new(name: String, config: ProviderConfig) -> Result<Self, BoxError> {
        match config {
            ProviderConfig::Opa(config) => Ok(Self::Opa(OpaProvider::new(name, config)?)),
        }
    }

    async fn evaluate(
        &self,
        request: &supergraph::Request,
        policies: BTreeSet<String>,
    ) -> Result<BTreeMap<String, bool>, BoxError> {
        match self {
            Self::Opa(provider) => provider.evaluate(request, policies).await,
        }
    }
}

impl RoutingTable {
    fn new(
        config: &RoutingConfig,
        providers: &BTreeMap<String, ProviderConfig>,
    ) -> Result<Self, BoxError> {
        if !providers.contains_key(&config.default.provider) {
            return Err(format!(
                "unknown default policy provider `{}`",
                config.default.provider
            )
            .into());
        }

        let mut exact_routes = BTreeMap::new();
        let mut prefix_routes = BTreeMap::new();
        for rule in &config.rules {
            if !providers.contains_key(&rule.target.provider) {
                return Err(format!("unknown policy provider `{}`", rule.target.provider).into());
            }
            for policy in &rule.r#match.exact {
                if exact_routes
                    .insert(policy.clone(), rule.target.provider.clone())
                    .is_some()
                {
                    return Err(format!("policy `{policy}` is routed more than once").into());
                }
            }
            for prefix in &rule.r#match.prefix {
                if prefix.is_empty() {
                    return Err("policy route prefixes must not be empty".into());
                }
                if prefix_routes
                    .insert(prefix.clone(), rule.target.provider.clone())
                    .is_some()
                {
                    return Err(format!("policy prefix `{prefix}` is routed more than once").into());
                }
            }
            if rule.r#match.exact.is_empty() && rule.r#match.prefix.is_empty() {
                return Err(
                    "policy routing rules must match at least one exact label or prefix".into(),
                );
            }
        }
        let mut prefix_routes = prefix_routes.into_iter().collect::<Vec<_>>();
        // Longest prefix wins, so a narrow namespace can override a broad one.
        prefix_routes.sort_by(|(left, _), (right, _)| {
            right.len().cmp(&left.len()).then_with(|| left.cmp(right))
        });

        Ok(Self {
            default_provider: config.default.provider.clone(),
            exact_routes,
            prefix_routes,
        })
    }

    fn provider_for(&self, policy: &str) -> &str {
        self.exact_routes
            .get(policy)
            .or_else(|| {
                self.prefix_routes
                    .iter()
                    .find_map(|(prefix, provider)| policy.starts_with(prefix).then_some(provider))
            })
            .unwrap_or(&self.default_provider)
    }
}
