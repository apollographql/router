// Declare modules
mod config;
mod effective_config;
#[cfg(test)]
mod tests;

// Use items from modules
use std::sync::Arc;

use config::Config;
use config::ErrorMode;
use config::SubgraphConfig;
use effective_config::EffectiveConfig;
use tower::BoxError;
use tower::ServiceExt;

use crate::error::Error;
use crate::graphql;
use crate::json_ext::Object;
use crate::plugin::Plugin;
use crate::plugin::PluginInit;
use crate::register_plugin;
use crate::services::SupergraphResponse;
use crate::services::fetch::AddSubgraphNameExt;
use crate::services::fetch::SubgraphNameExt;
use crate::services::supergraph::BoxService;

static REDACTED_ERROR_MESSAGE: &str = "Subgraph errors redacted";

register_plugin!("apollo", "include_subgraph_errors", IncludeSubgraphErrors);

struct IncludeSubgraphErrors {
    // Store the calculated effective configuration
    config: Arc<EffectiveConfig>,
}

#[async_trait::async_trait]
impl Plugin for IncludeSubgraphErrors {
    type Config = Config; // Use Config from the config module

    async fn new(init: PluginInit<Self::Config>) -> Result<Self, BoxError> {
        // Validate that subgraph configs are boolean only if global config is boolean
        if let ErrorMode::Included(_) = &init.config.all {
            for (name, config) in &init.config.subgraphs {
                if !matches!(config, SubgraphConfig::Included(_)) {
                    return Err(format!(
                        "Subgraph '{name}' must use boolean config when global config is boolean",
                    )
                    .into());
                }
            }
        }

        // Generate and store the effective configuration
        let config = Arc::new(init.config.try_into()?);

        Ok(IncludeSubgraphErrors { config })
    }

    fn supergraph_service(&self, service: BoxService) -> BoxService {
        let config = Arc::clone(&self.config);

        service
            .map_response(move |response: SupergraphResponse| {
                response.map_stream(move |mut graphql_response: graphql::Response| {
                    for error in &mut graphql_response.errors {
                        Self::process_error(&config, error);
                    }
                    for incremental in &mut graphql_response.incremental {
                        for error in &mut incremental.errors {
                            Self::process_error(&config, error);
                        }
                    }

                    graphql_response
                })
            })
            .boxed()
    }

    fn subgraph_service(
        &self,
        subgraph_name: &str,
        service: crate::services::subgraph::BoxService,
    ) -> crate::services::subgraph::BoxService {
        // We need to attach the subgraph name to each error so that we can do the filtering in the supergraph service.
        // The reason filtering is not done here is that other types of request may also generate errors that need filtering.
        // Pushing the error filtering to supergraph will ensure that everything gets filtered.
        let subgraph_name = subgraph_name.to_string();
        service
            .map_response(move |mut r| {
                let body = r.response.body_mut();
                for error in &mut body.errors {
                    error.add_subgraph_name(&subgraph_name);
                }
                r
            })
            .boxed()
    }
}

impl IncludeSubgraphErrors {
    fn process_error(config: &Arc<EffectiveConfig>, error: &mut Error) {
        if let Some(subgraph_name) = error.subgraph_name() {
            // Get the effective config for this specific subgraph, or use default
            let effective_config = config
                .subgraphs
                .get(&subgraph_name)
                .unwrap_or(&config.default);

            if !effective_config.include_errors {
                tracing::debug!(
                    "Redacting errors for subgraph '{}' based on config: include_errors=false",
                    subgraph_name
                );
                // Redact fully if errors should not be included
                error.message = REDACTED_ERROR_MESSAGE.to_string();
                error.extensions = Object::new(); // Clear all extensions
            } else {
                tracing::debug!(
                    "Processing errors for subgraph '{}' based on config: {:?}",
                    subgraph_name,
                    effective_config
                );
                // Process errors based on the effective config
                // 1. Redact message if needed
                if effective_config.redact_message {
                    error.message = REDACTED_ERROR_MESSAGE.to_string();
                }

                // 2. Add 'service' extension (unless denied)
                let service_key = "service".to_string();
                let is_service_denied = effective_config
                    .deny_extensions_keys
                    .as_ref()
                    .is_some_and(|deny| deny.contains(&service_key));
                let is_service_allowed = effective_config
                    .allow_extensions_keys
                    .as_ref()
                    .is_none_or(|allow| allow.contains(&service_key)); // Allowed if no allow list or if present in allow list

                if !is_service_denied && is_service_allowed {
                    error
                        .extensions
                        .entry(service_key)
                        .or_insert(subgraph_name.clone().into());
                }

                // 3. Filter extensions based on allow list
                if let Some(allow_keys) = &effective_config.allow_extensions_keys {
                    let mut original_extensions = std::mem::take(&mut error.extensions);
                    for key in allow_keys {
                        if let Some((key, value)) = original_extensions.remove_entry(key.as_str()) {
                            error.extensions.insert(key, value);
                        }
                    }
                }

                // 4. Remove extensions based on deny list (applied *after* allow list)
                if let Some(deny_keys) = &effective_config.deny_extensions_keys {
                    for key in deny_keys {
                        error.extensions.remove(key.as_str());
                    }
                }
            }
        }
    }
}
