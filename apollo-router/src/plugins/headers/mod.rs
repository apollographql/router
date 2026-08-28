use std::collections::HashMap;
use std::collections::HashSet;
use std::sync::Arc;
use std::task::Context;
use std::task::Poll;

use apollo_federation::connectors::runtime::http_json_transport::TransportRequest;
use futures::future::BoxFuture;
use http::HeaderMap;
use http::HeaderValue;
use http::header::ACCEPT;
use http::header::ACCEPT_ENCODING;
use http::header::CONNECTION;
use http::header::CONTENT_ENCODING;
use http::header::CONTENT_LENGTH;
use http::header::CONTENT_TYPE;
use http::header::HOST;
use http::header::HeaderName;
use http::header::PROXY_AUTHENTICATE;
use http::header::PROXY_AUTHORIZATION;
use http::header::TE;
use http::header::TRAILER;
use http::header::TRANSFER_ENCODING;
use http::header::UPGRADE;
use itertools::Itertools;
use regex::Regex;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json_bytes::Value;
use serde_json_bytes::path::JsonPathInst;
use tower::BoxError;
use tower::Layer;
use tower::ServiceBuilder;
use tower::ServiceExt;
use tower_service::Service;

use crate::plugin::PluginInit;
use crate::plugin::PluginPrivate;
use crate::plugin::serde::deserialize_header_name;
use crate::plugin::serde::deserialize_header_value;
use crate::plugin::serde::deserialize_jsonpath;
use crate::plugin::serde::deserialize_option_header_name;
use crate::plugin::serde::deserialize_option_header_value;
use crate::plugin::serde::deserialize_regex;
use crate::services::SubgraphRequest;
use crate::services::connector;
use crate::services::router;

register_private_plugin!("apollo", "headers", Headers);

/// Request-side header configuration: propagation operations + optional masking.
#[derive(Clone, JsonSchema, Deserialize, Default)]
#[serde(rename_all = "snake_case", deny_unknown_fields, default)]
struct HeadersLocation {
    /// Propagate/Insert/Remove operations
    #[serde(default)]
    operations: Vec<Operation>,

    /// Header masking configuration applied to request headers in logs/telemetry.
    #[serde(default)]
    masking: Option<crate::configuration::header_masking_config::HeaderMaskingConfig>,
}

/// Response-side header configuration. Response propagation isn't a router
/// feature, so only masking is configurable here.
#[derive(Clone, JsonSchema, Deserialize, Default)]
#[serde(rename_all = "snake_case", deny_unknown_fields, default)]
struct ResponseHeadersLocation {
    /// Header masking configuration applied to response headers in logs/telemetry.
    #[serde(default)]
    masking: Option<crate::configuration::header_masking_config::HeaderMaskingConfig>,
}

/// Configuration for connector headers at a specific location
/// Connectors only have request operations - masking is inherited from parent subgraph
#[derive(Clone, JsonSchema, Deserialize, Default)]
#[serde(rename_all = "snake_case", deny_unknown_fields, default)]
struct ConnectorHeadersLocation {
    /// Request-side propagate/insert/remove operations
    #[serde(default)]
    request: Option<ConnectorRequestHeadersLocation>,
}

/// Request-side connector header configuration. Mirrors the wrapped
/// `operations:` shape used by `HeadersLocation`, so connector config doesn't
/// drift from regular subgraph config.
#[derive(Clone, JsonSchema, Deserialize, Default)]
#[serde(rename_all = "snake_case", deny_unknown_fields, default)]
struct ConnectorRequestHeadersLocation {
    /// Propagate/Insert/Remove operations
    #[serde(default)]
    operations: Vec<Operation>,
}

#[derive(Clone, JsonSchema, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
enum Operation {
    Insert(Insert),
    Remove(Remove),
    Propagate(Propagate),
}

schemar_fn!(remove_named, String, "Remove a header given a header name");
schemar_fn!(
    remove_matching,
    String,
    "Remove a header given a regex matching against the header name"
);

#[derive(Clone, JsonSchema, Deserialize)]
#[serde(rename_all = "snake_case")]
/// Remove header
enum Remove {
    #[schemars(schema_with = "remove_named")]
    #[serde(deserialize_with = "deserialize_header_name")]
    /// Remove a header given a header name
    Named(HeaderName),

    #[schemars(schema_with = "remove_matching")]
    #[serde(deserialize_with = "deserialize_regex")]
    /// Remove a header given a regex matching header name
    Matching(Regex),
}

#[derive(Clone, JsonSchema, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
#[serde(untagged)]
/// Insert header
enum Insert {
    /// Insert static header
    Static(InsertStatic),
    /// Insert header with a value coming from context key (works only for a string in the context)
    FromContext(InsertFromContext),
    /// Insert header with a value coming from body
    FromBody(InsertFromBody),
}

#[derive(Clone, JsonSchema, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
/// Insert static header
struct InsertStatic {
    /// The name of the header
    #[schemars(with = "String")]
    #[serde(deserialize_with = "deserialize_header_name")]
    name: HeaderName,

    /// The value for the header
    #[schemars(with = "String")]
    #[serde(deserialize_with = "deserialize_header_value")]
    value: HeaderValue,
}

#[derive(Clone, JsonSchema, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
/// Insert header with a value coming from context key
struct InsertFromContext {
    #[schemars(with = "String")]
    #[serde(deserialize_with = "deserialize_header_name")]
    /// Specify header name
    name: HeaderName,
    /// Specify context key to fetch value
    from_context: String,
}

#[derive(Clone, JsonSchema, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
/// Insert header with a value coming from body
struct InsertFromBody {
    /// The target header name
    #[schemars(with = "String")]
    #[serde(deserialize_with = "deserialize_header_name")]
    name: HeaderName,

    /// The path in the request body
    #[schemars(with = "String")]
    #[serde(deserialize_with = "deserialize_jsonpath")]
    path: JsonPathInst,

    /// The default if the path in the body did not resolve to an element
    #[schemars(with = "Option<String>", default)]
    #[serde(deserialize_with = "deserialize_option_header_value", default)]
    default: Option<HeaderValue>,
}

schemar_fn!(
    propagate_matching,
    String,
    "Remove a header given a regex matching header name"
);

#[derive(Clone, JsonSchema, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
#[serde(untagged)]
/// Propagate header
enum Propagate {
    /// Propagate header given a header name
    Named {
        /// The source header name
        #[schemars(with = "String")]
        #[serde(deserialize_with = "deserialize_header_name")]
        named: HeaderName,

        /// An optional target header name
        #[schemars(with = "Option<String>", default)]
        #[serde(deserialize_with = "deserialize_option_header_name", default)]
        rename: Option<HeaderName>,

        /// Default value for the header.
        #[schemars(with = "Option<String>", default)]
        #[serde(deserialize_with = "deserialize_option_header_value", default)]
        default: Option<HeaderValue>,
    },
    /// Propagate header given a regex to match header name
    Matching {
        /// The regex on header name
        #[schemars(schema_with = "propagate_matching")]
        #[serde(deserialize_with = "deserialize_regex")]
        matching: Regex,
    },
}

/// Configuration for connectors (no masking - inherits from parent subgraph)
#[derive(Clone, JsonSchema, Default, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields, default)]
struct ConnectorHeadersConfiguration {
    /// Options applying to all sources across all subgraphs
    #[serde(default)]
    all: Option<ConnectorHeadersLocation>,

    /// Map of subgraph_name.connector_source_name to configuration
    #[serde(default)]
    sources: HashMap<String, ConnectorHeadersLocation>,
}

/// Per-subgraph (or global) header configuration. Request configuration covers
/// propagation + masking; response configuration covers masking only.
#[derive(Clone, JsonSchema, Default, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields, default)]
struct GlobalHeadersConfiguration {
    /// Request configuration (operations and masking)
    #[serde(default)]
    request: Option<HeadersLocation>,

    /// Response configuration (masking only)
    #[serde(default)]
    response: Option<ResponseHeadersLocation>,
}

/// Configuration for header propagation and masking
#[derive(Clone, JsonSchema, Default, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields, default)]
#[schemars(rename = "HeadersConfig")]
pub(crate) struct Config {
    /// Rules to apply to all subgraphs (global defaults)
    #[serde(default)]
    all: Option<GlobalHeadersConfiguration>,

    /// Rules for specific subgraphs
    #[serde(default)]
    subgraphs: HashMap<String, GlobalHeadersConfiguration>,

    /// Rules for connectors
    #[serde(default)]
    connector: ConnectorHeadersConfiguration,
}

pub(crate) struct Headers {
    all_operations: Arc<Vec<Operation>>,
    subgraph_operations: HashMap<String, Arc<Vec<Operation>>>,
    all_connector_operations: Arc<Vec<Operation>>,
    connector_source_operations: HashMap<String, Arc<Vec<Operation>>>,

    masking_rules_map: Arc<crate::services::header_masking::MaskingRulesMap>,
}

