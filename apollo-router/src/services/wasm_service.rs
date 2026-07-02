//! Runtime for hosting WebAssembly **components** as GraphQL data sources.
//!
//! A component (built with the toolchain in `router-wasm-plugins`/`wit-gql`) exports WIT functions
//! that each map to a GraphQL field. This module owns the embedded wasmtime host: it compiles a
//! component once, and per fetch it instantiates the component, invokes the WIT export that backs
//! the requested GraphQL field, and returns the export's `result<string, string>` (the raw
//! REST-style JSON, or an error string).
//!
//! The dispatch registry ([`WasmComponentServiceFactory`]) is the wasm analog of
//! [`ConnectorServiceFactory`](crate::services::connector_service::ConnectorServiceFactory): it maps
//! a supergraph subgraph name to the [`WasmComponent`] that backs it. It is built by the
//! `experimental_wasm_data_sources` plugin and threaded into the `FetchService` so a fetch to a
//! wasm-backed subgraph is dispatched here instead of over HTTP.
//!
//! The whole module is gated behind the `wasm-components` cargo feature (wasmtime is a heavy dep).

use std::collections::BTreeMap;
use std::collections::HashMap;
use std::sync::Arc;

use anyhow::{Result, anyhow};
use indexmap::IndexMap;
use serde_json_bytes::ByteString;
use serde_json_bytes::Map as JsonMap;
use serde_json_bytes::Value as JsonValue;
use tower::BoxError;
use wasmtime::component::types::Type;
use wasmtime::component::{Component, HasData, Linker, Resource, ResourceTable, Val};
use wasmtime::{Config, Engine, Store};
use wasmtime_wasi::{WasiCtx, WasiCtxView, WasiView};
use wasmtime_wasi_http::WasiHttpCtx;
use wasmtime_wasi_http::p2::{WasiHttpCtxView, WasiHttpView};
use wit_gql::naming::to_camel_case;
use wit_gql::{ExportLocation, FieldMapping, OperationMap};

use crate::error::Error;
use crate::graphql;
use crate::json_ext::Path;
use crate::query_planner::fetch::{FetchNode, Variables};
use crate::spec::Schema;

/// Host bindings for `wasmcloud:secrets@1.0.0` — the single keystore interface the router provides
/// to components. Resource-based: `store.get(key) -> secret` (an opaque handle), then
/// `reveal.reveal(borrow<secret>) -> secret-value`. Components read their API keys through this.
mod secrets_bindings {
    wasmtime::component::bindgen!({
        inline: r#"
            package wasmcloud:secrets@1.0.0;
            interface store {
              variant secrets-error { upstream(string), io(string), not-found }
              variant secret-value { %string(string), bytes(list<u8>) }
              resource secret;
              get: func(key: string) -> result<secret, secrets-error>;
            }
            interface reveal {
              use store.{secret, secret-value};
              reveal: func(s: borrow<secret>) -> secret-value;
            }
            world imports { import store; import reveal; }
        "#,
        world: "imports",
        imports: { default: trappable },
        with: {
            "wasmcloud:secrets/store.secret": crate::services::wasm_service::HostSecret,
        },
    });
}
use secrets_bindings::wasmcloud::secrets::reveal as secrets_reveal;
use secrets_bindings::wasmcloud::secrets::store as secrets_store;

/// Host representation of a `wasmcloud:secrets` `secret` resource: the stored value.
/// `pub` (not `pub(crate)`) so the `bindgen!` `with`-mapping can re-export it; the enclosing module
/// is `pub(crate)`, so it is not actually exposed outside the crate.
#[allow(unreachable_pub)]
pub struct HostSecret {
    value: String,
}

/// View over the host state passed to the generated secrets host: the resource table plus the
/// per-component keystore.
struct SecretsView<'a> {
    table: &'a mut ResourceTable,
    secrets: &'a HashMap<String, String>,
}

