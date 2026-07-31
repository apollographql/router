use std::collections::HashSet;
use std::ops::ControlFlow;
use std::sync::Arc;
use std::time::Duration;

use sha2::Digest;
use tower::BoxError;
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

use super::RouterPlugin;
use super::config::WasmConfig;
use super::config::WasmFailure;
use super::config::WasmFailureMode;
use super::config::WasmHook;
use super::config::WasmHookConfig;
use super::config::WasmLimits;
use super::config::WasmSource;
use super::hooks;
use crate::services::subgraph;
use crate::services::supergraph;

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
    hooks: Arc<Vec<WasmHookConfig>>,
    limits: WasmLimits,
    failure: WasmFailure,
    concurrency: Arc<tokio::sync::Semaphore>,
    queue: Arc<tokio::sync::Semaphore>,
}

pub(super) struct Runtime {
    engine: Engine,
    linker: Linker<StoreData>,
    plugins: Vec<LoadedPlugin>,
    epoch_task: tokio::task::JoinHandle<()>,
}

impl Runtime {
    pub(super) fn new(config: WasmConfig) -> Result<Self, BoxError> {
        let mut engine_config = WasmtimeConfig::new();
        engine_config.cache(Some(wasmtime::Cache::new(wasmtime::CacheConfig::new())?));
        engine_config.epoch_interruption(true);
        engine_config.wasm_component_model(true);
        let engine = Engine::new(&engine_config)?;
        let mut linker = Linker::new(&engine);
        wasmtime_wasi::p2::add_to_linker_async(&mut linker)?;

        let defaults = config.defaults;
        let mut names = HashSet::new();
        let mut plugins = Vec::new();
        for plugin in config.plugins.into_iter().filter(|plugin| plugin.enabled) {
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
            engine,
            linker,
            plugins,
            epoch_task,
        })
    }

    pub(super) async fn process_supergraph_request(
        &self,
        mut request: supergraph::Request,
    ) -> Result<ControlFlow<supergraph::Response, supergraph::Request>, BoxError> {
        for plugin in &self.plugins {
            let Some(hook) = plugin
                .hooks
                .iter()
                .find(|hook| hook.hook == WasmHook::SupergraphRequest)
            else {
                continue;
            };
            let event = hooks::supergraph_event(&request, hook, &plugin.configuration)?;
            match self.invoke(plugin, event).await {
                Ok(super::wit::Outcome::Proceed(mutation)) => {
                    hooks::apply_supergraph_mutation(&mut request, hook, mutation)?;
                }
                Ok(super::wit::Outcome::BreakRequest(response)) => {
                    return Ok(ControlFlow::Break(hooks::break_supergraph_response(
                        request.context,
                        response,
                    )?));
                }
                Err(error) => {
                    let failure = hook.failure.unwrap_or(plugin.failure);
                    if matches!(failure.default, WasmFailureMode::Open) {
                        tracing::error!(plugin = %plugin.name, %error, "wasm plugin failed open");
                        continue;
                    }
                    return Err(format!("wasm plugin `{}` failed: {error}", plugin.name).into());
                }
            }
        }
        Ok(ControlFlow::Continue(request))
    }

    pub(super) async fn process_subgraph_request(
        &self,
        mut request: subgraph::Request,
        service_name: &str,
    ) -> Result<ControlFlow<subgraph::Response, subgraph::Request>, BoxError> {
        for plugin in &self.plugins {
            let Some(hook) = plugin.hooks.iter().find(|hook| {
                hook.hook == WasmHook::SubgraphRequest
                    && hook.selector.matches_service(service_name)
            }) else {
                continue;
            };
            let event = hooks::subgraph_event(&request, hook, &plugin.configuration)?;
            match self.invoke(plugin, event).await {
                Ok(super::wit::Outcome::Proceed(mutation)) => {
                    hooks::apply_subgraph_mutation(&mut request, hook, mutation)?;
                }
                Ok(super::wit::Outcome::BreakRequest(response)) => {
                    return Ok(ControlFlow::Break(hooks::break_subgraph_response(
                        request, response,
                    )?));
                }
                Err(error) => {
                    let failure = hook.failure.unwrap_or(plugin.failure);
                    if matches!(failure.default, WasmFailureMode::Open) {
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
        event: super::wit::Event,
    ) -> Result<super::wit::Outcome, BoxError> {
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
}

impl Drop for Runtime {
    fn drop(&mut self) {
        self.epoch_task.abort();
    }
}

fn validate_limits(name: &str, limits: &WasmLimits) -> Result<(), BoxError> {
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

pub(super) fn load_source(name: &str, source: &WasmSource) -> Result<Vec<u8>, BoxError> {
    match source {
        WasmSource::File { path, digest } => {
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

fn event_size(event: &super::wit::Event) -> u64 {
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

fn outcome_size(outcome: &super::wit::Outcome) -> u64 {
    fn headers_size(headers: &[super::wit::Header]) -> usize {
        headers
            .iter()
            .map(|header| header.name.len() + header.values.iter().map(String::len).sum::<usize>())
            .sum()
    }

    let size = match outcome {
        super::wit::Outcome::Proceed(mutation) => {
            let headers = mutation
                .headers
                .iter()
                .map(|operation| match operation {
                    super::wit::HeaderOperation::Set(header) => {
                        headers_size(std::slice::from_ref(header))
                    }
                    super::wit::HeaderOperation::Append(value) => {
                        value.name.len() + value.value.len()
                    }
                    super::wit::HeaderOperation::Remove(name) => name.len(),
                })
                .sum::<usize>();
            let context = mutation
                .context
                .iter()
                .map(|operation| match operation {
                    super::wit::ContextOperation::Set(entry) => {
                        entry.name.len() + entry.value.len()
                    }
                    super::wit::ContextOperation::Remove(name) => name.len(),
                })
                .sum::<usize>();
            headers + context + mutation.body.as_ref().map_or(0, String::len)
        }
        super::wit::Outcome::BreakRequest(response) => {
            headers_size(&response.headers) + response.body.len()
        }
    };
    size as u64
}
