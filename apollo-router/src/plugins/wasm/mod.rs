//! Sandboxed WebAssembly extensions for the router pipeline.

use std::collections::HashSet;
use std::ops::ControlFlow;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use bytesize::ByteSize;
use http::HeaderName;
use http::HeaderValue;
use schemars::JsonSchema;
use serde::Deserialize;
use sha2::Digest;
use tower::BoxError;
use tower::Service;
use tower::ServiceExt;
use tower::service_fn;
use wasmtime::Config as WasmtimeConfig;
use wasmtime::Engine;
use wasmtime::Store;
use wasmtime::StoreLimits;
use wasmtime::StoreLimitsBuilder;
use wasmtime::component::Component;
use wasmtime::component::Linker;
use wasmtime::component::ResourceTable;
use wasmtime_wasi::WasiCtx;
use wasmtime_wasi::WasiCtxBuilder;
use wasmtime_wasi::WasiCtxView;
use wasmtime_wasi::WasiView;

use self::exports::apollo::router_plugin::hooks;
use crate::Context;
use crate::graphql;
use crate::plugin::PluginInit;
use crate::plugin::PluginPrivate;
use crate::services::subgraph;
use crate::services::supergraph;

wasmtime::component::bindgen!({
    path: "wit/router-plugin",
    world: "router-plugin",
    exports: { default: async },
});

#[derive(Clone, Debug, Default, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
struct Conf {
    defaults: Defaults,
    plugins: Vec<WasmPluginConfig>,
}

#[derive(Clone, Debug, Default, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
struct Defaults {
    limits: Limits,
    failure: Failure,
}

#[derive(Clone, Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct WasmPluginConfig {
    name: String,
    #[serde(default = "default_enabled")]
    enabled: bool,
    source: Source,
    #[serde(default = "empty_object")]
    configuration: serde_json::Value,
    hooks: Vec<HookConfig>,
    #[serde(default)]
    limits: LimitsOverride,
    #[serde(default)]
    failure: Option<Failure>,
}

fn default_enabled() -> bool {
    true
}

fn empty_object() -> serde_json::Value {
    serde_json::Value::Object(Default::default())
}

#[derive(Clone, Debug, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
enum Source {
    File {
        path: PathBuf,
        #[serde(default)]
        digest: Option<String>,
    },
}

#[derive(Clone, Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct HookConfig {
    hook: Hook,
    #[serde(default)]
    selector: HookSelector,
    #[serde(default)]
    permissions: Permissions,
    #[serde(default)]
    failure: Option<Failure>,
}

#[derive(Clone, Copy, Debug, Deserialize, JsonSchema, PartialEq, Eq)]
enum Hook {
    #[serde(rename = "supergraph.request")]
    SupergraphRequest,
    #[serde(rename = "subgraph.request")]
    SubgraphRequest,
}

#[derive(Clone, Debug, Default, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
struct HookSelector {
    service_names: HashSet<String>,
}

impl HookSelector {
    fn matches_service(&self, service_name: &str) -> bool {
        self.service_names.is_empty() || self.service_names.contains(service_name)
    }
}

#[derive(Clone, Debug, Default, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
struct Permissions {
    headers: HeaderPermissions,
    context: ContextPermissions,
    graphql: GraphqlPermissions,
}

#[derive(Clone, Debug, Default, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
struct HeaderPermissions {
    read: NameMatcher,
    write: NameMatcher,
}

#[derive(Clone, Debug, Default, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
struct ContextPermissions {
    read: NameMatcher,
    write: NameMatcher,
}

#[derive(Clone, Debug, Default, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
struct NameMatcher {
    names: HashSet<String>,
}

impl NameMatcher {
    fn contains(&self, name: &str) -> bool {
        self.names
            .iter()
            .any(|candidate| candidate.eq_ignore_ascii_case(name))
    }
}