/// Resolve the effective masking config for one subgraph by layering its
/// `masking` block on top of the global config:
///
/// - `enabled: false` fully opts the subgraph out (masks nothing).
/// - `replace_defaults: true` makes the subgraph's `sensitive_headers` list
///   authoritative for that subgraph (no inherited headers).
/// - otherwise (the default) the subgraph's list *extends* the inherited list:
///   the global effective list when global masking is enabled, or the built-in
///   sensitive-header defaults when it's disabled — so enabling masking for a
///   single subgraph stays fail-secure rather than silently masking only the
///   subgraph's own list.
fn merge_subgraph_masking(
    global: &crate::configuration::header_masking_config::HeaderMaskingConfig,
    sg: &crate::configuration::header_masking_config::HeaderMaskingConfig,
) -> crate::configuration::header_masking_config::HeaderMaskingConfig {
    use crate::configuration::header_masking_config::HeaderMaskingConfig;
    use crate::configuration::header_masking_config::default_sensitive_headers;

    // A subgraph fully opts out with `enabled: false`.
    if !sg.enabled {
        return sg.clone();
    }

    let sensitive_headers = if sg.replace_defaults {
        // Authoritative: mask exactly the subgraph's list for this subgraph.
        sg.sensitive_headers.clone()
    } else {
        // Extend the inherited list. Fall back to the built-in defaults when
        // global masking is disabled so a subgraph that opts *in* still gets
        // the fail-secure list.
        let mut headers = if global.enabled {
            global.effective_sensitive_headers()
        } else {
            default_sensitive_headers()
        };
        headers.extend(sg.sensitive_headers.iter().cloned());
        headers
    };

    HeaderMaskingConfig {
        enabled: true,
        sensitive_headers,
        // Already a fully-resolved list; don't have `from_config` merge
        // defaults a second time.
        replace_defaults: true,
    }
}

#[async_trait::async_trait]
impl PluginPrivate for Headers {
    type Config = Config;

    async fn new(init: PluginInit<Self::Config>) -> Result<Self, BoxError> {
        use crate::services::header_masking::DirectionRules;
        use crate::services::header_masking::HeaderMaskingRules;

        // Extract global request operations from all.request.operations
        let operations: Vec<Operation> = init
            .config
            .all
            .as_ref()
            .and_then(|a| a.request.as_ref())
            .map(|r| r.operations.clone())
            .unwrap_or_default();

        // Build subgraph operations (global + subgraph-specific)
        let subgraph_operations = init
            .config
            .subgraphs
            .iter()
            .map(|(subgraph_name, sg_config)| {
                let mut operations = operations.clone();
                if let Some(request) = &sg_config.request {
                    operations.append(&mut request.operations.clone());
                }
                (subgraph_name.clone(), Arc::new(operations))
            })
            .collect();

        // Extract connector operations
        let all_connector_operations: Vec<Operation> = init
            .config
            .connector
            .all
            .as_ref()
            .and_then(|a| a.request.as_ref())
            .map(|r| r.operations.clone())
            .unwrap_or_default();

        let connector_source_operations = init
            .config
            .connector
            .sources
            .iter()
            .map(|(source_name, connector_config)| {
                let mut ops = operations.clone();
                if let Some(request) = &connector_config.request {
                    ops.append(&mut request.operations.clone());
                }
                (source_name.clone(), Arc::new(ops))
            })
            .collect();

        // Fail-secure default: when the user hasn't written a `masking:` block,
        // fall back to the full HeaderMaskingConfig::default() (the 12-header
        // sensitive list) — *not* HeaderMaskingRules::default(), which would
        // give an empty HashSet and silently mask nothing.
        let effective_global_request_config = init
            .config
            .all
            .as_ref()
            .and_then(|a| a.request.as_ref())
            .and_then(|r| r.masking.clone())
            .unwrap_or_default();
        let effective_global_response_config = init
            .config
            .all
            .as_ref()
            .and_then(|a| a.response.as_ref())
            .and_then(|r| r.masking.clone())
            .unwrap_or_default();

        let global_request_masking = Arc::new(HeaderMaskingRules::from_config(
            &effective_global_request_config,
        ));
        let global_response_masking = Arc::new(HeaderMaskingRules::from_config(
            &effective_global_response_config,
        ));

        let per_subgraph_request_masking: HashMap<String, Arc<HeaderMaskingRules>> = init
            .config
            .subgraphs
            .iter()
            .filter_map(|(name, sg_config)| {
                let sg_masking = sg_config
                    .request
                    .as_ref()
                    .and_then(|r| r.masking.as_ref())?;
                let merged = merge_subgraph_masking(&effective_global_request_config, sg_masking);
                Some((
                    name.clone(),
                    Arc::new(HeaderMaskingRules::from_config(&merged)),
                ))
            })
            .collect();

        let per_subgraph_response_masking: HashMap<String, Arc<HeaderMaskingRules>> = init
            .config
            .subgraphs
            .iter()
            .filter_map(|(name, sg_config)| {
                let sg_masking = sg_config
                    .response
                    .as_ref()
                    .and_then(|r| r.masking.as_ref())?;
                let merged = merge_subgraph_masking(&effective_global_response_config, sg_masking);
                Some((
                    name.clone(),
                    Arc::new(HeaderMaskingRules::from_config(&merged)),
                ))
            })
            .collect();

        let masking_rules_map = Arc::new(crate::services::header_masking::MaskingRulesMap::new(
            DirectionRules::new(global_request_masking, per_subgraph_request_masking),
            DirectionRules::new(global_response_masking, per_subgraph_response_masking),
        ));

        Ok(Headers {
            all_operations: Arc::new(operations),
            all_connector_operations: Arc::new(all_connector_operations),
            subgraph_operations,
            connector_source_operations,
            masking_rules_map,
        })
    }
}

impl Headers {
    /// Returns a layer that applies this subgraph's header operations.
    ///
    /// Note: masking rules aren't installed here — they're inserted into request context
    /// once by [`Headers::masking_rules_context_layer`], and consumers resolve per-subgraph rules
    /// at read time via `MaskingRulesMap::get_request(Some(name))` / `get_response(...)`.
    pub(crate) fn subgraph_headers_layer(&self, name: &str) -> HeadersLayer {
        let operations = self
            .subgraph_operations
            .get(name)
            .cloned()
            .unwrap_or_else(|| self.all_operations.clone());

        HeadersLayer::new(operations)
    }

    /// Returns a layer that applies this connector source's header operations (falling back
    /// to the global connector operations if the source has none of its own).
    pub(crate) fn connector_headers_layer(&self, source_name: &str) -> HeadersLayer {
        let operations = self
            .connector_source_operations
            .get(source_name)
            .cloned()
            .unwrap_or_else(|| self.all_connector_operations.clone());

        HeadersLayer::new(operations)
    }

    /// Returns a layer that inserts a [`MaskingRulesMap`] into the request context.
    pub(crate) fn masking_rules_context_layer(&self) -> MaskingContextLayer {
        MaskingContextLayer::new(self.masking_rules_map.clone())
    }
}

/// Layer type for [`Headers::masking_rules_context_layer`].
pub(crate) struct MaskingContextLayer {
    masking_rules_map: Arc<crate::services::header_masking::MaskingRulesMap>,
}

impl MaskingContextLayer {
    fn new(masking_rules_map: Arc<crate::services::header_masking::MaskingRulesMap>) -> Self {
        Self { masking_rules_map }
    }
}

impl<S> Layer<S> for MaskingContextLayer
where
    S: Service<router::Request, Response = router::Response, Error = BoxError>
        + Clone
        + Send
        + 'static,
    S::Future: Send + 'static,
{
    type Service = router::BoxCloneService;

    fn layer(&self, inner: S) -> Self::Service {
        let masking_rules_map = self.masking_rules_map.clone();

        ServiceBuilder::new()
            .map_request(move |req: router::Request| {
                req.context.extensions().with_lock(|lock| {
                    lock.insert(masking_rules_map.clone());
                });
                req
            })
            .service(inner)
            .boxed_clone()
    }
}

pub(crate) struct HeadersLayer {
    operations: Arc<Vec<Operation>>,
}

impl HeadersLayer {
    fn new(operations: Arc<Vec<Operation>>) -> Self {
        Self { operations }
    }
}

impl<S> Layer<S> for HeadersLayer {
    type Service = HeadersService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        HeadersService {
            inner,
            operations: self.operations.clone(),
        }
    }
}

#[derive(Clone)]
pub(crate) struct HeadersService<S> {
    inner: S,
    operations: Arc<Vec<Operation>>,
}