/// Project a [`SecretsView`] out of the host state. A free `fn` (not a closure) so it has the
/// higher-ranked `for<'a> fn(&'a mut WasmHostCtx) -> SecretsView<'a>` signature `add_to_linker` wants.
fn secrets_view(c: &mut WasmHostCtx) -> SecretsView<'_> {
    SecretsView {
        table: &mut c.table,
        secrets: &c.secrets,
    }
}

impl secrets_store::Host for SecretsView<'_> {
    fn get(
        &mut self,
        key: String,
    ) -> wasmtime::Result<std::result::Result<Resource<HostSecret>, secrets_store::SecretsError>> {
        match self.secrets.get(&key) {
            Some(value) => Ok(Ok(self.table.push(HostSecret {
                value: value.clone(),
            })?)),
            None => Ok(Err(secrets_store::SecretsError::NotFound)),
        }
    }
}

impl secrets_store::HostSecret for SecretsView<'_> {
    fn drop(&mut self, rep: Resource<HostSecret>) -> wasmtime::Result<()> {
        self.table.delete(rep)?;
        Ok(())
    }
}

impl secrets_reveal::Host for SecretsView<'_> {
    fn reveal(&mut self, s: Resource<HostSecret>) -> wasmtime::Result<secrets_store::SecretValue> {
        let secret = self.table.get(&s)?;
        Ok(secrets_store::SecretValue::String(secret.value.clone()))
    }
}

struct HasSecrets;
impl HasData for HasSecrets {
    type Data<'a> = SecretsView<'a>;
}

/// Per-`Store` host state. Each invocation gets a fresh one so instances never share mutable state.
struct WasmHostCtx {
    table: ResourceTable,
    wasi: WasiCtx,
    http: WasiHttpCtx,
    /// The component's keystore, exposed via `wasmcloud:secrets`.
    secrets: HashMap<String, String>,
}

impl WasiView for WasmHostCtx {
    fn ctx(&mut self) -> WasiCtxView<'_> {
        WasiCtxView {
            ctx: &mut self.wasi,
            table: &mut self.table,
        }
    }
}

impl WasiHttpView for WasmHostCtx {
    fn http(&mut self) -> WasiHttpCtxView<'_> {
        WasiHttpCtxView {
            ctx: &mut self.http,
            table: &mut self.table,
            hooks: Default::default(),
        }
    }
}

/// The shared wasmtime engine + linker, built once and reused for every component.
///
/// The `Engine` is cheap to clone (internally reference-counted); the `Linker` is shared by
/// reference. Both are immutable after construction.
pub(crate) struct WasmRuntime {
    engine: Engine,
    linker: Linker<WasmHostCtx>,
}

impl WasmRuntime {
    /// Build the engine and register the host interfaces components import:
    /// `wasi:io`/`wasi:clocks`/… , `wasi:http/outgoing-handler`, and `wasmcloud:secrets`
    /// (the single keystore the router provides).
    pub(crate) fn new() -> Result<Self> {
        let mut config = Config::new();
        // Component model is on by default; `async_support` is a deprecated no-op in wasmtime 46.
        config.wasm_component_model(true);
        let engine = Engine::new(&config).map_err(|e| anyhow!("building wasm engine: {e}"))?;

        let mut linker: Linker<WasmHostCtx> = Linker::new(&engine);
        wasmtime_wasi::p2::add_to_linker_async(&mut linker)
            .map_err(|e| anyhow!("linking wasi: {e}"))?;
        wasmtime_wasi_http::p2::add_only_http_to_linker_async(&mut linker)
            .map_err(|e| anyhow!("linking wasi:http: {e}"))?;
        secrets_store::add_to_linker::<_, HasSecrets>(&mut linker, secrets_view)
            .map_err(|e| anyhow!("linking wasmcloud:secrets/store: {e}"))?;
        secrets_reveal::add_to_linker::<_, HasSecrets>(&mut linker, secrets_view)
            .map_err(|e| anyhow!("linking wasmcloud:secrets/reveal: {e}"))?;

        Ok(Self { engine, linker })
    }
}