#[derive(Clone, Debug, Default, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
struct GraphqlPermissions {
    request: GraphqlAccess,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum GraphqlAccess {
    #[default]
    None,
    Read,
    ReadWrite,
}

#[derive(Clone, Debug, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
struct Limits {
    #[serde(deserialize_with = "humantime_serde::deserialize")]
    #[schemars(with = "String")]
    execution_timeout: Duration,
    #[serde(deserialize_with = "humantime_serde::deserialize")]
    #[schemars(with = "String")]
    queue_timeout: Duration,
    #[schemars(with = "String")]
    max_memory_per_instance: ByteSize,
    max_concurrency: usize,
    max_queue_size: usize,
    #[schemars(with = "String")]
    max_input_size: ByteSize,
    #[schemars(with = "String")]
    max_output_size: ByteSize,
}

#[derive(Clone, Debug, Default, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
struct LimitsOverride {
    #[serde(default, deserialize_with = "deserialize_optional_duration")]
    #[schemars(with = "Option<String>")]
    execution_timeout: Option<Duration>,
    #[serde(default, deserialize_with = "deserialize_optional_duration")]
    #[schemars(with = "Option<String>")]
    queue_timeout: Option<Duration>,
    #[schemars(with = "Option<String>")]
    max_memory_per_instance: Option<ByteSize>,
    max_concurrency: Option<usize>,
    max_queue_size: Option<usize>,
    #[schemars(with = "Option<String>")]
    max_input_size: Option<ByteSize>,
    #[schemars(with = "Option<String>")]
    max_output_size: Option<ByteSize>,
}

impl LimitsOverride {
    fn apply_to(self, mut limits: Limits) -> Limits {
        limits.execution_timeout = self.execution_timeout.unwrap_or(limits.execution_timeout);
        limits.queue_timeout = self.queue_timeout.unwrap_or(limits.queue_timeout);
        limits.max_memory_per_instance = self
            .max_memory_per_instance
            .unwrap_or(limits.max_memory_per_instance);
        limits.max_concurrency = self.max_concurrency.unwrap_or(limits.max_concurrency);
        limits.max_queue_size = self.max_queue_size.unwrap_or(limits.max_queue_size);
        limits.max_input_size = self.max_input_size.unwrap_or(limits.max_input_size);
        limits.max_output_size = self.max_output_size.unwrap_or(limits.max_output_size);
        limits
    }
}

fn deserialize_optional_duration<'de, D>(deserializer: D) -> Result<Option<Duration>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    humantime_serde::deserialize(deserializer).map(Some)
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            execution_timeout: Duration::from_millis(10),
            queue_timeout: Duration::from_millis(5),
            max_memory_per_instance: ByteSize::mib(16),
            max_concurrency: 128,
            max_queue_size: 256,
            max_input_size: ByteSize::mib(1),
            max_output_size: ByteSize::mib(1),
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
struct Failure {
    default: FailureMode,
}

impl Default for Failure {
    fn default() -> Self {
        Self {
            default: FailureMode::Closed,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
enum FailureMode {
    Open,
    #[default]
    Closed,
}

struct StoreData {
    limits: StoreLimits,
    table: ResourceTable,
    wasi: WasiCtx,
}

impl WasiView for StoreData {
    fn ctx(&mut self) -> WasiCtxView<'_> {
        WasiCtxView {
            ctx: &mut self.wasi,
            table: &mut self.table,
        }
    }
}

#[derive(Clone)]
struct LoadedPlugin {
    name: Arc<str>,
    component: Component,
    configuration: Arc<str>,
    hooks: Arc<Vec<HookConfig>>,
    limits: Limits,
    failure: Failure,
    concurrency: Arc<tokio::sync::Semaphore>,
    queue: Arc<tokio::sync::Semaphore>,
}

struct Runtime {
    engine: Engine,
    linker: Linker<StoreData>,
    plugins: Vec<LoadedPlugin>,
    epoch_task: tokio::task::JoinHandle<()>,
}

impl Drop for Runtime {
    fn drop(&mut self) {
        self.epoch_task.abort();
    }
}

struct Wasm {
    runtime: Arc<Runtime>,
}

#[async_trait::async_trait]
impl PluginPrivate for Wasm {
    type Config = Conf;

    async fn new(init: PluginInit<Self::Config>) -> Result<Self, BoxError> {
        let mut engine_config = WasmtimeConfig::new();
        engine_config.epoch_interruption(true);
        engine_config.wasm_component_model(true);
        let engine = Engine::new(&engine_config)?;
        let mut linker = Linker::new(&engine);
        wasmtime_wasi::p2::add_to_linker_async(&mut linker)?;

        let defaults = init.config.defaults;
        let mut names = HashSet::new();
        let mut plugins = Vec::new();
        for plugin in init
            .config
            .plugins
            .into_iter()
            .filter(|plugin| plugin.enabled)
        {
            if plugin.name.trim().is_empty() {
                return Err("wasm plugin names must not be empty".into());
            }
            if !names.insert(plugin.name.clone()) {
                return Err(format!("duplicate wasm plugin name `{}`", plugin.name).into());
            }
            if plugin.hooks.is_empty() {
                return Err(format!(
                    "wasm plugin `{}` must configure at least one hook",
                    plugin.name
                )
                .into());
            }

            let limits = plugin.limits.apply_to(defaults.limits.clone());
            validate_limits(&plugin.name, &limits)?;
            let failure = plugin.failure.unwrap_or(defaults.failure);
            let bytes = load_source(&plugin.name, &plugin.source)?;
            let component = Component::new(&engine, &bytes).map_err(|error| {
                format!("failed to compile wasm plugin `{}`: {error}", plugin.name)
            })?;
            let configuration = serde_json::to_string(&plugin.configuration)?;
            plugins.push(LoadedPlugin {
                name: Arc::from(plugin.name),
                component,
                configuration: Arc::from(configuration),
                hooks: Arc::new(plugin.hooks),
                concurrency: Arc::new(tokio::sync::Semaphore::new(limits.max_concurrency)),
                queue: Arc::new(tokio::sync::Semaphore::new(limits.max_queue_size)),
                limits,
                failure,
            });
        }

        let ticker_engine = engine.clone();
        let epoch_task = tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_millis(1));
            loop {
                interval.tick().await;
                ticker_engine.increment_epoch();
            }
        });