// Headers from https://datatracker.ietf.org/doc/html/rfc2616#section-13.5.1
// These are not propagated by default using a regex match as they will not make sense for the
// second hop.
// In addition because our requests are not regular proxy requests content-type, content-length
// and host are also in the exclude list.
static RESERVED_HEADERS: [HeaderName; 14] = [
    CONNECTION,
    PROXY_AUTHENTICATE,
    PROXY_AUTHORIZATION,
    TE,
    TRAILER,
    TRANSFER_ENCODING,
    UPGRADE,
    CONTENT_LENGTH,
    CONTENT_TYPE,
    CONTENT_ENCODING,
    HOST,
    ACCEPT,
    ACCEPT_ENCODING,
    HeaderName::from_static("keep-alive"),
];

impl<S> Service<SubgraphRequest> for HeadersService<S>
where
    S: Service<SubgraphRequest> + Clone + Send + 'static,
    S::Future: Send + 'static,
{
    type Response = S::Response;
    type Error = S::Error;
    type Future = BoxFuture<'static, Result<Self::Response, Self::Error>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, mut req: SubgraphRequest) -> Self::Future {
        let inner = self.inner.clone();
        let mut inner = std::mem::replace(&mut self.inner, inner);
        let operations = self.operations.clone();

        Box::pin(async move {
            Self::modify_subgraph_request(&operations, &mut req);
            inner.call(req).await
        })
    }
}

impl<S> Service<connector::request_service::Request> for HeadersService<S>
where
    S: Service<connector::request_service::Request> + Clone + Send + 'static,
    S::Future: Send + 'static,
{
    type Response = S::Response;
    type Error = S::Error;
    type Future = BoxFuture<'static, Result<Self::Response, Self::Error>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, mut req: connector::request_service::Request) -> Self::Future {
        let inner = self.inner.clone();
        let mut inner = std::mem::replace(&mut self.inner, inner);
        let operations = self.operations.clone();

        Box::pin(async move {
            Self::modify_connector_request(&operations, &mut req);
            inner.call(req).await
        })
    }
}

impl<S> HeadersService<S> {
    fn modify_subgraph_request(operations: &Arc<Vec<Operation>>, req: &mut SubgraphRequest) {
        let mut already_propagated: HashSet<String> = HashSet::new();

        let body_to_value = serde_json_bytes::value::to_value(req.supergraph_request.body()).ok();
        let supergraph_headers = req.supergraph_request.headers();
        let context = &req.context;
        let headers_mut = req.subgraph_request.headers_mut();

        for operation in &**operations {
            operation.process_header_rules(
                &mut already_propagated,
                supergraph_headers,
                &body_to_value,
                context,
                headers_mut,
                None,
            );
        }
    }

    fn modify_connector_request(
        operations: &Arc<Vec<Operation>>,
        req: &mut connector::request_service::Request,
    ) {
        let mut already_propagated: HashSet<String> = HashSet::new();

        let TransportRequest::Http(ref mut http_request) = req.transport_request else {
            return;
        };
        let body_to_value = serde_json::from_str(http_request.inner.body()).ok();
        let supergraph_headers = req.supergraph_request.headers();
        let context = &req.context;
        // We need to know what headers were added prior to this processing to that we can properly override as needed
        let existing_headers = http_request.inner.headers().clone();
        let headers_mut = http_request.inner.headers_mut();

        for operation in &**operations {
            operation.process_header_rules(
                &mut already_propagated,
                supergraph_headers,
                &body_to_value,
                context,
                headers_mut,
                Some(&existing_headers),
            );
        }
    }
}

impl Operation {
    fn process_header_rules(
        &self,
        already_propagated: &mut HashSet<String>,
        supergraph_headers: &HeaderMap,
        body_to_value: &Option<Value>,
        context: &crate::Context,
        headers_mut: &mut HeaderMap,
        existing_headers: Option<&HeaderMap>,
    ) {
        match self {
            Operation::Insert(insert) => {
                insert.process_header_rules(body_to_value, context, headers_mut)
            }
            Operation::Remove(remove) => remove.process_header_rules(headers_mut),
            Operation::Propagate(propagate) => propagate.process_header_rules(
                already_propagated,
                supergraph_headers,
                headers_mut,
                existing_headers,
            ),
        }
    }
}

impl Insert {
    fn process_header_rules(
        &self,
        body_to_value: &Option<Value>,
        context: &crate::Context,
        headers_mut: &mut HeaderMap,
    ) {
        match self {
            Insert::Static(insert_static) => {
                headers_mut.insert(&insert_static.name, insert_static.value.clone());
            }
            Insert::FromContext(insert_from_context) => {
                if let Some(val) = context
                    .get::<_, String>(&insert_from_context.from_context)
                    .ok()
                    .flatten()
                {
                    match HeaderValue::from_str(&val) {
                        Ok(header_value) => {
                            headers_mut.insert(&insert_from_context.name, header_value);
                        }
                        Err(err) => {
                            tracing::error!(
                                "cannot convert from the context into a header value for header name '{}': {:?}",
                                insert_from_context.name,
                                err
                            );
                        }
                    }
                }
            }
            Insert::FromBody(from_body) => {
                if let Some(body_to_value) = &body_to_value {
                    let output = from_body.path.find(body_to_value);
                    if let serde_json_bytes::Value::Null = output {
                        if let Some(default_val) = &from_body.default {
                            headers_mut.insert(&from_body.name, default_val.clone());
                        }
                    } else {
                        let header_value = if let serde_json_bytes::Value::String(val_str) = output
                        {
                            val_str.as_str().to_string()
                        } else {
                            output.to_string()
                        };
                        match HeaderValue::from_str(&header_value) {
                            Ok(header_value) => {
                                headers_mut.insert(&from_body.name, header_value);
                            }
                            Err(err) => {
                                let header_name = &from_body.name;
                                tracing::error!(%header_name, ?err, "cannot convert from the body into a header value for header name");
                            }
                        }
                    }
                } else if let Some(default_val) = &from_body.default {
                    headers_mut.insert(&from_body.name, default_val.clone());
                }
            }
        }
    }
}

impl Remove {
    fn process_header_rules(&self, headers_mut: &mut HeaderMap) {
        match self {
            Remove::Named(name) => {
                headers_mut.remove(name);
            }
            Remove::Matching(matching) => {
                let new_headers = headers_mut
                    .drain()
                    .filter_map(|(name, value)| {
                        name.and_then(|name| {
                            (RESERVED_HEADERS.contains(&name) || !matching.is_match(name.as_str()))
                                .then_some((name, value))
                        })
                    })
                    .collect();

                let _ = std::mem::replace(headers_mut, new_headers);
            }
        }
    }
}

impl Propagate {
    fn process_header_rules(
        &self,
        already_propagated: &mut HashSet<String>,
        supergraph_headers: &HeaderMap,
        headers_mut: &mut HeaderMap,
        existing_headers: Option<&HeaderMap>,
    ) {
        let default_headers = Default::default();
        let existing_headers = existing_headers.unwrap_or(&default_headers);
        match self {
            Propagate::Named {
                named,
                rename,
                default,
            } => {
                let target_header = rename.as_ref().unwrap_or(named);
                if !already_propagated.contains(target_header.as_str()) {
                    // If the header was already added previously by some other
                    // method (e.g Connectors), remove it first before propagating
                    // the value from the client request. This allows us to use
                    // `.append` instead of `.insert` to handle multiple headers.
                    //
                    // Note: Rhai and Coprocessor plugins run after this plugin,
                    // so this will not remove headers added there.
                    if existing_headers.contains_key(target_header) {
                        headers_mut.remove(target_header);
                    }

                    let values = supergraph_headers.get_all(named);
                    if values.iter().count() == 0 {
                        if let Some(default) = default {
                            headers_mut.append(target_header, default.clone());
                            already_propagated.insert(target_header.to_string());
                        }
                    } else {
                        for value in values {
                            headers_mut.append(target_header, value.clone());
                            already_propagated.insert(target_header.to_string());
                        }
                    }
                }
            }
            Propagate::Matching { matching } => {
                supergraph_headers
                    .iter()
                    .filter(|(name, _)| {
                        !RESERVED_HEADERS.contains(*name) && matching.is_match(name.as_str())
                    })
                    .chunk_by(|(name, ..)| name.to_owned())
                    .into_iter()
                    .for_each(|(name, headers)| {
                        if !already_propagated.contains(name.as_str()) {
                            // If the header was already added previously by some other
                            // method (e.g Connectors), remove it first before propagating
                            // the value from the client request. This allows us to use
                            // `.append` instead of `.insert` to handle multiple headers.
                            //
                            // Note: Rhai and Coprocessor plugins run after this plugin,
                            // so this will not remove headers added there.
                            if existing_headers.contains_key(name) {
                                headers_mut.remove(name);
                            }

                            headers.for_each(|(_, value)| {
                                headers_mut.append(name, value.clone());
                            });
                            already_propagated.insert(name.to_string());
                        }
                    });
            }
        }
    }
}

#[cfg(test)]
mod test {
    use std::collections::HashSet;
    use std::str::FromStr;
    use std::sync::Arc;

