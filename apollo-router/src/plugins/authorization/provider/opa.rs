use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::fmt;
use std::future::Ready;
use std::num::NonZeroUsize;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;
use std::time::Duration;

use http::HeaderName;
use http::HeaderValue;
use http::Method;
use serde::Deserialize;
use serde::Serialize;
use serde_json::Value;
use tower::BoxError;
use tower::Layer;
use tower::Service;
use tower::ServiceExt;
use tower::retry::Policy;
use tower::retry::RetryLayer;
use tower::timeout::TimeoutLayer;
use tracing::Instrument;

use super::config::InputConfig;
use super::config::LoadBalancingStrategy;
use super::config::OpaConfig;
use crate::Context;
use crate::context::OPERATION_KIND;
use crate::plugins::authentication::APOLLO_AUTHENTICATION_JWT_CLAIMS;
use crate::services::http::HttpClientService;
use crate::services::http::HttpRequest;
use crate::services::router;
use crate::services::supergraph;

pub(super) const CONTRACT_VERSION: &str = "apollo.router.policy/v1";

#[derive(Clone)]
pub(super) struct OpaProvider {
    name: String,
    client: HttpClientService,
    endpoints: Arc<EndpointPool>,
    decision: String,
    timeout: Duration,
    max_attempts: NonZeroUsize,
    headers: BTreeMap<String, String>,
    input: InputConfig,
}

struct EndpointPool {
    endpoints: Vec<reqwest::Url>,
    health: Vec<EndpointHealth>,
    selector: EndpointSelector,
}

struct EndpointHealth {
    failures: AtomicUsize,
    ejected_until: Mutex<Option<tokio::time::Instant>>,
}

enum EndpointSelector {
    RoundRobin(AtomicUsize),
}

#[derive(Serialize)]
struct OpaRequest<'a> {
    input: PolicyInput<'a>,
}

#[derive(Serialize)]
struct PolicyInput<'a> {
    contract: &'static str,
    policies: &'a BTreeSet<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    identity: Option<IdentityInput>,
    operation: OperationInput,
    request: RequestInput,
    context: BTreeMap<String, Value>,
}

#[derive(Serialize)]
struct IdentityInput {
    claims: Value,
}

#[derive(Serialize)]
struct OperationInput {
    name: Option<String>,
    kind: Option<Value>,
    variables: BTreeMap<String, Value>,
}

#[derive(Serialize)]
struct RequestInput {
    headers: BTreeMap<String, Vec<String>>,
}

#[derive(Deserialize)]
struct OpaResponse {
    #[serde(default)]
    decision_id: Option<String>,
    result: Option<OpaResult>,
}

#[derive(Deserialize)]
struct OpaResult {
    contract: String,
    decisions: BTreeMap<String, bool>,
}

enum AttemptError {
    Retryable {
        endpoint_index: usize,
        source: BoxError,
    },
    Fatal(BoxError),
}

struct EvaluationError(BoxError);

impl fmt::Display for EvaluationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

impl fmt::Debug for EvaluationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, formatter)
    }
}

impl std::error::Error for EvaluationError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(self.0.as_ref())
    }
}

impl fmt::Display for AttemptError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Retryable { source, .. } | Self::Fatal(source) => source.fmt(formatter),
        }
    }
}

impl fmt::Debug for AttemptError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, formatter)
    }
}

impl std::error::Error for AttemptError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Retryable { source, .. } | Self::Fatal(source) => Some(source.as_ref()),
        }
    }
}

#[derive(Clone)]
struct AttemptRequest {
    body: Arc<[u8]>,
    start: usize,
    attempt: usize,
}

#[derive(Clone)]
struct AttemptService {
    name: Arc<str>,
    client: HttpClientService,
    endpoints: Arc<EndpointPool>,
    decision: Arc<str>,
    headers: Arc<BTreeMap<String, String>>,
}

#[derive(Clone)]
struct RetryPolicy {
    name: Arc<str>,
    endpoints: Arc<EndpointPool>,
    retries_remaining: usize,
}

struct InputSource {
    context: Context,
    body: crate::graphql::Request,
    headers: http::HeaderMap,
}

