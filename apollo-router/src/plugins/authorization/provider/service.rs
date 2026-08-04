use std::sync::Arc;
use std::task::Context;
use std::task::Poll;

use futures::future::BoxFuture;
use http::StatusCode;
use tower::BoxError;
use tower::Layer;
use tower::Service;

use super::FailureMode;
use super::PolicyProviderRegistry;
use super::ProviderPolicyDecisions;
use crate::graphql;
use crate::services::supergraph;

/// Evaluates the policies required by a supergraph request before calling the inner service.
#[derive(Clone)]
pub(crate) struct PolicyProviderLayer {
    registry: Arc<PolicyProviderRegistry>,
    dry_run: bool,
}

impl PolicyProviderLayer {
    pub(crate) fn with_dry_run(registry: Arc<PolicyProviderRegistry>, dry_run: bool) -> Self {
        Self { registry, dry_run }
    }
}

impl<S> Layer<S> for PolicyProviderLayer {
    type Service = PolicyProviderService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        PolicyProviderService {
            inner,
            registry: Arc::clone(&self.registry),
            dry_run: self.dry_run,
        }
    }
}

/// Tower service produced by [`PolicyProviderLayer`].
#[derive(Clone)]
pub(crate) struct PolicyProviderService<S> {
    inner: S,
    registry: Arc<PolicyProviderRegistry>,
    dry_run: bool,
}

impl<S> Service<supergraph::Request> for PolicyProviderService<S>
where
    S: Service<supergraph::Request, Response = supergraph::Response, Error = BoxError>
        + Clone
        + Send
        + 'static,
    S::Future: Send + 'static,
{
    type Response = supergraph::Response;
    type Error = BoxError;
    type Future = BoxFuture<'static, Result<Self::Response, Self::Error>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, request: supergraph::Request) -> Self::Future {
        let registry = Arc::clone(&self.registry);
        let inner = self.inner.clone();
        let dry_run = self.dry_run;
        let mut inner = std::mem::replace(&mut self.inner, inner);

        Box::pin(async move {
            let required = ProviderPolicyDecisions::required(&request.context);
            if required.is_empty() {
                return inner.call(request).await;
            }

            let evaluation = registry.evaluate(&request, required.clone());
            match evaluation.await {
                Ok(decisions) => {
                    decisions.store(&request.context)?;
                    inner.call(request).await
                }
                Err(error) if dry_run => {
                    let provider_name = PolicyProviderRegistry::provider_name_for_error(&error);
                    u64_counter!(
                        "apollo.router.operations.policy_provider.failure",
                        "OPA policy provider evaluation failures",
                        1,
                        policy.provider.name = provider_name.to_string(),
                        policy.provider.outcome = "dry_run_failure"
                    );
                    tracing::warn!(error = %error, "policy provider evaluation failed during authorization dry run");
                    ProviderPolicyDecisions::deny_all(required).store(&request.context)?;
                    inner.call(request).await
                }
                Err(error) if registry.failure_mode() == FailureMode::Deny => {
                    let provider_name = PolicyProviderRegistry::provider_name_for_error(&error);
                    u64_counter!(
                        "apollo.router.operations.policy_provider.failure",
                        "OPA policy provider evaluation failures",
                        1,
                        policy.provider.name = provider_name.to_string(),
                        policy.provider.outcome = "deny_failure"
                    );
                    tracing::error!(error = %error, "policy provider evaluation failed; denying policies");
                    ProviderPolicyDecisions::deny_all(required).store(&request.context)?;
                    inner.call(request).await
                }
                Err(error) => {
                    let provider_name = PolicyProviderRegistry::provider_name_for_error(&error);
                    u64_counter!(
                        "apollo.router.operations.policy_provider.failure",
                        "OPA policy provider evaluation failures",
                        1,
                        policy.provider.name = provider_name.to_string(),
                        policy.provider.outcome = "reject_failure"
                    );
                    tracing::error!(error = %error, "policy provider evaluation failed");
                    Ok(supergraph::Response::error_builder()
                        .error(
                            graphql::Error::builder()
                                .message("policy evaluation failed".to_string())
                                .extension_code("POLICY_EVALUATION_FAILED")
                                .build(),
                        )
                        .status_code(StatusCode::SERVICE_UNAVAILABLE)
                        .context(request.context)
                        .build()?)
                }
            }
        })
    }
}