    use apollo_compiler::name;
    use apollo_federation::connectors::ConnectId;
    use apollo_federation::connectors::ConnectSpec;
    use apollo_federation::connectors::Connector;
    use apollo_federation::connectors::HttpJsonTransport;
    use apollo_federation::connectors::JSONSelection;
    use apollo_federation::connectors::runtime::http_json_transport::HttpRequest;
    use apollo_federation::connectors::runtime::key::ResponseKey;
    use serde_json_bytes::json;
    use subgraph::SubgraphRequestId;
    use tower::BoxError;

    use super::*;
    use crate::Context;
    use crate::graphql;
    use crate::graphql::Request;
    use crate::plugins::test::PluginTestHarness;
    use crate::query_planner::fetch::OperationKind;
    use crate::services::SubgraphRequest;
    use crate::services::SubgraphResponse;
    use crate::services::subgraph;

    #[test]
    fn test_subgraph_config() {
        serde_yaml::from_str::<Config>(
            r#"
        subgraphs:
          products:
            request:
              operations:
                - insert:
                    name: "test"
                    value: "test"
        "#,
        )
        .unwrap();
    }

    #[test]
    fn test_insert_config() {
        serde_yaml::from_str::<Config>(
            r#"
        all:
            request:
                operations:
                - insert:
                    name: "test"
                    value: "test"
        "#,
        )
        .unwrap();
    }

    #[test]
    fn test_remove_config() {
        serde_yaml::from_str::<Config>(
            r#"
        all:
            request:
                operations:
                    - remove:
                        named: "test"
        "#,
        )
        .unwrap();

        serde_yaml::from_str::<Config>(
            r#"
        all:
            request:
                operations:
                    - remove:
                        matching: "d.*"
        "#,
        )
        .unwrap();

        assert!(
            serde_yaml::from_str::<Config>(
                r#"
        all:
            request:
                operations:
                    - remove:
                        matching: "d.*["
        "#,
            )
            .is_err()
        );
    }

    #[test]
    fn test_propagate_config() {
        serde_yaml::from_str::<Config>(
            r#"
        all:
            request:
                operations:
                    - propagate:
                        named: "test"
        "#,
        )
        .unwrap();

        serde_yaml::from_str::<Config>(
            r#"
        all:
            request:
                operations:
                    - propagate:
                        named: "test"
                        rename: "bif"
        "#,
        )
        .unwrap();

        serde_yaml::from_str::<Config>(
            r#"
        all:
            request:
                operations:
                    - propagate:
                        named: "test"
                        rename: "bif"
                        default: "bof"
        "#,
        )
        .unwrap();

        serde_yaml::from_str::<Config>(
            r#"
        all:
            request:
                operations:
                    - propagate:
                        matching: "d.*"
        "#,
        )
        .unwrap();
    }

    #[test]
    fn test_masking_config_global() {
        serde_yaml::from_str::<Config>(
            r#"
        all:
            request:
                masking:
                    enabled: true
                    sensitive_headers:
                        - authorization
                        - x-api-key
        "#,
        )
        .unwrap();
    }

    #[test]
    fn test_masking_config_per_subgraph() {
        serde_yaml::from_str::<Config>(
            r#"
        subgraphs:
          products:
            request:
              masking:
                enabled: true
                sensitive_headers:
                  - authorization
        "#,
        )
        .unwrap();
    }

    #[test]
    fn test_masking_config_combined_with_operations() {
        serde_yaml::from_str::<Config>(
            r#"
        all:
            request:
                operations:
                    - propagate:
                        named: "x-forwarded-for"
                masking:
                    enabled: true
                    sensitive_headers:
                        - authorization
                        - cookie
        "#,
        )
        .unwrap();
    }

    #[test]
    fn test_masking_config_response_global() {
        let config = serde_yaml::from_str::<Config>(
            r#"
        all:
            response:
                masking:
                    enabled: true
                    sensitive_headers:
                        - set-cookie
                        - www-authenticate
        "#,
        )
        .unwrap();

        let masking = config
            .all
            .as_ref()
            .and_then(|a| a.response.as_ref())
            .and_then(|r| r.masking.as_ref())
            .expect("response masking should deserialize");
        assert!(masking.enabled);
        assert!(masking.sensitive_headers.iter().any(|h| h == "set-cookie"));
    }

    #[test]
    fn test_masking_config_response_per_subgraph_differs_from_request() {
        let config = serde_yaml::from_str::<Config>(
            r#"
        subgraphs:
          products:
            request:
              masking:
                enabled: true
                sensitive_headers:
                  - authorization
            response:
              masking:
                enabled: true
                sensitive_headers:
                  - set-cookie
        "#,
        )
        .unwrap();

        let products = config.subgraphs.get("products").unwrap();
        let req = products.request.as_ref().unwrap().masking.as_ref().unwrap();
        let resp = products
            .response
            .as_ref()
            .unwrap()
            .masking
            .as_ref()
            .unwrap();
        assert_eq!(req.sensitive_headers, vec!["authorization".to_string()]);
        assert_eq!(resp.sensitive_headers, vec!["set-cookie".to_string()]);
    }

    #[test]
    fn merge_subgraph_masking_extends_global_list() {
        use crate::configuration::header_masking_config::HeaderMaskingConfig;
        let global = HeaderMaskingConfig {
            enabled: true,
            sensitive_headers: vec!["authorization".into(), "cookie".into()],
            replace_defaults: false,
        };
        let sg = HeaderMaskingConfig {
            enabled: true,
            sensitive_headers: vec!["x-products-secret".into()],
            replace_defaults: false,
        };
        let merged = merge_subgraph_masking(&global, &sg);
        assert!(merged.enabled);
        assert!(merged.sensitive_headers.contains(&"authorization".into()));
        assert!(merged.sensitive_headers.contains(&"cookie".into()));
        assert!(
            merged
                .sensitive_headers
                .contains(&"x-products-secret".into())
        );
    }

    #[test]
    fn merge_subgraph_masking_disabled_subgraph_is_full_opt_out() {
        use crate::configuration::header_masking_config::HeaderMaskingConfig;
        let global = HeaderMaskingConfig {
            enabled: true,
            sensitive_headers: vec!["authorization".into()],
            replace_defaults: false,
        };
        let sg = HeaderMaskingConfig {
            enabled: false,
            sensitive_headers: vec![],
            replace_defaults: false,
        };
        let merged = merge_subgraph_masking(&global, &sg);
        assert!(!merged.enabled);
    }

    #[test]
    fn merge_subgraph_masking_disabled_global_falls_back_to_defaults() {
        use crate::configuration::header_masking_config::HeaderMaskingConfig;
        // Global masking off, but the subgraph opts in: it should still get the
        // built-in fail-secure defaults, plus its own header — not just its own
        // list.
        let global = HeaderMaskingConfig {
            enabled: false,
            sensitive_headers: vec![],
            replace_defaults: false,
        };
        let sg = HeaderMaskingConfig {
            enabled: true,
            sensitive_headers: vec!["x-products-secret".into()],
            replace_defaults: false,
        };
        let merged = merge_subgraph_masking(&global, &sg);
        assert!(merged.enabled);
        assert!(merged.sensitive_headers.contains(&"authorization".into()));
        assert!(merged.sensitive_headers.contains(&"cookie".into()));
        assert!(
            merged
                .sensitive_headers
                .contains(&"x-products-secret".into())
        );
    }

    #[test]
    fn merge_subgraph_masking_replace_defaults_is_authoritative() {
        use crate::configuration::header_masking_config::HeaderMaskingConfig;
        // `replace_defaults: true` makes the subgraph's list authoritative — no
        // inherited global or built-in headers.
        let global = HeaderMaskingConfig {
            enabled: true,
            sensitive_headers: vec!["authorization".into()],
            replace_defaults: false,
        };
        let sg = HeaderMaskingConfig {
            enabled: true,
            sensitive_headers: vec!["x-only-this".into()],
            replace_defaults: true,
        };
        let merged = merge_subgraph_masking(&global, &sg);
        assert!(merged.enabled);
        assert_eq!(merged.sensitive_headers, vec!["x-only-this".to_string()]);
    }

    #[test]
    fn test_masking_config_disabled() {
        let config = serde_yaml::from_str::<Config>(
            r#"
        all:
            request:
                masking:
                    enabled: false
        "#,
        )
        .unwrap();

        let masking = config
            .all
            .as_ref()
            .and_then(|a| a.request.as_ref())
            .and_then(|r| r.masking.as_ref());
        assert!(masking.is_some());
        assert!(!masking.unwrap().enabled);
    }