impl OpaProvider {
    pub(super) fn new(name: String, config: OpaConfig) -> Result<Self, BoxError> {
        if config.endpoints.is_empty() {
            return Err(format!("OPA provider `{name}` must have at least one endpoint").into());
        }
        if config.transport.timeouts.total.is_zero() {
            return Err(format!("OPA provider `{name}` timeout must be greater than zero").into());
        }
        let decision = config.api.decision.trim_matches('/').to_string();
        if decision.is_empty() {
            return Err(format!("OPA provider `{name}` decision must not be empty").into());
        }
        let endpoints = config
            .endpoints
            .into_iter()
            .map(|endpoint| {
                crate::services::validate_external_service_url(
                    &endpoint.url,
                    &format!("OPA provider `{name}` endpoint"),
                    false,
                    crate::services::UnixSocketQueryPolicy::OptionalAbsolutePath,
                )
            })
            .collect::<Result<Vec<_>, _>>()?;
        let mut unique_endpoints = BTreeSet::new();
        for endpoint in &endpoints {
            if !unique_endpoints.insert(endpoint.as_str()) {
                return Err(format!(
                    "OPA provider `{name}` contains duplicate endpoint `{endpoint}`"
                )
                .into());
            }
        }
        for (header_name, header_value) in &config.transport.headers {
            HeaderName::try_from(header_name)?;
            HeaderValue::try_from(header_value)?;
        }

        let transport = config.transport;
        Ok(Self {
            name: name.clone(),
            client: HttpClientService::from_config_for_policy_provider(
                name.clone(),
                &crate::Configuration::default(),
                &HttpClientService::native_roots_store(),
                transport.client.clone(),
            )?,
            endpoints: Arc::new(EndpointPool::new(
                endpoints,
                transport.load_balancing.strategy,
            )),
            decision,
            timeout: transport.timeouts.total,
            max_attempts: transport.retry.max_attempts,
            headers: transport.headers,
            input: config.input,
        })
    }

    pub(super) async fn evaluate(
        &self,
        request: &supergraph::Request,
        policies: BTreeSet<String>,
    ) -> Result<BTreeMap<String, bool>, BoxError> {
        let provider_name = self.name.clone();
        let outcome = std::sync::Arc::new(Mutex::new("failure"));
        let outcome_for_timer = outcome.clone();
        let _timer = crate::plugins::telemetry::utils::Timer::new(move |duration| {
            let outcome = outcome_for_timer
                .lock()
                .map(|value| *value)
                .unwrap_or("failure");
            f64_histogram!(
                "apollo.router.operations.policy_provider.duration",
                "Duration of native policy provider evaluation",
                duration.as_secs_f64(),
                policy.provider.type = "opa",
                policy.provider.name = provider_name,
                policy.provider.outcome = outcome
            );
        });
        let span = tracing::info_span!(
            "policy_provider.evaluate",
            "otel.kind" = "INTERNAL",
            "policy.provider.type" = "opa",
            "policy.provider.name" = self.name.as_str(),
            "policy.count" = policies.len()
        );
        let input_source = InputSource {
            context: request.context.clone(),
            body: request.supergraph_request.body().clone(),
            headers: request.supergraph_request.headers().clone(),
        };
        let provider = self.clone();
        let outcome_for_evaluation = Arc::clone(&outcome);
        let evaluation = tower::service_fn(move |(input_source, policies)| {
            let outcome = Arc::clone(&outcome_for_evaluation);
            let provider = provider.clone();
            async move {
                let result: Result<BTreeMap<String, bool>, EvaluationError> = async {
                    let body = OpaRequest {
                        input: provider
                            .build_input(&input_source, &policies)
                            .map_err(EvaluationError)?,
                    };
                    let body: Arc<[u8]> = serde_json::to_vec(&body)
                        .map_err(|error| EvaluationError(Box::new(error)))?
                        .into();
                    let start = provider.endpoints.next_start();
                    let attempt_service = AttemptService {
                        name: Arc::from(provider.name.as_str()),
                        client: provider.client.clone(),
                        endpoints: Arc::clone(&provider.endpoints),
                        decision: Arc::from(provider.decision.as_str()),
                        headers: Arc::new(provider.headers.clone()),
                    };
                    let retry_policy = RetryPolicy {
                        name: Arc::from(provider.name.as_str()),
                        endpoints: Arc::clone(&provider.endpoints),
                        retries_remaining: provider.max_attempts.get() - 1,
                    };
                    let response = RetryLayer::new(retry_policy)
                        .layer(attempt_service)
                        .oneshot(AttemptRequest {
                            body,
                            start,
                            attempt: 0,
                        })
                        .await
                        .map_err(|error| EvaluationError(Box::new(error)))?;
                    let decisions = provider
                        .decisions(response, policies)
                        .map_err(EvaluationError);
                    if let Ok(mut value) = outcome.lock() {
                        *value = if decisions.is_ok() {
                            "success"
                        } else {
                            "contract_error"
                        };
                    }
                    decisions
                }
                .await;
                result
            }
        });

        match TimeoutLayer::new(self.timeout)
            .layer(evaluation)
            .oneshot((input_source, policies))
            .instrument(span)
            .await
        {
            Ok(decisions) => Ok(decisions),
            Err(error) if error.is::<tower::timeout::error::Elapsed>() => {
                if let Ok(mut value) = outcome.lock() {
                    *value = "timeout";
                }
                Err(format!("OPA provider `{}` timed out", self.name).into())
            }
            Err(error) => Err(error),
        }
    }