/// A single compiled component plus the GraphQL-field → WIT-export mapping derived from its WIT.
pub(crate) struct WasmComponent {
    runtime: Arc<WasmRuntime>,
    component: Component,
    /// Keystore values (e.g. API keys) exposed to this component via `wasmcloud:secrets`.
    config: BTreeMap<String, String>,
    operation_map: OperationMap,
}

impl WasmComponent {
    /// Decode the component's WIT (via `wit-gql`, the same code the SDL generator uses) to build the
    /// dispatch [`OperationMap`], and compile the component with wasmtime (the expensive step, done
    /// once at load).
    pub(crate) fn compile(
        runtime: Arc<WasmRuntime>,
        bytes: &[u8],
        config: BTreeMap<String, String>,
    ) -> Result<Self> {
        let (resolve, world) =
            wit_gql::decode_component(bytes).map_err(|e| anyhow!("decoding component WIT: {e}"))?;
        let operation_map = wit_gql::operation_map(&resolve, world);
        let component = Component::from_binary(&runtime.engine, bytes)
            .map_err(|e| anyhow!("compiling component: {e}"))?;
        Ok(Self {
            runtime,
            component,
            config,
            operation_map,
        })
    }

    /// The GraphQL-field → WIT-export mapping (used by the fetch path to route root fields).
    pub(crate) fn operation_map(&self) -> &OperationMap {
        &self.operation_map
    }

    /// Invoke the WIT export backing `field`, passing `args` (a JSON object keyed by GraphQL
    /// argument name). Returns the export's `result<string, string>`: `Ok(json)` is the raw
    /// response body, `Err(msg)` a guest-side error. The outer `Result` is a host/runtime failure
    /// (instantiation, missing export, trap).
    pub(crate) async fn invoke(
        &self,
        field: &FieldMapping,
        args: &JsonValue,
    ) -> Result<std::result::Result<String, String>> {
        let ctx = WasmHostCtx {
            table: ResourceTable::new(),
            wasi: WasiCtx::builder().build(),
            http: WasiHttpCtx::new(),
            secrets: self.config.clone().into_iter().collect(),
        };
        let mut store = Store::new(&self.runtime.engine, ctx);

        let instance = self
            .runtime
            .linker
            .instantiate_async(&mut store, &self.component)
            .await
            .map_err(|e| anyhow!("instantiating component: {e}"))?;

        // Navigate to the exported function: either on the component root or within an interface
        // instance identified by its fully-qualified name (e.g. `incidentio:api/incidents-v2@0.1.0`).
        let func = match &field.export {
            ExportLocation::Root => {
                let idx = instance
                    .get_export_index(&mut store, None, &field.func)
                    .ok_or_else(|| anyhow!("export `{}` not found", field.func))?;
                instance.get_func(&mut store, &idx)
            }
            ExportLocation::Interface { instance_name } => {
                let iface = instance
                    .get_export_index(&mut store, None, instance_name)
                    .ok_or_else(|| anyhow!("interface export `{instance_name}` not found"))?;
                let idx = instance
                    .get_export_index(&mut store, Some(&iface), &field.func)
                    .ok_or_else(|| {
                        anyhow!("function `{}` not found in `{instance_name}`", field.func)
                    })?;
                instance.get_func(&mut store, &idx)
            }
        }
        .ok_or_else(|| anyhow!("could not resolve export for field `{}`", field.graphql_field))?;

        // Build the argument list, type-guided by the function's declared parameter types.
        let param_types: Vec<(String, Type)> = func
            .ty(&store)
            .params()
            .map(|(name, ty)| (name.to_string(), ty))
            .collect();
        let mut call_args = Vec::with_capacity(param_types.len());
        for (wit_param, ty) in &param_types {
            // Map the WIT parameter name back to the GraphQL argument name it was emitted as.
            let gql_arg = field
                .params
                .iter()
                .find(|p| &p.wit_param == wit_param)
                .map(|p| p.graphql_arg.as_str())
                .unwrap_or(wit_param.as_str());
            let value = get_field(args, gql_arg).unwrap_or(JsonValue::Null);
            call_args.push(json_to_val(ty, &value)?);
        }

        let mut results = vec![Val::Bool(false)];
        func.call_async(&mut store, &call_args, &mut results)
            .await
            .map_err(|e| anyhow!("invoking `{}`: {e}", field.graphql_field))?;
        // NB: `post_return_async` is a deprecated no-op in wasmtime 46; not called.

        decode_result_string(results.into_iter().next().unwrap_or(Val::Bool(false)))
    }
}