        Ok(Self {
            runtime: Arc::new(Runtime {
                engine,
                linker,
                plugins,
                epoch_task,
            }),
        })
    }

    fn supergraph_service(
        &self,
        service: supergraph::BoxCloneService,
    ) -> supergraph::BoxCloneService {
        let runtime = self.runtime.clone();
        tower::util::BoxCloneService::new(service_fn(move |request: supergraph::Request| {
            let runtime = runtime.clone();
            let mut service = service.clone();
            async move {
                match runtime.process_supergraph_request(request).await? {
                    ControlFlow::Continue(request) => service.ready().await?.call(request).await,
                    ControlFlow::Break(response) => Ok(response),
                }
            }
        }))
    }

    fn subgraph_service(
        &self,
        service_name: &str,
        service: subgraph::BoxCloneService,
    ) -> subgraph::BoxCloneService {
        let runtime = self.runtime.clone();
        let service_name: Arc<str> = Arc::from(service_name);
        tower::util::BoxCloneService::new(service_fn(move |request: subgraph::Request| {
            let runtime = runtime.clone();
            let service_name = service_name.clone();
            let mut service = service.clone();
            async move {
                match runtime
                    .process_subgraph_request(request, &service_name)
                    .await?
                {
                    ControlFlow::Continue(request) => service.ready().await?.call(request).await,
                    ControlFlow::Break(response) => Ok(response),
                }
            }
        }))
    }
}

impl Runtime {
    async fn process_supergraph_request(
        &self,
        mut request: supergraph::Request,
    ) -> Result<ControlFlow<supergraph::Response, supergraph::Request>, BoxError> {
        for plugin in &self.plugins {
            let Some(hook) = plugin
                .hooks
                .iter()
                .find(|hook| hook.hook == Hook::SupergraphRequest)
            else {
                continue;
            };
            let event = supergraph_event(&request, hook, &plugin.configuration)?;
            match self.invoke(plugin, event).await {
                Ok(hooks::Outcome::Proceed(mutation)) => {
                    apply_supergraph_mutation(&mut request, hook, mutation)?;
                }
                Ok(hooks::Outcome::BreakRequest(response)) => {
                    return Ok(ControlFlow::Break(break_supergraph_response(
                        request.context,
                        response,
                    )?));
                }
                Err(error) => {
                    let failure = hook.failure.unwrap_or(plugin.failure);
                    if matches!(failure.default, FailureMode::Open) {
                        tracing::error!(plugin = %plugin.name, %error, "wasm plugin failed open");
                        continue;
                    }
                    return Err(format!("wasm plugin `{}` failed: {error}", plugin.name).into());
                }
            }
        }
        Ok(ControlFlow::Continue(request))
    }