    fn decisions(
        &self,
        response: OpaResponse,
        policies: BTreeSet<String>,
    ) -> Result<BTreeMap<String, bool>, BoxError> {
        if let Some(decision_id) = response.decision_id {
            tracing::debug!(
                opa.decision_id = decision_id,
                policy.provider.name = self.name,
                "OPA policy decision"
            );
        }
        let Some(result) = response.result else {
            u64_counter!(
                "apollo.router.operations.policy_provider.missing_result",
                "OPA responses missing the result field",
                1,
                policy.provider.name = self.name.clone()
            );
            tracing::warn!(
                policy.provider.name = self.name,
                "OPA response omitted result; denying policies"
            );
            return Ok(policies.into_iter().map(|policy| (policy, false)).collect());
        };
        if result.contract != CONTRACT_VERSION {
            return Err(format!(
                "OPA provider `{}` returned unsupported contract `{}`",
                self.name, result.contract
            )
            .into());
        }
        if result
            .decisions
            .keys()
            .any(|policy| !policies.contains(policy))
        {
            return Err(format!(
                "OPA provider `{}` returned a decision for an unrequested policy",
                self.name
            )
            .into());
        }
        Ok(policies
            .into_iter()
            .map(|policy| {
                let allowed = result.decisions.get(&policy).copied().unwrap_or(false);
                if allowed {
                    u64_counter!(
                        "apollo.router.operations.policy_provider.allow",
                        "OPA policy decisions",
                        1,
                        policy.provider.name = self.name.clone()
                    );
                } else {
                    u64_counter!(
                        "apollo.router.operations.policy_provider.deny",
                        "OPA policy decisions",
                        1,
                        policy.provider.name = self.name.clone()
                    );
                }
                (policy, allowed)
            })
            .collect())
    }

    fn build_input<'a>(
        &self,
        request: &'a InputSource,
        policies: &'a BTreeSet<String>,
    ) -> Result<PolicyInput<'a>, BoxError> {
        let identity = (!self.input.claims.include.is_empty()).then(|| IdentityInput {
            claims: context_value(&request.context, APOLLO_AUTHENTICATION_JWT_CLAIMS)
                .map(|claims| select_claims(claims, &self.input.claims.include))
                .unwrap_or(Value::Null),
        });
        let variables = self
            .input
            .variables
            .include
            .iter()
            .filter_map(|name| {
                request
                    .body
                    .variables
                    .get(name.as_str())
                    .and_then(|value| serde_json::to_value(value).ok())
                    .map(|value| (name.clone(), value))
            })
            .collect();
        let headers = self
            .input
            .headers
            .include
            .iter()
            .map(|name| {
                let header_name = HeaderName::try_from(name.as_str()).map_err(|error| {
                    format!(
                        "OPA provider `{}` cannot forward invalid header name `{name}`: {error}",
                        self.name
                    )
                })?;
                let values = request.headers.get_all(header_name);
                let values = values
                    .iter()
                    .map(|value| value.to_str().map(str::to_owned))
                    .collect::<Result<Vec<_>, _>>()
                    .map_err(|_| {
                        format!(
                            "OPA provider `{}` cannot forward non-UTF-8 header `{name}`",
                            self.name
                        )
                    })?;
                Ok((name.to_ascii_lowercase(), values))
            })
            .collect::<Result<BTreeMap<_, _>, BoxError>>()?;
        let context = self
            .input
            .context
            .include
            .iter()
            .filter_map(|name| {
                context_value(&request.context, name).map(|value| (name.clone(), value))
            })
            .collect();

        Ok(PolicyInput {
            contract: CONTRACT_VERSION,
            policies,
            identity,
            operation: OperationInput {
                name: request.body.operation_name.clone(),
                kind: context_value(&request.context, OPERATION_KIND),
                variables,
            },
            request: RequestInput { headers },
            context,
        })
    }
}