/// Registry mapping a supergraph subgraph name to the wasm component that backs it.
///
/// The wasm analog of `ConnectorServiceFactory { connectors_by_service_name }`. Built from router
/// config by the `experimental_wasm_data_sources` plugin, and threaded into the `FetchService`.
#[derive(Clone)]
pub(crate) struct WasmComponentServiceFactory {
    pub(crate) wasm_components_by_service_name: Arc<IndexMap<Arc<str>, Arc<WasmComponent>>>,
}

impl WasmComponentServiceFactory {
    /// An empty registry (used where no wasm data sources are configured).
    pub(crate) fn empty() -> Self {
        Self {
            wasm_components_by_service_name: Arc::new(IndexMap::new()),
        }
    }

    /// Build a registry from a name→component map.
    pub(crate) fn new(components: IndexMap<Arc<str>, Arc<WasmComponent>>) -> Self {
        Self {
            wasm_components_by_service_name: Arc::new(components),
        }
    }

    /// The component backing `service_name`, if any.
    pub(crate) fn get(&self, service_name: &str) -> Option<Arc<WasmComponent>> {
        self.wasm_components_by_service_name
            .get(service_name)
            .cloned()
    }
}

/// Convert a JSON value into a wasmtime `Val`, guided by the WIT parameter `Type`.
///
/// Record field names are matched by their camelCase GraphQL spelling (falling back to the raw WIT
/// name), mirroring how `wit-gql` emits the input types.
fn json_to_val(ty: &Type, v: &JsonValue) -> Result<Val> {
    Ok(match ty {
        Type::Bool => Val::Bool(v.as_bool().unwrap_or(false)),
        Type::S8 => Val::S8(as_i64(v) as i8),
        Type::U8 => Val::U8(as_i64(v) as u8),
        Type::S16 => Val::S16(as_i64(v) as i16),
        Type::U16 => Val::U16(as_i64(v) as u16),
        Type::S32 => Val::S32(as_i64(v) as i32),
        Type::U32 => Val::U32(as_i64(v) as u32),
        Type::S64 => Val::S64(as_i64(v)),
        Type::U64 => Val::U64(as_i64(v) as u64),
        Type::Float32 => Val::Float32(as_f64(v) as f32),
        Type::Float64 => Val::Float64(as_f64(v)),
        Type::Char => Val::Char(v.as_str().and_then(|s| s.chars().next()).unwrap_or('\0')),
        Type::String => Val::String(v.as_str().unwrap_or_default().to_string()),
        Type::Option(o) => {
            if v.is_null() {
                Val::Option(None)
            } else {
                Val::Option(Some(Box::new(json_to_val(&o.ty(), v)?)))
            }
        }
        Type::List(l) => {
            let inner = l.ty();
            let items = match v.as_array() {
                Some(a) => a
                    .iter()
                    .map(|e| json_to_val(&inner, e))
                    .collect::<Result<Vec<_>>>()?,
                None => Vec::new(),
            };
            Val::List(items)
        }
        Type::Record(r) => {
            let mut fields = Vec::new();
            for f in r.fields() {
                let camel = to_camel_case(f.name);
                let value = get_field(v, &camel)
                    .or_else(|| get_field(v, f.name))
                    .unwrap_or(JsonValue::Null);
                fields.push((f.name.to_string(), json_to_val(&f.ty, &value)?));
            }
            Val::Record(fields)
        }
        Type::Enum(_) => {
            // GraphQL enum values are emitted SCREAMING_SNAKE; WIT cases are kebab-case.
            let case = v.as_str().unwrap_or_default().to_lowercase().replace('_', "-");
            Val::Enum(case)
        }
        other => {
            return Err(anyhow!(
                "unsupported WIT parameter type for a wasm data source: {other:?}"
            ));
        }
    })
}