    async fn invoke(
        &self,
        plugin: &LoadedPlugin,
        event: hooks::Event,
    ) -> Result<hooks::Outcome, BoxError> {
        if event_size(&event) > plugin.limits.max_input_size.as_u64() {
            return Err(format!("input exceeded {}", plugin.limits.max_input_size).into());
        }

        let queue_permit = plugin
            .queue
            .clone()
            .try_acquire_owned()
            .map_err(|_| "wasm plugin queue is full")?;
        let concurrency_permit = tokio::time::timeout(
            plugin.limits.queue_timeout,
            plugin.concurrency.clone().acquire_owned(),
        )
        .await
        .map_err(|_| "wasm plugin queue timeout")??;
        drop(queue_permit);

        let store_limits = StoreLimitsBuilder::new()
            .memory_size(plugin.limits.max_memory_per_instance.as_u64() as usize)
            .instances(64)
            .memories(16)
            .tables(16)
            .trap_on_grow_failure(true)
            .build();
        let mut store = Store::new(
            &self.engine,
            StoreData {
                limits: store_limits,
                table: ResourceTable::new(),
                // No inherited stdio, environment, filesystem, or network capabilities.
                wasi: WasiCtxBuilder::new()
                    .allow_tcp(false)
                    .allow_udp(false)
                    .build(),
            },
        );
        store.limiter(|data| &mut data.limits);
        let deadline = plugin.limits.execution_timeout.as_millis().max(1) as u64;
        store.set_epoch_deadline(deadline);
        store.epoch_deadline_trap();

        let bindings =
            RouterPlugin::instantiate_async(&mut store, &plugin.component, &self.linker).await?;
        let call = bindings
            .apollo_router_plugin_hooks()
            .call_handle(&mut store, &event);
        let outcome = tokio::time::timeout(plugin.limits.execution_timeout, call)
            .await
            .map_err(|_| "wasm plugin execution timeout")???;
        drop(concurrency_permit);

        if outcome_size(&outcome) > plugin.limits.max_output_size.as_u64() {
            return Err(format!("output exceeded {}", plugin.limits.max_output_size).into());
        }
        Ok(outcome)
    }

    async fn process_subgraph_request(
        &self,
        mut request: subgraph::Request,
        service_name: &str,
    ) -> Result<ControlFlow<subgraph::Response, subgraph::Request>, BoxError> {
        for plugin in &self.plugins {
            let Some(hook) = plugin.hooks.iter().find(|hook| {
                hook.hook == Hook::SubgraphRequest && hook.selector.matches_service(service_name)
            }) else {
                continue;
            };
            let event = subgraph_event(&request, hook, &plugin.configuration)?;
            match self.invoke(plugin, event).await {
                Ok(hooks::Outcome::Proceed(mutation)) => {
                    apply_subgraph_mutation(&mut request, hook, mutation)?;
                }
                Ok(hooks::Outcome::BreakRequest(response)) => {
                    return Ok(ControlFlow::Break(break_subgraph_response(
                        request, response,
                    )?));
                }
                Err(error) => {
                    let failure = hook.failure.unwrap_or(plugin.failure);
                    if matches!(failure.default, FailureMode::Open) {
                        tracing::error!(plugin = %plugin.name, %error, "wasm plugin failed open");
                        continue;
                    }
                    return Err(format!("wasm plugin `{}` failed: {error}", plugin.name).into());
                }
            }
        }
        Ok(ControlFlow::Continue(request))
    }
}

fn event_size(event: &hooks::Event) -> u64 {
    let fixed = event.hook.len()
        + event.request_id.len()
        + event.service_name.as_ref().map_or(0, String::len)
        + event.method.as_ref().map_or(0, String::len)
        + event.uri.as_ref().map_or(0, String::len)
        + event.body.as_ref().map_or(0, String::len)
        + event.configuration.len();
    let headers = event
        .headers
        .iter()
        .map(|header| header.name.len() + header.values.iter().map(String::len).sum::<usize>())
        .sum::<usize>();
    let context = event
        .context
        .iter()
        .map(|entry| entry.name.len() + entry.value.len())
        .sum::<usize>();
    (fixed + headers + context) as u64
}