impl Service<AttemptRequest> for AttemptService {
    type Response = OpaResponse;
    type Error = AttemptError;
    type Future = futures::future::BoxFuture<'static, Result<Self::Response, Self::Error>>;

    fn poll_ready(
        &mut self,
        _context: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        std::task::Poll::Ready(Ok(()))
    }

    fn call(&mut self, request: AttemptRequest) -> Self::Future {
        let name = Arc::clone(&self.name);
        let client = self.client.clone();
        let endpoints = Arc::clone(&self.endpoints);
        let decision = Arc::clone(&self.decision);
        let headers = Arc::clone(&self.headers);

        Box::pin(async move {
            let (endpoint_index, endpoint) = endpoints.choose(request.start + request.attempt);
            let endpoint = endpoint.clone();
            let retryable = |source: BoxError| AttemptError::Retryable {
                endpoint_index,
                source,
            };
            let mut url = endpoint.clone();
            let base_path = endpoint.path().trim_end_matches('/');
            if endpoint.scheme() != "unix" {
                url.set_path(&format!("{base_path}/v1/data/{decision}"));
            }
            let uri = if url.scheme() == "unix" {
                #[cfg(unix)]
                {
                    let socket = url.path();
                    let base = endpoint
                        .query_pairs()
                        .find_map(|(key, value)| (key == "path").then_some(value))
                        .unwrap_or_else(|| "/".into());
                    let path = format!("{}/v1/data/{decision}", base.trim_end_matches('/'));
                    let converted: http::Uri = hyperlocal::Uri::new(socket, &path).into();
                    converted
                }
                #[cfg(not(unix))]
                {
                    return Err(AttemptError::Fatal(
                        "unix socket URLs are unsupported on this platform".into(),
                    ));
                }
            } else {
                url.as_str()
                    .parse::<http::Uri>()
                    .map_err(|error| AttemptError::Fatal(Box::new(error)))?
            };
            let mut http_request = http::Request::builder()
                .method(Method::POST)
                .uri(uri)
                .header(http::header::CONTENT_TYPE, "application/json")
                .header(http::header::ACCEPT, "application/json")
                .body(router::body::from_bytes(request.body.to_vec()))
                .map_err(|error| AttemptError::Fatal(Box::new(error)))?;
            for (header_name, header_value) in headers.iter() {
                http_request.headers_mut().insert(
                    HeaderName::try_from(header_name)
                        .map_err(|error| AttemptError::Fatal(Box::new(error)))?,
                    HeaderValue::try_from(header_value)
                        .map_err(|error| AttemptError::Fatal(Box::new(error)))?,
                );
            }

            let response = client
                .oneshot(HttpRequest {
                    http_request,
                    context: crate::Context::new(),
                })
                .await
                .map_err(&retryable)?;
            let status = response.http_response.status();
            if status.is_server_error()
                || status == http::StatusCode::REQUEST_TIMEOUT
                || status == http::StatusCode::TOO_MANY_REQUESTS
            {
                return Err(retryable(
                    format!("OPA provider `{name}` returned {status}").into(),
                ));
            }
            if !status.is_success() {
                return Err(AttemptError::Fatal(
                    format!("OPA provider `{name}` returned {status}").into(),
                ));
            }
            let bytes = router::body::into_bytes(response.http_response.into_body())
                .await
                .map_err(|error| {
                    retryable(
                        format!("OPA provider `{name}` returned an invalid response: {error}")
                            .into(),
                    )
                })?;
            let response = serde_json::from_slice(&bytes).map_err(|error| {
                retryable(
                    format!("OPA provider `{name}` returned an invalid response: {error}").into(),
                )
            })?;
            endpoints.success(endpoint_index);
            Ok(response)
        })
    }
}