    #[tokio::test]
    async fn test_insert_static() -> Result<(), BoxError> {
        let (mock, mut handle) = tower_test::mock::pair::<SubgraphRequest, SubgraphResponse>();

        let mut service = HeadersLayer::new(Arc::new(vec![Operation::Insert(Insert::Static(
            InsertStatic {
                name: "c".try_into()?,
                value: "d".try_into()?,
            },
        ))]))
        .layer(mock);

        let driver = tokio::spawn(async move {
            let (request, responder) = handle.next_request().await.unwrap();
            request.assert_headers(vec![
                ("aa", "vaa"),
                ("ab", "vab"),
                ("ac", "vac"),
                ("c", "d"),
            ]);
            responder.send_response(example_response(request).unwrap());
        });
        service.ready().await?.call(example_request()).await?;
        crate::plugin::test::await_mock_driver(driver).await;
        Ok(())
    }

    #[tokio::test]
    async fn test_connector_insert_static() -> Result<(), BoxError> {
        let (mock, mut handle) = tower_test::mock::pair::<
            connector::request_service::Request,
            connector::request_service::Response,
        >();

        let mut service = HeadersLayer::new(Arc::new(vec![Operation::Insert(Insert::Static(
            InsertStatic {
                name: "c".try_into()?,
                value: "d".try_into()?,
            },
        ))]))
        .layer(mock);

        let call = tokio::spawn(service.ready().await?.call(example_connector_request()));
        let (request, responder) = handle.next_request().await.unwrap();
        request.assert_headers(vec![
            ("aa", "vaa"),
            ("ab", "vab"),
            ("ac", "vac"),
            ("c", "d"),
        ]);
        responder.send_response(example_connector_response(request).unwrap());
        call.await.unwrap()?;
        Ok(())
    }

    #[tokio::test]
    async fn test_insert_from_context() -> Result<(), BoxError> {
        let (mock, mut handle) = tower_test::mock::pair::<SubgraphRequest, SubgraphResponse>();

        let mut service = HeadersLayer::new(Arc::new(vec![Operation::Insert(
            Insert::FromContext(InsertFromContext {
                name: "header_from_context".try_into()?,
                from_context: "my_key".to_string(),
            }),
        )]))
        .layer(mock);

        let call = tokio::spawn(service.ready().await?.call(example_request()));
        let (request, responder) = handle.next_request().await.unwrap();
        request.assert_headers(vec![
            ("aa", "vaa"),
            ("ab", "vab"),
            ("ac", "vac"),
            ("header_from_context", "my_value_from_context"),
        ]);
        responder.send_response(example_response(request).unwrap());
        call.await.unwrap()?;
        Ok(())
    }

    #[tokio::test]
    async fn test_connector_insert_from_context() -> Result<(), BoxError> {
        let (mock, mut handle) = tower_test::mock::pair::<
            connector::request_service::Request,
            connector::request_service::Response,
        >();

        let mut service = HeadersLayer::new(Arc::new(vec![Operation::Insert(
            Insert::FromContext(InsertFromContext {
                name: "header_from_context".try_into()?,
                from_context: "my_key".to_string(),
            }),
        )]))
        .layer(mock);

        let call = tokio::spawn(service.ready().await?.call(example_connector_request()));
        let (request, responder) = handle.next_request().await.unwrap();
        request.assert_headers(vec![
            ("aa", "vaa"),
            ("ab", "vab"),
            ("ac", "vac"),
            ("header_from_context", "my_value_from_context"),
        ]);
        responder.send_response(example_connector_response(request).unwrap());
        call.await.unwrap()?;
        Ok(())
    }

    #[tokio::test]
    async fn test_insert_from_request_body() -> Result<(), BoxError> {
        let (mock, mut handle) = tower_test::mock::pair::<SubgraphRequest, SubgraphResponse>();

        let mut service = HeadersLayer::new(Arc::new(vec![Operation::Insert(Insert::FromBody(
            InsertFromBody {
                name: "header_from_request".try_into()?,
                path: JsonPathInst::from_str("$.operationName").unwrap(),
                default: None,
            },
        ))]))
        .layer(mock);

        let call = tokio::spawn(service.ready().await?.call(example_request()));
        let (request, responder) = handle.next_request().await.unwrap();
        request.assert_headers(vec![
            ("aa", "vaa"),
            ("ab", "vab"),
            ("ac", "vac"),
            ("header_from_request", "my_operation_name"),
        ]);
        responder.send_response(example_response(request).unwrap());
        call.await.unwrap()?;
        Ok(())
    }

    #[tokio::test]
    async fn test_connector_insert_from_request_body() -> Result<(), BoxError> {
        let (mock, mut handle) = tower_test::mock::pair::<
            connector::request_service::Request,
            connector::request_service::Response,
        >();

        let mut service = HeadersLayer::new(Arc::new(vec![Operation::Insert(Insert::FromBody(
            InsertFromBody {
                name: "header_from_request".try_into()?,
                path: JsonPathInst::from_str("$.myCoolField").unwrap(),
                default: None,
            },
        ))]))
        .layer(mock);

        let call = tokio::spawn(service.ready().await?.call(example_connector_request()));
        let (request, responder) = handle.next_request().await.unwrap();
        request.assert_headers(vec![
            ("aa", "vaa"),
            ("ab", "vab"),
            ("ac", "vac"),
            ("header_from_request", "myCoolValue"),
        ]);
        responder.send_response(example_connector_response(request).unwrap());
        call.await.unwrap()?;
        Ok(())
    }

    #[tokio::test]
    async fn test_insert_from_request_body_with_old_access_json_notation() -> Result<(), BoxError> {
        let (mock, mut handle) = tower_test::mock::pair::<SubgraphRequest, SubgraphResponse>();

        let mut service = HeadersLayer::new(Arc::new(vec![Operation::Insert(Insert::FromBody(
            InsertFromBody {
                name: "header_from_request".try_into()?,
                path: JsonPathInst::from_str(".operationName").unwrap(),
                default: None,
            },
        ))]))
        .layer(mock);

        let call = tokio::spawn(service.ready().await?.call(example_request()));
        let (request, responder) = handle.next_request().await.unwrap();
        request.assert_headers(vec![
            ("aa", "vaa"),
            ("ab", "vab"),
            ("ac", "vac"),
            ("header_from_request", "my_operation_name"),
        ]);
        responder.send_response(example_response(request).unwrap());
        call.await.unwrap()?;
        Ok(())
    }

    #[tokio::test]
    async fn test_connector_insert_from_request_body_with_old_access_json_notation()
    -> Result<(), BoxError> {
        let (mock, mut handle) = tower_test::mock::pair::<
            connector::request_service::Request,
            connector::request_service::Response,
        >();

        let mut service = HeadersLayer::new(Arc::new(vec![Operation::Insert(Insert::FromBody(
            InsertFromBody {
                name: "header_from_request".try_into()?,
                path: JsonPathInst::from_str(".myCoolField").unwrap(),
                default: None,
            },
        ))]))
        .layer(mock);

        let call = tokio::spawn(service.ready().await?.call(example_connector_request()));
        let (request, responder) = handle.next_request().await.unwrap();
        request.assert_headers(vec![
            ("aa", "vaa"),
            ("ab", "vab"),
            ("ac", "vac"),
            ("header_from_request", "myCoolValue"),
        ]);
        responder.send_response(example_connector_response(request).unwrap());
        call.await.unwrap()?;
        Ok(())
    }

    #[tokio::test]
    async fn test_remove_exact() -> Result<(), BoxError> {
        let (mock, mut handle) = tower_test::mock::pair::<SubgraphRequest, SubgraphResponse>();

        let mut service = HeadersLayer::new(Arc::new(vec![Operation::Remove(Remove::Named(
            "aa".try_into()?,
        ))]))
        .layer(mock);

        let call = tokio::spawn(service.ready().await?.call(example_request()));
        let (request, responder) = handle.next_request().await.unwrap();
        request.assert_headers(vec![("ac", "vac"), ("ab", "vab")]);
        responder.send_response(example_response(request).unwrap());
        call.await.unwrap()?;
        Ok(())
    }

    #[tokio::test]
    async fn test_remove_exact_multiple() -> Result<(), BoxError> {
        let (mock, mut handle) = tower_test::mock::pair::<SubgraphRequest, SubgraphResponse>();

        let mut service = HeadersLayer::new(Arc::new(vec![Operation::Remove(Remove::Named(
            "aa".try_into()?,
        ))]))
        .layer(mock);

        let ctx = Context::new();
        ctx.insert("my_key", "my_value_from_context".to_string())
            .unwrap();
        let req = SubgraphRequest {
            supergraph_request: Arc::new(
                http::Request::builder()
                    .header("da", "vda")
                    .header("db", "vdb")
                    .header("db", "vdb")
                    .header("db", "vdb2")
                    .header(HOST, "host")
                    .header(CONTENT_LENGTH, "2")
                    .header(CONTENT_TYPE, "graphql")
                    .body(
                        Request::builder()
                            .query("query")
                            .operation_name("my_operation_name")
                            .build(),
                    )
                    .expect("expecting valid request"),
            ),
            subgraph_request: http::Request::builder()
                .header("aa", "vaa") // will be removed
                .header("aa", "vaa") // will be removed
                .header("aa", "vaa2") // will be removed
                .header("ab", "vab")
                .header("ac", "vac")
                .header(HOST, "rhost")
                .header(CONTENT_LENGTH, "22")
                .header(CONTENT_TYPE, "graphql")
                .body(Request::builder().query("query").build())
                .expect("expecting valid request"),
            operation_kind: OperationKind::Query,
            context: ctx,
            subgraph_name: String::from("test"),
            subscription_stream: None,
            connection_closed_signal: None,
            query_hash: Default::default(),
            authorization: Default::default(),
            executable_document: None,
            id: SubgraphRequestId(String::new()),
            is_deferred_fetch: false,
        };

        let call = tokio::spawn(service.ready().await?.call(req));
        let (request, responder) = handle.next_request().await.unwrap();
        request.assert_headers(vec![("ac", "vac"), ("ab", "vab")]);
        responder.send_response(example_response(request).unwrap());
        call.await.unwrap()?;
        Ok(())
    }