fn outcome_size(outcome: &hooks::Outcome) -> u64 {
    fn headers_size(headers: &[hooks::Header]) -> usize {
        headers
            .iter()
            .map(|header| header.name.len() + header.values.iter().map(String::len).sum::<usize>())
            .sum()
    }

    let size = match outcome {
        hooks::Outcome::Proceed(mutation) => {
            let headers = mutation
                .headers
                .iter()
                .map(|operation| match operation {
                    hooks::HeaderOperation::Set(header) => {
                        headers_size(std::slice::from_ref(header))
                    }
                    hooks::HeaderOperation::Append(value) => value.name.len() + value.value.len(),
                    hooks::HeaderOperation::Remove(name) => name.len(),
                })
                .sum::<usize>();
            let context = mutation
                .context
                .iter()
                .map(|operation| match operation {
                    hooks::ContextOperation::Set(entry) => entry.name.len() + entry.value.len(),
                    hooks::ContextOperation::Remove(name) => name.len(),
                })
                .sum::<usize>();
            headers + context + mutation.body.as_ref().map_or(0, String::len)
        }
        hooks::Outcome::BreakRequest(response) => {
            headers_size(&response.headers) + response.body.len()
        }
    };
    size as u64
}

fn validate_limits(name: &str, limits: &Limits) -> Result<(), BoxError> {
    if limits.execution_timeout.is_zero()
        || limits.max_concurrency == 0
        || limits.max_queue_size == 0
    {
        return Err(format!(
            "wasm plugin `{name}` must have a positive timeout, concurrency, and queue size"
        )
        .into());
    }
    Ok(())
}

fn load_source(name: &str, source: &Source) -> Result<Vec<u8>, BoxError> {
    match source {
        Source::File { path, digest } => {
            let bytes = std::fs::read(path).map_err(|error| {
                format!(
                    "failed to read wasm plugin `{name}` from {}: {error}",
                    path.display()
                )
            })?;
            if let Some(expected) = digest {
                let actual = format!("sha256:{:x}", sha2::Sha256::digest(&bytes));
                if &actual != expected {
                    return Err(format!(
                        "digest mismatch for wasm plugin `{name}`: expected `{expected}`, got `{actual}`"
                    )
                    .into());
                }
            }
            Ok(bytes)
        }
    }
}

fn supergraph_event(
    request: &supergraph::Request,
    hook: &HookConfig,
    configuration: &str,
) -> Result<hooks::Event, BoxError> {
    let permissions = &hook.permissions;
    let headers = externalize_headers(
        request.supergraph_request.headers(),
        &permissions.headers.read,
    );
    let context = externalize_context(&request.context, &permissions.context.read)?;
    let body = (!matches!(permissions.graphql.request, GraphqlAccess::None))
        .then(|| serde_json::to_string(request.supergraph_request.body()))
        .transpose()?;

    Ok(hooks::Event {
        hook: "supergraph.request".to_string(),
        request_id: request.context.id.clone(),
        service_name: None,
        method: Some(request.supergraph_request.method().to_string()),
        uri: Some(request.supergraph_request.uri().to_string()),
        headers,
        context,
        body,
        configuration: configuration.to_string(),
    })
}

fn externalize_headers(source: &http::HeaderMap, allowed: &NameMatcher) -> Vec<hooks::Header> {
    source
        .iter()
        .filter(|(name, _)| allowed.contains(name.as_str()))
        .fold(Vec::<hooks::Header>::new(), |mut headers, (name, value)| {
            if let Some(existing) = headers
                .iter_mut()
                .find(|header| header.name == name.as_str())
            {
                if let Ok(value) = value.to_str() {
                    existing.values.push(value.to_string());
                }
            } else if let Ok(value) = value.to_str() {
                headers.push(hooks::Header {
                    name: name.to_string(),
                    values: vec![value.to_string()],
                });
            }
            headers
        })
}

fn externalize_context(
    context: &Context,
    allowed: &NameMatcher,
) -> Result<Vec<hooks::ContextEntry>, BoxError> {
    context
        .iter()
        .filter(|entry| allowed.contains(entry.key()))
        .map(|entry| {
            Ok(hooks::ContextEntry {
                name: entry.key().clone(),
                value: serde_json::to_string(entry.value())?,
            })
        })
        .collect()
}