impl Policy<AttemptRequest, OpaResponse, AttemptError> for RetryPolicy {
    type Future = Ready<()>;

    fn retry(
        &mut self,
        request: &mut AttemptRequest,
        result: &mut Result<OpaResponse, AttemptError>,
    ) -> Option<Self::Future> {
        let Err(AttemptError::Retryable { endpoint_index, .. }) = result else {
            return None;
        };
        self.endpoints.failure(*endpoint_index, &self.name);
        if self.retries_remaining == 0 {
            u64_counter!(
                "apollo.router.operations.policy_provider.retry_exhausted",
                "OPA provider retry exhaustion",
                1,
                policy.provider.name = self.name.to_string()
            );
            return None;
        }

        self.retries_remaining -= 1;
        request.attempt += 1;
        u64_counter!(
            "apollo.router.operations.policy_provider.retry",
            "OPA provider retries",
            1,
            policy.provider.name = self.name.to_string(),
            policy.provider.endpoint.index = *endpoint_index as i64
        );
        Some(std::future::ready(()))
    }

    fn clone_request(&mut self, request: &AttemptRequest) -> Option<AttemptRequest> {
        Some(request.clone())
    }
}

impl EndpointPool {
    fn new(endpoints: Vec<reqwest::Url>, strategy: LoadBalancingStrategy) -> Self {
        let selector = match strategy {
            LoadBalancingStrategy::RoundRobin => EndpointSelector::RoundRobin(AtomicUsize::new(0)),
        };
        let health = (0..endpoints.len())
            .map(|_| EndpointHealth {
                failures: AtomicUsize::new(0),
                ejected_until: Mutex::new(None),
            })
            .collect();
        Self {
            endpoints,
            health,
            selector,
        }
    }

    fn next_start(&self) -> usize {
        match &self.selector {
            EndpointSelector::RoundRobin(next) => next.fetch_add(1, Ordering::Relaxed),
        }
    }

    fn choose(&self, index: usize) -> (usize, &reqwest::Url) {
        for offset in 0..self.endpoints.len() {
            let candidate = (index + offset) % self.endpoints.len();
            let ejected = self.health[candidate]
                .ejected_until
                .lock()
                .ok()
                .and_then(|until| *until)
                .is_some_and(|until| until > tokio::time::Instant::now());
            if !ejected {
                return (candidate, &self.endpoints[candidate]);
            }
        }
        (
            index % self.endpoints.len(),
            &self.endpoints[index % self.endpoints.len()],
        )
    }

    fn success(&self, index: usize) {
        self.health[index].failures.store(0, Ordering::Relaxed);
    }

    fn failure(&self, index: usize, provider_name: &str) {
        let failures = self.health[index].failures.fetch_add(1, Ordering::Relaxed) + 1;
        if failures >= 2 {
            if let Ok(mut until) = self.health[index].ejected_until.lock() {
                *until = Some(tokio::time::Instant::now() + Duration::from_secs(1));
            }
            u64_counter!(
                "apollo.router.operations.policy_provider.endpoint_ejected",
                "OPA provider endpoint ejections",
                1,
                policy.provider.name = provider_name.to_string(),
                policy.provider.endpoint.index = index as i64
            );
        }
    }
}

fn context_value(context: &Context, key: &str) -> Option<Value> {
    context
        .get_json_value(key)
        .and_then(|value| serde_json::to_value(value).ok())
}

fn select_claims(claims: Value, names: &[String]) -> Value {
    let Some(object) = claims.as_object() else {
        return Value::Null;
    };
    let selected = names
        .iter()
        .filter_map(|name| object.get(name).map(|v| (name.clone(), v.clone())))
        .collect::<serde_json::Map<_, _>>();
    Value::Object(selected)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn ejects_failed_endpoint_and_allows_it_after_cooldown() {
        let endpoints = vec![
            "http://127.0.0.1:8181".parse().unwrap(),
            "http://127.0.0.1:8182".parse().unwrap(),
        ];
        let pool = EndpointPool::new(endpoints, LoadBalancingStrategy::RoundRobin);

        pool.failure(0, "primary");
        pool.failure(0, "primary");
        assert_eq!(pool.choose(0).0, 1);

        tokio::time::sleep(Duration::from_millis(1_050)).await;
        assert_eq!(pool.choose(0).0, 0);
    }
}