    #[tokio::test]
    async fn test_connector_remove_exact() -> Result<(), BoxError> {
        let (mock, mut handle) = tower_test::mock::pair::<
            connector::request_service::Request,
            connector::request_service::Response,
        >();

        let mut service = HeadersLayer::new(Arc::new(vec![Operation::Remove(Remove::Named(
            "aa".try_into()?,
        ))]))
        .layer(mock);

        let call = tokio::spawn(service.ready().await?.call(example_connector_request()));
        let (request, responder) = handle.next_request().await.unwrap();
        request.assert_headers(vec![("ac", "vac"), ("ab", "vab")]);
        responder.send_response(example_connector_response(request).unwrap());
        call.await.unwrap()?;
        Ok(())
    }

    #[tokio::test]
    async fn test_remove_matching() -> Result<(), BoxError> {
        let (mock, mut handle) = tower_test::mock::pair::<SubgraphRequest, SubgraphResponse>();

        let mut service = HeadersLayer::new(Arc::new(vec![Operation::Remove(Remove::Matching(
            Regex::from_str("a[ab]")?,
        ))]))
        .layer(mock);

        let call = tokio::spawn(service.ready().await?.call(example_request()));
        let (request, responder) = handle.next_request().await.unwrap();
        request.assert_headers(vec![("ac", "vac")]);
        responder.send_response(example_response(request).unwrap());
        call.await.unwrap()?;
        Ok(())
    }

    #[tokio::test]
    async fn test_connector_remove_matching() -> Result<(), BoxError> {
        let (mock, mut handle) = tower_test::mock::pair::<
            connector::request_service::Request,
            connector::request_service::Response,
        >();

        let mut service = HeadersLayer::new(Arc::new(vec![Operation::Remove(Remove::Matching(
            Regex::from_str("a[ab]")?,
        ))]))
        .layer(mock);

        let call = tokio::spawn(service.ready().await?.call(example_connector_request()));
        let (request, responder) = handle.next_request().await.unwrap();
        request.assert_headers(vec![("ac", "vac")]);
        responder.send_response(example_connector_response(request).unwrap());
        call.await.unwrap()?;
        Ok(())
    }

    #[tokio::test]
    async fn test_propagate_matching() -> Result<(), BoxError> {
        let (mock, mut handle) = tower_test::mock::pair::<SubgraphRequest, SubgraphResponse>();

        let mut service =
            HeadersLayer::new(Arc::new(vec![Operation::Propagate(Propagate::Matching {
                matching: Regex::from_str("d[ab]")?,
            })]))
            .layer(mock);

        let call = tokio::spawn(service.ready().await?.call(example_request()));
        let (request, responder) = handle.next_request().await.unwrap();
        request.assert_headers(vec![
            ("aa", "vaa"),
            ("ab", "vab"),
            ("ac", "vac"),
            ("da", "vda"),
            ("db", "vdb"),
            ("db", "vdb2"),
        ]);
        responder.send_response(example_response(request).unwrap());
        call.await.unwrap()?;
        Ok(())
    }

    #[tokio::test]
    async fn test_connector_propagate_matching() -> Result<(), BoxError> {
        let (mock, mut handle) = tower_test::mock::pair::<
            connector::request_service::Request,
            connector::request_service::Response,
        >();

        let mut service =
            HeadersLayer::new(Arc::new(vec![Operation::Propagate(Propagate::Matching {
                matching: Regex::from_str("d[ab]")?,
            })]))
            .layer(mock);

        let call = tokio::spawn(service.ready().await?.call(example_connector_request()));
        let (request, responder) = handle.next_request().await.unwrap();
        request.assert_headers(vec![
            ("aa", "vaa"),
            ("ab", "vab"),
            ("ac", "vac"),
            ("da", "vda"),
            ("db", "vdb"),
            ("db", "vdb2"),
        ]);
        responder.send_response(example_connector_response(request).unwrap());
        call.await.unwrap()?;
        Ok(())
    }

    #[tokio::test]
    async fn test_propagate_exact() -> Result<(), BoxError> {
        let (mock, mut handle) = tower_test::mock::pair::<SubgraphRequest, SubgraphResponse>();

        let mut service =
            HeadersLayer::new(Arc::new(vec![Operation::Propagate(Propagate::Named {
                named: "da".try_into()?,
                rename: None,
                default: None,
            })]))
            .layer(mock);

        let call = tokio::spawn(service.ready().await?.call(example_request()));
        let (request, responder) = handle.next_request().await.unwrap();
        request.assert_headers(vec![
            ("aa", "vaa"),
            ("ab", "vab"),
            ("ac", "vac"),
            ("da", "vda"),
        ]);
        responder.send_response(example_response(request).unwrap());
        call.await.unwrap()?;
        Ok(())
    }

    #[tokio::test]
    async fn test_connector_propagate_exact() -> Result<(), BoxError> {
        let (mock, mut handle) = tower_test::mock::pair::<
            connector::request_service::Request,
            connector::request_service::Response,
        >();

        let mut service =
            HeadersLayer::new(Arc::new(vec![Operation::Propagate(Propagate::Named {
                named: "da".try_into()?,
                rename: None,
                default: None,
            })]))
            .layer(mock);

        let call = tokio::spawn(service.ready().await?.call(example_connector_request()));
        let (request, responder) = handle.next_request().await.unwrap();
        request.assert_headers(vec![
            ("aa", "vaa"),
            ("ab", "vab"),
            ("ac", "vac"),
            ("da", "vda"),
        ]);
        responder.send_response(example_connector_response(request).unwrap());
        call.await.unwrap()?;
        Ok(())
    }

    #[tokio::test]
    async fn test_propagate_exact_rename() -> Result<(), BoxError> {
        let (mock, mut handle) = tower_test::mock::pair::<SubgraphRequest, SubgraphResponse>();

        let mut service =
            HeadersLayer::new(Arc::new(vec![Operation::Propagate(Propagate::Named {
                named: "da".try_into()?,
                rename: Some("ea".try_into()?),
                default: None,
            })]))
            .layer(mock);

        let call = tokio::spawn(service.ready().await?.call(example_request()));
        let (request, responder) = handle.next_request().await.unwrap();
        request.assert_headers(vec![
            ("aa", "vaa"),
            ("ab", "vab"),
            ("ac", "vac"),
            ("ea", "vda"),
        ]);
        responder.send_response(example_response(request).unwrap());
        call.await.unwrap()?;
        Ok(())
    }

    #[tokio::test]
    async fn test_connect_propagate_exact_rename() -> Result<(), BoxError> {
        let (mock, mut handle) = tower_test::mock::pair::<
            connector::request_service::Request,
            connector::request_service::Response,
        >();

        let mut service =
            HeadersLayer::new(Arc::new(vec![Operation::Propagate(Propagate::Named {
                named: "da".try_into()?,
                rename: Some("ea".try_into()?),
                default: None,
            })]))
            .layer(mock);

        let call = tokio::spawn(service.ready().await?.call(example_connector_request()));
        let (request, responder) = handle.next_request().await.unwrap();
        request.assert_headers(vec![
            ("aa", "vaa"),
            ("ab", "vab"),
            ("ac", "vac"),
            ("ea", "vda"),
        ]);
        responder.send_response(example_connector_response(request).unwrap());
        call.await.unwrap()?;
        Ok(())
    }

    #[tokio::test]
    async fn test_propagate_multiple() -> Result<(), BoxError> {
        let (mock, mut handle) = tower_test::mock::pair::<SubgraphRequest, SubgraphResponse>();

        let mut service = HeadersLayer::new(Arc::new(vec![
            Operation::Propagate(Propagate::Named {
                named: "da".try_into()?,
                rename: Some("ra".try_into()?),
                default: None,
            }),
            Operation::Propagate(Propagate::Named {
                named: "da".try_into()?,
                rename: Some("rb".try_into()?),
                default: None,
            }),
            // This should not take effect as the header is already propagated
            Operation::Propagate(Propagate::Named {
                named: "db".try_into()?,
                rename: Some("ra".try_into()?),
                default: None,
            }),
        ]))
        .layer(mock);

        let call = tokio::spawn(service.ready().await?.call(example_request()));
        let (request, responder) = handle.next_request().await.unwrap();
        request.assert_headers(vec![
            ("aa", "vaa"),
            ("ab", "vab"),
            ("ac", "vac"),
            ("ra", "vda"),
            ("rb", "vda"),
        ]);
        responder.send_response(example_response(request).unwrap());
        call.await.unwrap()?;
        Ok(())
    }