/// Decode a `result<string, string>` return value.
fn decode_result_string(v: Val) -> Result<std::result::Result<String, String>> {
    match v {
        Val::Result(Ok(payload)) => Ok(Ok(payload_string(payload.as_deref()))),
        Val::Result(Err(payload)) => Ok(Err(payload_string(payload.as_deref()))),
        // A bare string (result was degraded to its ok payload during SDL generation).
        Val::String(s) => Ok(Ok(s)),
        other => Err(anyhow!(
            "wasm export returned an unexpected value (expected result<string,string>): {other:?}"
        )),
    }
}

fn payload_string(p: Option<&Val>) -> String {
    match p {
        Some(Val::String(s)) => s.clone(),
        _ => String::new(),
    }
}

fn as_i64(v: &JsonValue) -> i64 {
    v.as_i64()
        .or_else(|| v.as_u64().map(|u| u as i64))
        .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
        .unwrap_or(0)
}

fn as_f64(v: &JsonValue) -> f64 {
    v.as_f64()
        .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
        .unwrap_or(0.0)
}

fn get_field(v: &JsonValue, key: &str) -> Option<JsonValue> {
    v.as_object().and_then(|m| m.get(key)).cloned()
}

/// Resolve a query-plan fetch by invoking the wasm component that backs the target subgraph.
///
/// Called from `FetchService::handle_fetch` when `service_name` maps to a wasm data source. Walks
/// the planned subgraph operation's root fields, invokes the WIT export backing each, shapes the
/// `result<string,string>` into the union response the generated SDL expects
/// (`<Field>Ok { value } | <Field>Err { error }`), and merges via `FetchNode::response_at_path` —
/// the same final step the connector path uses. Returns the fetch `(data, errors)`.
pub(crate) async fn fetch_with_wasm_service(
    schema: Arc<Schema>,
    component: Arc<WasmComponent>,
    fetch_node: FetchNode,
    variables: Variables,
    current_dir: Path,
    hoist_orphan_errors: bool,
) -> (JsonValue, Vec<Error>) {
    let inverted_paths = variables.inverted_paths.clone();

    let document = match fetch_node.operation.as_parsed() {
        Ok(doc) => doc.clone(),
        Err(e) => {
            let err = wasm_error(format!("invalid wasm subgraph operation: {e}"), &current_dir);
            return (JsonValue::Null, vec![err]);
        }
    };
    let operation = match document.operations.get(fetch_node.operation_name.as_deref()) {
        Ok(op) => op,
        Err(_) => {
            let err = wasm_error("wasm subgraph operation not found".to_string(), &current_dir);
            return (JsonValue::Null, vec![err]);
        }
    };

    let mut data = JsonMap::new();
    let mut errors: Vec<Error> = Vec::new();

    for selection in &operation.selection_set.selections {
        let apollo_compiler::executable::Selection::Field(field) = selection else {
            continue;
        };
        let field_name = field.name.as_str();
        let response_key = field.alias.as_ref().unwrap_or(&field.name).as_str();

        // `__typename` on the root is answered from the schema by execution — nothing to invoke.
        if field_name == "__typename" {
            continue;
        }

        let Some(mapping) = component.operation_map().get(field_name) else {
            errors.push(wasm_error(
                format!("field `{field_name}` has no matching export in the wasm component"),
                &current_dir,
            ));
            data.insert(ByteString::from(response_key), JsonValue::Null);
            continue;
        };

        let args = match field_arguments(field, &variables.variables) {
            Ok(args) => JsonValue::Object(args),
            Err(e) => {
                errors.push(wasm_error(
                    format!("building arguments for `{field_name}`: {e}"),
                    &current_dir,
                ));
                data.insert(ByteString::from(response_key), JsonValue::Null);
                continue;
            }
        };

        match component.invoke(mapping, &args).await {
            Ok(Ok(json_text)) => {
                data.insert(
                    ByteString::from(response_key),
                    result_member(field_name, "Ok", "value", json_text),
                );
            }
            Ok(Err(message)) => {
                data.insert(
                    ByteString::from(response_key),
                    result_member(field_name, "Err", "error", message),
                );
            }
            Err(host_err) => {
                errors.push(wasm_error(
                    format!("invoking `{field_name}` in the wasm component failed: {host_err}"),
                    &current_dir,
                ));
                data.insert(ByteString::from(response_key), JsonValue::Null);
            }
        }
    }

    let response = graphql::Response::builder()
        .data(JsonValue::Object(data))
        .errors(errors)
        .build();

    fetch_node.response_at_path(
        &schema,
        &current_dir,
        inverted_paths,
        response,
        hoist_orphan_errors,
    )
}