fn apply_header_mutations(
    headers: &mut http::HeaderMap,
    allowed: &NameMatcher,
    operations: Vec<hooks::HeaderOperation>,
) -> Result<(), BoxError> {
    for operation in operations {
        match operation {
            hooks::HeaderOperation::Set(header) => {
                ensure_allowed(allowed, &header.name, "header")?;
                let name = HeaderName::try_from(header.name)?;
                headers.remove(&name);
                for value in header.values {
                    headers.append(name.clone(), HeaderValue::try_from(value)?);
                }
            }
            hooks::HeaderOperation::Append(value) => {
                ensure_allowed(allowed, &value.name, "header")?;
                headers.append(
                    HeaderName::try_from(value.name)?,
                    HeaderValue::try_from(value.value)?,
                );
            }
            hooks::HeaderOperation::Remove(name) => {
                ensure_allowed(allowed, &name, "header")?;
                headers.remove(HeaderName::try_from(name)?);
            }
        }
    }
    Ok(())
}

fn apply_context_mutations(
    context: &Context,
    allowed: &NameMatcher,
    operations: Vec<hooks::ContextOperation>,
) -> Result<(), BoxError> {
    for operation in operations {
        match operation {
            hooks::ContextOperation::Set(entry) => {
                ensure_allowed(allowed, &entry.name, "context")?;
                let value: serde_json::Value = serde_json::from_str(&entry.value)?;
                context.insert_json_value(entry.name, serde_json_bytes::to_value(value)?);
            }
            hooks::ContextOperation::Remove(name) => {
                ensure_allowed(allowed, &name, "context")?;
                context.retain(|key, _| key != &name);
            }
        }
    }
    Ok(())
}

fn apply_supergraph_mutation(
    request: &mut supergraph::Request,
    hook: &HookConfig,
    mutation: hooks::Mutation,
) -> Result<(), BoxError> {
    apply_header_mutations(
        request.supergraph_request.headers_mut(),
        &hook.permissions.headers.write,
        mutation.headers,
    )?;
    apply_context_mutations(
        &request.context,
        &hook.permissions.context.write,
        mutation.context,
    )?;
    if let Some(body) = mutation.body {
        if !matches!(hook.permissions.graphql.request, GraphqlAccess::ReadWrite) {
            return Err(
                "wasm plugin attempted to modify the GraphQL request without write permission"
                    .into(),
            );
        }
        *request.supergraph_request.body_mut() = serde_json::from_str(&body)?;
    }
    Ok(())
}

fn subgraph_event(
    request: &subgraph::Request,
    hook: &HookConfig,
    configuration: &str,
) -> Result<hooks::Event, BoxError> {
    let permissions = &hook.permissions;
    let headers = externalize_headers(
        request.subgraph_request.headers(),
        &permissions.headers.read,
    );
    let context = externalize_context(&request.context, &permissions.context.read)?;
    let body = (!matches!(permissions.graphql.request, GraphqlAccess::None))
        .then(|| serde_json::to_string(request.subgraph_request.body()))
        .transpose()?;

    Ok(hooks::Event {
        hook: "subgraph.request".to_string(),
        request_id: request.context.id.clone(),
        service_name: Some(request.subgraph_name.clone()),
        method: Some(request.subgraph_request.method().to_string()),
        uri: Some(request.subgraph_request.uri().to_string()),
        headers,
        context,
        body,
        configuration: configuration.to_string(),
    })
}

fn apply_subgraph_mutation(
    request: &mut subgraph::Request,
    hook: &HookConfig,
    mutation: hooks::Mutation,
) -> Result<(), BoxError> {
    apply_header_mutations(
        request.subgraph_request.headers_mut(),
        &hook.permissions.headers.write,
        mutation.headers,
    )?;
    apply_context_mutations(
        &request.context,
        &hook.permissions.context.write,
        mutation.context,
    )?;
    if let Some(body) = mutation.body {
        if !matches!(hook.permissions.graphql.request, GraphqlAccess::ReadWrite) {
            return Err(
                "wasm plugin attempted to modify the GraphQL request without write permission"
                    .into(),
            );
        }
        *request.subgraph_request.body_mut() = serde_json::from_str(&body)?;
    }
    Ok(())
}

fn break_subgraph_response(
    request: subgraph::Request,
    response: hooks::BreakResponse,
) -> Result<subgraph::Response, BoxError> {
    let body: graphql::Response = serde_json::from_str(&response.body)?;
    let mut builder = http::Response::builder().status(response.status_code);
    for header in response.headers {
        let name = HeaderName::try_from(header.name)?;
        for value in header.values {
            builder = builder.header(name.clone(), HeaderValue::try_from(value)?);
        }
    }
    Ok(subgraph::Response::new_from_response(
        builder.body(body)?,
        request.context,
        request.subgraph_name,
        request.id,
    ))
}