    #[tokio::test]
    async fn test_connector_propagate_multiple() -> Result<(), BoxError> {
        let (mock, mut handle) = tower_test::mock::pair::<
            connector::request_service::Request,
            connector::request_service::Response,
        >();

        let mut service = HeadersLayer::new(Arc::new(vec![
            Operation::Propagate(Propagate::Named {
                named: "da".try_into()?,
                rename: Some("ra".try_into()?),
                default: None,
            }),
            Operation::Propagate(Propagate::Named {
                named: "da".try_into()?,
                rename: Some("rb".try_into()?),
                default: None,
            }),
            // This should not take effect as the header is already propagated
            Operation::Propagate(Propagate::Named {
                named: "db".try_into()?,
                rename: Some("ra".try_into()?),
                default: None,
            }),
        ]))
        .layer(mock);

        let call = tokio::spawn(service.ready().await?.call(example_connector_request()));
        let (request, responder) = handle.next_request().await.unwrap();
        request.assert_headers(vec![
            ("aa", "vaa"),
            ("ab", "vab"),
            ("ac", "vac"),
            ("ra", "vda"),
            ("rb", "vda"),
        ]);
        responder.send_response(example_connector_response(request).unwrap());
        call.await.unwrap()?;
        Ok(())
    }

    #[tokio::test]
    async fn test_propagate_exact_default() -> Result<(), BoxError> {
        let (mock, mut handle) = tower_test::mock::pair::<SubgraphRequest, SubgraphResponse>();
        let mut service =
            HeadersLayer::new(Arc::new(vec![Operation::Propagate(Propagate::Named {
                named: "ea".try_into()?,
                rename: None,
                default: Some("defaulted".try_into()?),
            })]))
            .layer(mock);
        let call = tokio::spawn(service.ready().await?.call(example_request()));
        let (request, responder) = handle.next_request().await.unwrap();
        request.assert_headers(vec![
            ("aa", "vaa"),
            ("ab", "vab"),
            ("ac", "vac"),
            ("ea", "defaulted"),
        ]);
        responder.send_response(example_response(request).unwrap());
        call.await.unwrap()?;
        Ok(())
    }

    #[tokio::test]
    async fn test_connector_propagate_exact_default() -> Result<(), BoxError> {
        let (mock, mut handle) = tower_test::mock::pair::<
            connector::request_service::Request,
            connector::request_service::Response,
        >();

        let mut service =
            HeadersLayer::new(Arc::new(vec![Operation::Propagate(Propagate::Named {
                named: "ea".try_into()?,
                rename: None,
                default: Some("defaulted".try_into()?),
            })]))
            .layer(mock);

        let call = tokio::spawn(service.ready().await?.call(example_connector_request()));
        let (request, responder) = handle.next_request().await.unwrap();
        request.assert_headers(vec![
            ("aa", "vaa"),
            ("ab", "vab"),
            ("ac", "vac"),
            ("ea", "defaulted"),
        ]);
        responder.send_response(example_connector_response(request).unwrap());
        call.await.unwrap()?;
        Ok(())
    }

    #[tokio::test]
    async fn test_propagate_reserved() -> Result<(), BoxError> {
        let service = HeadersService {
            inner: tower_test::mock::pair::<SubgraphRequest, SubgraphResponse>().0,
            operations: Arc::new(vec![Operation::Propagate(Propagate::Matching {
                matching: Regex::from_str(".*")?,
            })]),
        };

        let mut request = SubgraphRequest {
            supergraph_request: Arc::new(
                http::Request::builder()
                    .header("da", "vda")
                    .header("db", "vdb")
                    .header("db", "vdb")
                    .header("db", "vdb2")
                    .header(HOST, "host")
                    .header(CONTENT_LENGTH, "2")
                    .header(CONTENT_TYPE, "graphql")
                    .header(CONTENT_ENCODING, "identity")
                    .header(ACCEPT, "application/json")
                    .header(ACCEPT_ENCODING, "gzip")
                    .body(
                        Request::builder()
                            .query("query")
                            .operation_name("my_operation_name")
                            .build(),
                    )
                    .expect("expecting valid request"),
            ),
            subgraph_request: http::Request::builder()
                .header("aa", "vaa")
                .header("ab", "vab")
                .header("ac", "vac")
                .header(HOST, "rhost")
                .header(CONTENT_LENGTH, "22")
                .header(CONTENT_TYPE, "graphql")
                .body(Request::builder().query("query").build())
                .expect("expecting valid request"),
            operation_kind: OperationKind::Query,
            context: Context::new(),
            subgraph_name: String::from("test"),
            subscription_stream: None,
            connection_closed_signal: None,
            query_hash: Default::default(),
            authorization: Default::default(),
            executable_document: None,
            id: SubgraphRequestId(String::new()),
            is_deferred_fetch: false,
        };
        HeadersService::<tower_test::mock::Mock<SubgraphRequest, SubgraphResponse>>::modify_subgraph_request(&service.operations, &mut request);
        let headers = request
            .subgraph_request
            .headers()
            .iter()
            .map(|(name, value)| (name.as_str(), value.to_str().unwrap()))
            .collect::<Vec<_>>();
        assert_eq!(
            headers,
            vec![
                ("aa", "vaa"),
                ("ab", "vab"),
                ("ac", "vac"),
                ("host", "rhost"),
                ("content-length", "22"),
                ("content-type", "graphql"),
                ("da", "vda"),
                ("db", "vdb"),
                ("db", "vdb"),
                ("db", "vdb2"),
            ]
        );

        Ok(())
    }

    #[tokio::test]
    async fn test_propagate_multiple_matching_rules() -> Result<(), BoxError> {
        let service = HeadersService {
            inner: tower_test::mock::pair::<SubgraphRequest, SubgraphResponse>().0,
            operations: Arc::new(vec![
                Operation::Propagate(Propagate::Named {
                    named: HeaderName::from_static("dc"),
                    rename: None,
                    default: None,
                }),
                Operation::Propagate(Propagate::Matching {
                    matching: Regex::from_str("dc")?,
                }),
            ]),
        };

        let mut request = SubgraphRequest {
            supergraph_request: Arc::new(
                http::Request::builder()
                    .header("da", "vda")
                    .header("db", "vdb")
                    .header("dc", "vdb2")
                    .body(
                        Request::builder()
                            .query("query")
                            .operation_name("my_operation_name")
                            .build(),
                    )
                    .expect("expecting valid request"),
            ),
            subgraph_request: http::Request::builder()
                .header("aa", "vaa")
                .header("ab", "vab")
                .header("ac", "vac")
                .body(Request::builder().query("query").build())
                .expect("expecting valid request"),
            operation_kind: OperationKind::Query,
            context: Context::new(),
            subgraph_name: String::from("test"),
            subscription_stream: None,
            connection_closed_signal: None,
            query_hash: Default::default(),
            authorization: Default::default(),
            executable_document: None,
            id: SubgraphRequestId(String::new()),
            is_deferred_fetch: false,
        };
        HeadersService::<tower_test::mock::Mock<SubgraphRequest, SubgraphResponse>>::modify_subgraph_request(&service.operations, &mut request);
        let headers = request
            .subgraph_request
            .headers()
            .iter()
            .map(|(name, value)| (name.as_str(), value.to_str().unwrap()))
            .collect::<Vec<_>>();
        assert_eq!(
            headers,
            vec![("aa", "vaa"), ("ab", "vab"), ("ac", "vac"), ("dc", "vdb2"),]
        );

        Ok(())
    }

    fn example_response(req: SubgraphRequest) -> Result<SubgraphResponse, BoxError> {
        Ok(SubgraphResponse::new_from_response(
            http::Response::default(),
            Context::new(),
            req.subgraph_name,
            SubgraphRequestId(String::new()),
        ))
    }

    fn example_connector_response(
        req: connector::request_service::Request,
    ) -> Result<connector::request_service::Response, BoxError> {
        let key = ResponseKey::RootField {
            name: "hello".to_string(),
            inputs: Default::default(),
            selection: Arc::new(JSONSelection::parse("$.data").unwrap()),
        };
        Ok(connector::request_service::Response::test_new(
            req.context.clone(),
            key,
            Vec::new(),
            json!(""),
            None,
        ))
    }