/// Build a `result<string,string>` union member matching the generated SDL:
/// `type <Field>Ok { value: String! }` / `type <Field>Err { error: String! }`.
fn result_member(field_name: &str, suffix: &str, payload_key: &str, payload: String) -> JsonValue {
    let mut member = JsonMap::new();
    member.insert(
        ByteString::from("__typename"),
        JsonValue::String(format!("{}{}", pascalize(field_name), suffix).into()),
    );
    member.insert(ByteString::from(payload_key), JsonValue::String(payload.into()));
    JsonValue::Object(member)
}

/// GraphQL result-wrapper type names are PascalCase; our field names are camelCase, so PascalCase is
/// just the field name with an upper-cased first character.
fn pascalize(camel: &str) -> String {
    let mut chars = camel.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

fn wasm_error(message: String, path: &Path) -> Error {
    graphql::Error::builder()
        .message(message)
        .extension_code("WASM_DATA_SOURCE_ERROR")
        .path(path.clone())
        .build()
}

/// Resolve a planned field's arguments into a JSON object keyed by GraphQL argument name, resolving
/// variable references against `variables`. Mirrors
/// `plugins::connectors::make_requests::graphql_utils::field_arguments_map`.
fn field_arguments(
    field: &apollo_compiler::Node<apollo_compiler::executable::Field>,
    variables: &JsonMap<ByteString, JsonValue>,
) -> Result<JsonMap<ByteString, JsonValue>, BoxError> {
    let mut arguments = JsonMap::new();
    for argument in field.arguments.iter() {
        arguments.insert(
            argument.name.as_str(),
            arg_value_to_json(&argument.value, variables)?,
        );
    }
    for argument_def in field.definition.arguments.iter() {
        if let Some(value) = argument_def.default_value.as_ref() {
            if !arguments.contains_key(argument_def.name.as_str()) {
                arguments.insert(argument_def.name.as_str(), arg_value_to_json(value, variables)?);
            }
        }
    }
    Ok(arguments)
}

fn arg_value_to_json(
    value: &apollo_compiler::ast::Value,
    variables: &JsonMap<ByteString, JsonValue>,
) -> Result<JsonValue, BoxError> {
    use apollo_compiler::ast::Value as V;
    Ok(match value {
        V::Null => JsonValue::Null,
        V::Enum(e) => JsonValue::String(e.as_str().into()),
        V::Variable(name) => variables.get(name.as_str()).cloned().unwrap_or(JsonValue::Null),
        V::String(s) => JsonValue::String(s.as_str().into()),
        V::Float(f) => JsonValue::Number(
            serde_json::Number::from_f64(f.try_to_f64().map_err(|_| "invalid float argument")?)
                .ok_or("invalid float argument")?,
        ),
        V::Int(i) => JsonValue::Number(serde_json::Number::from(
            i.try_to_i32().map_err(|_| "invalid int argument")?,
        )),
        V::Boolean(b) => JsonValue::Bool(*b),
        V::List(l) => JsonValue::Array(
            l.iter()
                .map(|v| arg_value_to_json(v, variables))
                .collect::<Result<Vec<_>, _>>()?,
        ),
        V::Object(o) => JsonValue::Object(
            o.iter()
                .map(|(k, v)| arg_value_to_json(v, variables).map(|v| (k.as_str().into(), v)))
                .collect::<Result<JsonMap<_, _>, _>>()?,
        ),
    })
}