fn ensure_allowed(matcher: &NameMatcher, name: &str, kind: &str) -> Result<(), BoxError> {
    if matcher.contains(name) {
        Ok(())
    } else {
        Err(format!("wasm plugin attempted to write unauthorized {kind} `{name}`").into())
    }
}

fn break_supergraph_response(
    context: Context,
    response: hooks::BreakResponse,
) -> Result<supergraph::Response, BoxError> {
    let body: graphql::Response = serde_json::from_str(&response.body)?;
    let mut result = supergraph::Response::new_from_graphql_response(body, context);
    *result.response.status_mut() = http::StatusCode::from_u16(response.status_code)?;
    for header in response.headers {
        let name = HeaderName::try_from(header.name)?;
        for value in header.values {
            result
                .response
                .headers_mut()
                .append(name.clone(), HeaderValue::try_from(value)?);
        }
    }
    Ok(result)
}

register_private_plugin!("apollo", "wasm", Wasm);

#[cfg(test)]
mod tests {
    use std::io::Write;

    use super::*;

    #[test]
    fn plugin_limit_overrides_inherit_configured_defaults() {
        let config: Conf = serde_yaml::from_str(
            r#"
defaults:
  limits:
    execution_timeout: 40ms
    max_memory_per_instance: 24MiB
plugins:
  - name: policy
    source:
      type: file
      path: policy.wasm
    hooks:
      - hook: supergraph.request
    limits:
      execution_timeout: 5ms
"#,
        )
        .expect("valid configuration");

        let limits = config.plugins[0]
            .limits
            .clone()
            .apply_to(config.defaults.limits);
        assert_eq!(limits.execution_timeout, Duration::from_millis(5));
        assert_eq!(limits.max_memory_per_instance, ByteSize::mib(24));
        assert_eq!(limits.max_concurrency, 128);
    }

    #[test]
    fn config_has_no_version_or_verification_policy() {
        assert!(serde_yaml::from_str::<Conf>("version: 1").is_err());
        assert!(serde_yaml::from_str::<Conf>("verification: required").is_err());

        let config: Conf = serde_yaml::from_str(
            r#"
plugins:
  - name: policy
    source:
      type: file
      path: policy.wasm
      digest: sha256:abc
    configuration:
      policy_name: checkout
    hooks:
      - hook: supergraph.request
"#,
        )
        .expect("valid configuration");
        let Source::File { digest, .. } = &config.plugins[0].source;
        assert_eq!(digest.as_deref(), Some("sha256:abc"));
        assert_eq!(config.plugins[0].configuration["policy_name"], "checkout");
    }

    #[test]
    fn names_are_case_insensitive() {
        let matcher = NameMatcher {
            names: HashSet::from(["Authorization".to_string()]),
        };
        assert!(matcher.contains("authorization"));
        assert!(matcher.contains("AUTHORIZATION"));
        assert!(!matcher.contains("x-other"));
    }

    #[test]
    fn source_digest_is_verified() {
        let mut file = tempfile::NamedTempFile::new().expect("temporary file");
        file.write_all(b"component").expect("write component");
        let digest = format!("sha256:{:x}", sha2::Sha256::digest(b"component"));
        let source = Source::File {
            path: file.path().to_path_buf(),
            digest: Some(digest),
        };
        assert_eq!(load_source("test", &source).unwrap(), b"component");

        let invalid = Source::File {
            path: file.path().to_path_buf(),
            digest: Some("sha256:invalid".to_string()),
        };
        assert!(load_source("test", &invalid).is_err());
    }

    #[test]
    fn unauthorized_header_mutation_is_rejected() {
        let mut headers = http::HeaderMap::new();
        let result = apply_header_mutations(
            &mut headers,
            &NameMatcher::default(),
            vec![hooks::HeaderOperation::Set(hooks::Header {
                name: "x-not-allowed".to_string(),
                values: vec!["value".to_string()],
            })],
        );
        assert!(result.is_err());
        assert!(headers.is_empty());
    }

    #[test]
    fn empty_selector_matches_every_service() {
        let selector = HookSelector::default();
        assert!(selector.matches_service("products"));

        let selector = HookSelector {
            service_names: HashSet::from(["inventory".to_string()]),
        };
        assert!(selector.matches_service("inventory"));
        assert!(!selector.matches_service("products"));
    }
}