    fn example_request() -> SubgraphRequest {
        let ctx = Context::new();
        ctx.insert("my_key", "my_value_from_context".to_string())
            .unwrap();
        SubgraphRequest {
            supergraph_request: Arc::new(
                http::Request::builder()
                    .header("da", "vda")
                    .header("db", "vdb")
                    .header("db", "vdb")
                    .header("db", "vdb2")
                    .header(HOST, "host")
                    .header(CONTENT_LENGTH, "2")
                    .header(CONTENT_TYPE, "graphql")
                    .body(
                        Request::builder()
                            .query("query")
                            .operation_name("my_operation_name")
                            .build(),
                    )
                    .expect("expecting valid request"),
            ),
            subgraph_request: http::Request::builder()
                .header("aa", "vaa")
                .header("ab", "vab")
                .header("ac", "vac")
                .header(HOST, "rhost")
                .header(CONTENT_LENGTH, "22")
                .header(CONTENT_TYPE, "graphql")
                .body(Request::builder().query("query").build())
                .expect("expecting valid request"),
            operation_kind: OperationKind::Query,
            context: ctx,
            subgraph_name: String::from("test"),
            subscription_stream: None,
            connection_closed_signal: None,
            query_hash: Default::default(),
            authorization: Default::default(),
            executable_document: None,
            id: SubgraphRequestId(String::new()),
            is_deferred_fetch: false,
        }
    }

    fn example_connector_request() -> connector::request_service::Request {
        let ctx = Context::new();
        ctx.insert("my_key", "my_value_from_context".to_string())
            .unwrap();
        let connector = Connector {
            spec: ConnectSpec::V0_1,
            schema_subtypes_map: Default::default(),
            id: ConnectId::new(
                "subgraph_name".into(),
                None,
                name!(Query),
                name!(a),
                None,
                0,
            ),
            transport: Some(HttpJsonTransport {
                source_template: "http://localhost/api".parse().ok(),
                connect_template: "/path".parse().unwrap(),
                ..Default::default()
            }),
            selection: JSONSelection::parse("f").unwrap(),
            entity_resolver: None,
            config: Default::default(),
            max_requests: None,
            batch_settings: None,
            request_headers: Default::default(),
            response_headers: Default::default(),
            request_variable_keys: Default::default(),
            response_variable_keys: Default::default(),
            error_settings: Default::default(),
            output_type: None,
            label: "test label".into(),
        };
        let key = ResponseKey::RootField {
            name: "hello".to_string(),
            inputs: Default::default(),
            selection: Arc::new(JSONSelection::parse("$.data").unwrap()),
        };

        let request = http::Request::builder()
            .header("aa", "vaa")
            .header("ab", "vab")
            .header("ac", "vac")
            .header(HOST, "rhost")
            .header(CONTENT_LENGTH, "22")
            .header(CONTENT_TYPE, "graphql")
            .body(
                json!({
                    "myCoolField": "myCoolValue"
                })
                .to_string(),
            )
            .unwrap();

        let http_request = HttpRequest {
            inner: request,
            debug: Default::default(),
        };

        connector::request_service::Request {
            context: ctx,
            connector: Arc::new(connector),
            transport_request: http_request.into(),
            key,
            mapping_problems: Default::default(),
            supergraph_request: Arc::new(
                http::Request::builder()
                    .header("da", "vda")
                    .header("db", "vdb")
                    .header("db", "vdb")
                    .header("db", "vdb2")
                    .header(HOST, "host")
                    .header(CONTENT_LENGTH, "2")
                    .header(CONTENT_TYPE, "graphql")
                    .body(
                        Request::builder()
                            .query("query")
                            .operation_name("my_operation_name")
                            .build(),
                    )
                    .expect("expecting valid request"),
            ),
            operation: Default::default(),
        }
    }

    impl SubgraphRequest {
        fn assert_headers(&self, headers: Vec<(&'static str, &'static str)>) -> bool {
            let mut headers = headers.clone();
            headers.push((HOST.as_str(), "rhost"));
            headers.push((CONTENT_LENGTH.as_str(), "22"));
            headers.push((CONTENT_TYPE.as_str(), "graphql"));
            let actual_headers = self
                .subgraph_request
                .headers()
                .iter()
                .map(|(name, value)| (name.as_str(), value.to_str().unwrap()))
                .collect::<HashSet<_>>();
            assert_eq!(actual_headers, headers.into_iter().collect::<HashSet<_>>());

            true
        }
    }

    impl connector::request_service::Request {
        fn assert_headers(&self, headers: Vec<(&'static str, &'static str)>) -> bool {
            let mut headers = headers.clone();
            headers.push((HOST.as_str(), "rhost"));
            headers.push((CONTENT_LENGTH.as_str(), "22"));
            headers.push((CONTENT_TYPE.as_str(), "graphql"));
            let TransportRequest::Http(ref http_request) = self.transport_request else {
                panic!("expected Http transport request");
            };
            let actual_headers = http_request
                .inner
                .headers()
                .iter()
                .map(|(name, value)| (name.as_str(), value.to_str().unwrap()))
                .collect::<HashSet<_>>();
            assert_eq!(actual_headers, headers.into_iter().collect::<HashSet<_>>());

            true
        }
    }

    async fn assert_headers(
        config: &'static str,
        input: Vec<(&'static str, &'static str)>,
        output: Vec<(&'static str, &'static str)>,
    ) {
        let harness = PluginTestHarness::<Headers>::builder()
            .config(config)
            .build()
            .await
            .expect("test harness");

        let (mock, mut handle) = tower_test::mock::pair::<subgraph::Request, subgraph::Response>();

        let mut service = ServiceBuilder::new()
            .layer(harness.subgraph_headers_layer("test"))
            .service(mock);

        let driver = tokio::spawn(async move {
            let (request, responder) = handle.next_request().await.unwrap();
            let headers = request.subgraph_request.headers();
            for (name, value) in output.iter() {
                if let Some(header) = headers.get(*name) {
                    assert_eq!(header.to_str().unwrap(), *value);
                } else {
                    panic!("missing header {name}");
                }
            }
            responder.send_response(subgraph::Response::fake_builder().build());
        });

        let mut req = http::Request::builder();
        for (name, value) in input.iter() {
            req = req.header(*name, *value);
        }

        service
            .ready()
            .await
            .unwrap()
            .call(
                subgraph::Request::fake_builder()
                    .supergraph_request(Arc::new(
                        req.body(graphql::Request::default())
                            .expect("valid request"),
                    ))
                    .build(),
            )
            .await
            .unwrap();

        crate::plugin::test::await_mock_driver(driver).await;
    }

    #[tokio::test]
    async fn test_propagate_passthrough() {
        assert_headers(
            include_str!("fixtures/propagate_passthrough.router.yaml"),
            vec![("a", "av"), ("c", "cv")],
            vec![("a", "av"), ("b", "av"), ("c", "cv")],
        )
        .await;

        assert_headers(
            include_str!("fixtures/propagate_passthrough.router.yaml"),
            vec![("b", "bv"), ("c", "cv")],
            vec![("b", "bv"), ("c", "cv")],
        )
        .await;
    }

    #[tokio::test]
    async fn test_propagate_passthrough_defaulted() {
        assert_headers(
            include_str!("fixtures/propagate_passthrough_defaulted.router.yaml"),
            vec![("a", "av")],
            vec![("b", "av")],
        )
        .await;

        assert_headers(
            include_str!("fixtures/propagate_passthrough_defaulted.router.yaml"),
            vec![("b", "bv")],
            vec![("b", "bv")],
        )
        .await;
        assert_headers(
            include_str!("fixtures/propagate_passthrough_defaulted.router.yaml"),
            vec![("c", "cv")],
            vec![("b", "defaulted")],
        )
        .await;
    }

    /// A dropped `.apply_required_plugin_layer(..)` call in `stages.rs` would leave every
    /// consumer falling back to `default_masking_rules()`, so the wiring needs a test of its
    /// own. The probe header is absent from the built-in sensitive list, so only rules built
    /// from this configuration mask it.
    #[tokio::test]
    async fn masking_rules_are_wired_into_the_router_pipeline() {
        let service = crate::TestHarness::builder()
            .configuration_json(serde_json::json!({
                "headers": {
                    "all": {
                        "request": {
                            "masking": { "sensitive_headers": ["x-wiring-probe"] }
                        }
                    }
                }
            }))
            .expect("valid config")
            .build_router()
            .await
            .expect("router pipeline");

        let response = tower::ServiceExt::oneshot(
            service,
            crate::services::router::Request::fake_builder()
                .build()
                .expect("valid request"),
        )
        .await
        .expect("router call");

        let masks_probe = response.context.extensions().with_lock(|lock| {
            lock.get::<Arc<crate::services::header_masking::MaskingRulesMap>>()
                .map(|rules| rules.get_request(None).should_mask("x-wiring-probe"))
        });

        assert_eq!(
            masks_probe,
            Some(true),
            "the configured masking rules never reached the request context",
        );
    }
}
