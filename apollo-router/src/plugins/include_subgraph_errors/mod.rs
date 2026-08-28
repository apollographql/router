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
use futures::FutureExt as _;
use futures::TryFutureExt as _;
use futures::future::BoxFuture;
use tower::BoxError;
use tower::ServiceExt;

use crate::error::Error;
use crate::graphql;
use crate::json_ext::Object;
use crate::plugin::Plugin;
use crate::plugin::PluginInit;
use crate::services::fetch::AddSubgraphNameExt;
use crate::services::fetch::SubgraphNameExt;
use crate::services::subgraph;
use crate::services::supergraph;

static REDACTED_ERROR_MESSAGE: &str = "Subgraph errors redacted";

register_plugin!("apollo", "include_subgraph_errors", IncludeSubgraphErrors);

/// Layer type for [`IncludeSubgraphErrors::redact_subgraph_errors_layer`].
pub(crate) struct RedactSubgraphErrorsLayer {
    config: Arc<EffectiveConfig>,
}

impl RedactSubgraphErrorsLayer {
    fn new(config: Arc<EffectiveConfig>) -> Self {
        Self { config }
    }
}

impl<S> tower::Layer<S> for RedactSubgraphErrorsLayer
where
    S: tower::Service<supergraph::Request, Response = supergraph::Response, Error = BoxError>
        + Clone
        + Send
        + 'static,
    S::Future: Send + 'static,
{
    type Service = supergraph::BoxCloneService;

    fn layer(&self, inner: S) -> Self::Service {
        let config = self.config.clone();

        inner
            .map_response(move |response: supergraph::Response| {
                response.map_stream(move |mut graphql_response: graphql::Response| {
                    for error in &mut graphql_response.errors {
                        IncludeSubgraphErrors::process_error(&config, error);
                    }
                    for incremental in &mut graphql_response.incremental {
                        for error in &mut incremental.errors {
                            IncludeSubgraphErrors::process_error(&config, error);
                        }
                    }

                    graphql_response
                })
            })
            .boxed_clone()
    }
}

/// Layer type for [`IncludeSubgraphErrors::tag_errors_with_subgraph_name_layer`], which
/// documents which extension the tag uses and why filtering happens at the supergraph stage.
pub(crate) struct TagSubgraphErrorsLayer {
    subgraph_name: Arc<str>,
}

impl TagSubgraphErrorsLayer {
    fn new(subgraph_name: Arc<str>) -> Self {
        Self { subgraph_name }
    }
}

impl<S> tower::Layer<S> for TagSubgraphErrorsLayer
where
    S: tower::Service<subgraph::Request, Response = subgraph::Response> + Clone + Send + 'static,
    S::Future: Send + 'static,
{
    type Service = TagSubgraphErrorsService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        TagSubgraphErrorsService {
            inner,
            subgraph_name: self.subgraph_name.clone(),
        }
    }
}

/// Service type for [`IncludeSubgraphErrors::tag_errors_with_subgraph_name_layer`].
#[derive(Clone)]
pub(crate) struct TagSubgraphErrorsService<S> {
    inner: S,
    /// The subgraph this service stack was built for. Taken from the pipeline rather than
    /// from `request.subgraph_name` so that it cannot disagree with the stack it is
    /// installed on -- a mis-tagged error silently falls back to the default redaction
    /// config in `process_error`.
    subgraph_name: Arc<str>,
}

impl<S> tower::Service<subgraph::Request> for TagSubgraphErrorsService<S>
where
    S: tower::Service<subgraph::Request, Response = subgraph::Response> + Clone + Send + 'static,
    S::Future: Send + 'static,
{
    type Response = S::Response;
    type Error = S::Error;
    type Future = BoxFuture<'static, Result<Self::Response, Self::Error>>;

    fn poll_ready(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, req: subgraph::Request) -> Self::Future {
        let subgraph_name = self.subgraph_name.clone();
        self.inner
            .call(req)
            .map_ok(move |mut response| {
                let body = response.response.body_mut();
                for error in &mut body.errors {
                    error.add_subgraph_name(&subgraph_name);
                }
                response
            })
            .boxed()
    }
}

pub(crate) struct IncludeSubgraphErrors {
    // Store the calculated effective configuration
    config: Arc<EffectiveConfig>,
}

impl IncludeSubgraphErrors {
    /// Returns a layer that redacts subgraph errors from a supergraph response.
    pub(crate) fn redact_subgraph_errors_layer(&self) -> RedactSubgraphErrorsLayer {
        RedactSubgraphErrorsLayer::new(self.config.clone())
    }

    /// Returns a layer that tags each subgraph error with the name of the subgraph it came
    /// from, so that [`Self::redact_subgraph_errors_layer`] can apply that subgraph's
    /// redaction config once the error reaches the supergraph response.
    ///
    /// The tag is the private `apollo.private.subgraph.name` extension (see
    /// [`AddSubgraphNameExt`]), *not* the user-facing `service` extension: `process_error`
    /// removes the private one during redaction and adds `service` separately, subject to the
    /// configured allow/deny lists.
    ///
    /// Filtering deliberately does not happen here. Other kinds of request also generate
    /// errors that need filtering, so pushing the filtering out to the supergraph response
    /// ensures everything gets filtered.
    pub(crate) fn tag_errors_with_subgraph_name_layer(
        &self,
        subgraph_name: Arc<str>,
    ) -> TagSubgraphErrorsLayer {
        TagSubgraphErrorsLayer::new(subgraph_name)
    }
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
