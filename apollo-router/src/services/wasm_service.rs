//! Runtime for hosting WebAssembly **components** as GraphQL data sources.
//!
//! A component (built with the toolchain in `router-wasm-plugins`/`wit-gql`) exports WIT functions
//! that each map to a GraphQL field. This module owns the embedded wasmtime host: it compiles a
//! component once, and per fetch it instantiates the component, invokes the WIT export that backs
//! the requested GraphQL field, and converts the returned WIT value into the JSON shape promised
//! by the SDL that `wit-gql` generated from the same WIT (records → objects with camelCase keys,
//! enums → SCREAMING_SNAKE strings, a top-level `result<ok, err>` → the `<Field>Ok | <Field>Err`
//! union, …).
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
use wit_gql::naming::{to_camel_case, to_pascal_case, to_screaming_snake};
use wit_gql::wit_parser::{self, Resolve, TypeDefKind};
use wit_gql::{
    ExportLocation, FieldMapping, OperationMap, SHARED_ERROR_TYPE, is_string_error_arm,
    qualified_type_name, shared_error_type_available,
};

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
    /// The component's decoded WIT — needed at invoke time to convert returned values into the
    /// JSON shapes the generated SDL promises (named types, variant `__typename`s, flag sets).
    resolve: Resolve,
    /// Whether string-error results use the shared `Error` union member (the SDL emits it unless
    /// a WIT type claims that name) — must match the SDL generated from the same WIT.
    shared_error: bool,
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
        let shared_error = shared_error_type_available(&resolve);
        Ok(Self {
            runtime,
            component,
            config,
            operation_map,
            resolve,
            shared_error,
        })
    }

    /// The GraphQL-field → WIT-export mapping (used by the fetch path to route root fields).
    pub(crate) fn operation_map(&self) -> &OperationMap {
        &self.operation_map
    }

    /// Invoke the WIT export backing `field`, passing `args` (a JSON object keyed by GraphQL
    /// argument name). Returns the GraphQL value for the field, converted from the export's
    /// returned WIT value into the shape the generated SDL promises (see [`decode_return`]) —
    /// including the `<Field>Ok | <Field>Err` union wrapping when the export returns a
    /// two-armed `result`. The outer `Result` is a host/runtime failure (instantiation, missing
    /// export, trap, or a value the SDL has no representation for).
    pub(crate) async fn invoke(&self, field: &FieldMapping, args: &JsonValue) -> Result<JsonValue> {
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

        // A WIT function has at most one result; size the slot list to its declared arity.
        let mut results = vec![Val::Bool(false); usize::from(field.result.is_some())];
        func.call_async(&mut store, &call_args, &mut results)
            .await
            .map_err(|e| anyhow!("invoking `{}`: {e}", field.graphql_field))?;
        // NB: `post_return_async` is a deprecated no-op in wasmtime 46; not called.

        decode_return(&self.resolve, field, results.into_iter().next(), self.shared_error)
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

/// Convert an export's returned `Val` into the GraphQL value its generated SDL promises for the
/// field. Mirrors `wit_gql::schema::format_return_type`:
/// - no declared result (`func() -> ()`) → SDL `Boolean!` → `true` (the call completed);
/// - top-level `result<ok, err>` with both arms → the `<Field>Ok { value } | <Field>Err { error }`
///   union, tagged with `__typename` so response formatting can match inline fragments; when the
///   err arm is a plain string (and `shared_error`), the member is the shared `Error` type the
///   SDL emits instead of a per-field `<Field>Err`;
/// - top-level `result<ok>` (no err arm) → SDL is the bare ok type → the converted ok payload,
///   or `null` for the (unrepresentable) err case;
/// - top-level `result<_, err>` (no ok payload) → SDL `Boolean!` → whether the call succeeded;
/// - anything else → converted structurally by [`val_to_json`].
fn decode_return(
    resolve: &Resolve,
    field: &FieldMapping,
    val: Option<Val>,
    shared_error: bool,
) -> Result<JsonValue> {
    let Some(result_ty) = field.result else {
        return Ok(JsonValue::Bool(true));
    };
    let val = val.ok_or_else(|| {
        anyhow!("export `{}` returned no value despite declaring one", field.graphql_field)
    })?;

    if let wit_parser::Type::Id(id) = result_ty {
        if let TypeDefKind::Result(r) = &resolve.types[id].kind {
            let Val::Result(payload) = val else {
                return Err(anyhow!(
                    "export `{}` declared result<..> but returned {val:?}",
                    field.graphql_field
                ));
            };
            return match (r.ok, r.err) {
                (Some(ok_ty), Some(err_ty)) => {
                    let pascal = pascalize(&field.graphql_field);
                    match payload {
                        Ok(p) => Ok(union_member(
                            format!("{pascal}Ok"),
                            "value",
                            result_payload_json(resolve, &ok_ty, p.as_deref(), field)?,
                        )),
                        Err(p) => {
                            let type_name = if shared_error && is_string_error_arm(resolve, &err_ty)
                            {
                                SHARED_ERROR_TYPE.to_string()
                            } else {
                                format!("{pascal}Err")
                            };
                            Ok(union_member(
                                type_name,
                                "error",
                                result_payload_json(resolve, &err_ty, p.as_deref(), field)?,
                            ))
                        }
                    }
                }
                (Some(ok_ty), None) => match payload {
                    Ok(p) => result_payload_json(resolve, &ok_ty, p.as_deref(), field),
                    Err(_) => Ok(JsonValue::Null),
                },
                (None, _) => Ok(JsonValue::Bool(payload.is_ok())),
            };
        }
    }
    val_to_json(resolve, &result_ty, &val)
}

fn result_payload_json(
    resolve: &Resolve,
    ty: &wit_parser::Type,
    payload: Option<&Val>,
    field: &FieldMapping,
) -> Result<JsonValue> {
    let payload = payload.ok_or_else(|| {
        anyhow!("export `{}` returned a result arm without its declared payload", field.graphql_field)
    })?;
    val_to_json(resolve, ty, payload)
}

/// Build one member of the generated `<Field>Ok | <Field>Err` union, tagged with its
/// `__typename` so the router's response formatting can match the client's inline fragments.
fn union_member(type_name: String, payload_key: &'static str, payload: JsonValue) -> JsonValue {
    let mut member = JsonMap::new();
    member.insert(ByteString::from("__typename"), JsonValue::String(type_name.into()));
    member.insert(ByteString::from(payload_key), payload);
    JsonValue::Object(member)
}

/// Convert a WIT value into JSON matching how `wit_gql::schema` rendered its type into the SDL:
/// records → objects keyed by camelCase field name, enums → SCREAMING_SNAKE strings, variants →
/// `__typename`-tagged union members, flags → an all-flags boolean object, 64-bit ints → strings
/// (the SDL maps them to `String!`), options/lists/aliases structurally.
fn val_to_json(resolve: &Resolve, ty: &wit_parser::Type, val: &Val) -> Result<JsonValue> {
    if let wit_parser::Type::Id(id) = ty {
        let def = &resolve.types[*id];
        return match (&def.kind, val) {
            (TypeDefKind::Type(inner), _) => val_to_json(resolve, inner, val),
            (TypeDefKind::Option(_), Val::Option(None)) => Ok(JsonValue::Null),
            (TypeDefKind::Option(inner), Val::Option(Some(v))) => val_to_json(resolve, inner, v),
            (TypeDefKind::List(inner), Val::List(items)) => Ok(JsonValue::Array(
                items
                    .iter()
                    .map(|v| val_to_json(resolve, inner, v))
                    .collect::<Result<Vec<_>>>()?,
            )),
            (TypeDefKind::Record(r), Val::Record(entries)) => {
                let mut obj = JsonMap::new();
                for f in &r.fields {
                    let value = match entries.iter().find(|(name, _)| name == &f.name) {
                        Some((_, v)) => val_to_json(resolve, &f.ty, v)?,
                        None => JsonValue::Null,
                    };
                    obj.insert(ByteString::from(to_camel_case(&f.name)), value);
                }
                Ok(JsonValue::Object(obj))
            }
            (TypeDefKind::Enum(_), Val::Enum(case)) => {
                Ok(JsonValue::String(to_screaming_snake(case).into()))
            }
            (TypeDefKind::Variant(v), Val::Variant(case, payload)) => {
                let case_def = v.cases.iter().find(|c| &c.name == case).ok_or_else(|| {
                    anyhow!("variant value has unknown case `{case}`")
                })?;
                let type_name = format!("{}{}", qualified_type_name(resolve, *id), to_pascal_case(case));
                match (case_def.ty, payload) {
                    // SDL: `type <Variant><Case> { value: <payload type> }`
                    (Some(payload_ty), Some(p)) => Ok(union_member(
                        type_name,
                        "value",
                        val_to_json(resolve, &payload_ty, p)?,
                    )),
                    // SDL: payload-less cases render as `type <Variant><Case> { _tag: Boolean }`
                    (None, _) => Ok(union_member(type_name, "_tag", JsonValue::Bool(true))),
                    (Some(_), None) => Err(anyhow!(
                        "variant case `{case}` is missing its declared payload"
                    )),
                }
            }
            (TypeDefKind::Flags(f), Val::Flags(set)) => {
                // SDL renders every flag as `Boolean!`, so emit the full set, not just what's on.
                let mut obj = JsonMap::new();
                for flag in &f.flags {
                    obj.insert(
                        ByteString::from(to_camel_case(&flag.name)),
                        JsonValue::Bool(set.iter().any(|s| s == &flag.name)),
                    );
                }
                Ok(JsonValue::Object(obj))
            }
            // Anonymous (nested) result — the SDL degraded it to its ok payload (or Boolean!).
            (TypeDefKind::Result(r), Val::Result(payload)) => match (r.ok, payload) {
                (Some(ok_ty), Ok(Some(p))) => val_to_json(resolve, &ok_ty, p),
                (Some(_), _) => Ok(JsonValue::Null),
                (None, p) => Ok(JsonValue::Bool(p.is_ok())),
            },
            (TypeDefKind::Tuple(t), Val::Tuple(items)) => {
                let converted = t
                    .types
                    .iter()
                    .zip(items)
                    .map(|(ty, v)| val_to_json(resolve, ty, v))
                    .collect::<Result<Vec<_>>>()?;
                let uniform = t.types.windows(2).all(|w| w[0] == w[1]);
                if uniform {
                    // SDL renders uniform tuples as a list of the element type.
                    Ok(JsonValue::Array(converted))
                } else {
                    // Non-uniform tuples render as `String!` in the SDL — serialize to match.
                    Ok(JsonValue::String(
                        serde_json::to_string(&JsonValue::Array(converted))?.into(),
                    ))
                }
            }
            (kind, val) => Err(anyhow!(
                "cannot convert wasm value {val:?} (WIT kind {kind:?}) into a GraphQL value"
            )),
        };
    }

    Ok(match val {
        Val::Bool(b) => JsonValue::Bool(*b),
        Val::S8(v) => JsonValue::Number((*v).into()),
        Val::U8(v) => JsonValue::Number((*v).into()),
        Val::S16(v) => JsonValue::Number((*v).into()),
        Val::U16(v) => JsonValue::Number((*v).into()),
        Val::S32(v) => JsonValue::Number((*v).into()),
        Val::U32(v) => JsonValue::Number((*v).into()),
        // The SDL maps 64-bit ints to `String!` (they overflow GraphQL's 32-bit `Int`).
        Val::S64(v) => JsonValue::String(v.to_string().into()),
        Val::U64(v) => JsonValue::String(v.to_string().into()),
        Val::Float32(v) => serde_json::Number::from_f64(f64::from(*v))
            .map(JsonValue::Number)
            .unwrap_or(JsonValue::Null),
        Val::Float64(v) => serde_json::Number::from_f64(*v)
            .map(JsonValue::Number)
            .unwrap_or(JsonValue::Null),
        Val::Char(c) => JsonValue::String(c.to_string().into()),
        Val::String(s) => JsonValue::String(s.as_str().into()),
        other => {
            return Err(anyhow!(
                "cannot convert wasm value {other:?} into a GraphQL scalar"
            ));
        }
    })
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
/// the planned subgraph operation's root fields, invokes the WIT export backing each, converts the
/// returned WIT value into the JSON shape the generated SDL expects (see [`decode_return`]), and
/// merges via `FetchNode::response_at_path` — the same final step the connector path uses. Returns
/// the fetch `(data, errors)`.
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
            Ok(value) => {
                data.insert(ByteString::from(response_key), value);
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

#[cfg(test)]
mod tests {
    use serde_json_bytes::json;
    use wit_gql::operation_map;

    use super::*;

    /// A WIT surface exercising the shapes wit-gql's SDL generation maps specially: typed records
    /// behind `result<record, string>` (the autostamp component shape), raw `result<string,
    /// string>` (the legacy shape), typed variant errors, enums, s64, options, and lists.
    const WIT: &str = r#"
        package test:api;

        interface me {
          enum plan { free, paid-annual }

          variant fail { forbidden(string), gone }

          record user {
            admin: option<bool>,
            login: string,
            sign-in-count: s64,
            plan: plan,
            emails: list<string>,
          }

          get-me: func() -> result<user, string>;
          get-raw: func() -> result<string, string>;
          get-guarded: func() -> result<user, fail>;
        }

        world api {
          export me;
        }
    "#;

    fn resolve_and_map() -> (Resolve, OperationMap) {
        let mut resolve = Resolve::default();
        let pkg = resolve.push_str("test.wit", WIT).expect("parse WIT");
        let world = resolve.select_world(&[pkg], None).expect("select world");
        let map = operation_map(&resolve, world);
        (resolve, map)
    }

    fn user_val() -> Val {
        Val::Record(vec![
            ("admin".into(), Val::Option(Some(Box::new(Val::Bool(true))))),
            ("login".into(), Val::String("lrlna".into())),
            ("sign-in-count".into(), Val::S64(9_000_000_000)),
            ("plan".into(), Val::Enum("paid-annual".into())),
            (
                "emails".into(),
                Val::List(vec![Val::String("a@b.c".into())]),
            ),
        ])
    }

    #[test]
    fn typed_record_ok_becomes_structured_union_member() {
        let (resolve, map) = resolve_and_map();
        let field = map.get("meGetMe").expect("meGetMe mapping");

        let val = Val::Result(Ok(Some(Box::new(user_val()))));
        let json = decode_return(&resolve, field, Some(val), true).expect("decode");

        assert_eq!(
            json,
            json!({
                "__typename": "MeGetMeOk",
                "value": {
                    "admin": true,
                    "login": "lrlna",
                    // s64 renders as String! in the SDL (overflows GraphQL Int).
                    "signInCount": "9000000000",
                    "plan": "PAID_ANNUAL",
                    "emails": ["a@b.c"],
                },
            })
        );
    }

    #[test]
    fn string_err_becomes_shared_error_member() {
        let (resolve, map) = resolve_and_map();
        let field = map.get("meGetMe").expect("meGetMe mapping");

        let val = Val::Result(Err(Some(Box::new(Val::String("401 unauthorized".into())))));
        let json = decode_return(&resolve, field, Some(val), true).expect("decode");

        // Plain-string err arms use the one shared `Error` type the SDL emits, not a per-op
        // `MeGetMeErr` wrapper.
        assert_eq!(json, json!({ "__typename": "Error", "error": "401 unauthorized" }));

        // Without the shared type (a WIT type claimed the name), fall back to the per-op wrapper.
        let val = Val::Result(Err(Some(Box::new(Val::String("401 unauthorized".into())))));
        let json = decode_return(&resolve, field, Some(val), false).expect("decode");
        assert_eq!(
            json,
            json!({ "__typename": "MeGetMeErr", "error": "401 unauthorized" })
        );
    }

    #[test]
    fn raw_string_payload_stays_a_string() {
        let (resolve, map) = resolve_and_map();
        let field = map.get("meGetRaw").expect("meGetRaw mapping");

        let body = r#"{"admin":true}"#;
        let val = Val::Result(Ok(Some(Box::new(Val::String(body.into())))));
        let json = decode_return(&resolve, field, Some(val), true).expect("decode");

        assert_eq!(json, json!({ "__typename": "MeGetRawOk", "value": body }));
    }

    #[test]
    fn variant_err_is_tagged_with_union_member_typename() {
        let (resolve, map) = resolve_and_map();
        let field = map.get("meGetGuarded").expect("meGetGuarded mapping");

        let forbidden = Val::Variant("forbidden".into(), Some(Box::new(Val::String("no".into()))));
        let json = decode_return(&resolve, field, Some(Val::Result(Err(Some(Box::new(forbidden))))), true)
            .expect("decode");
        assert_eq!(
            json,
            json!({
                "__typename": "MeGetGuardedErr",
                "error": { "__typename": "MeFailForbidden", "value": "no" },
            })
        );

        let gone = Val::Variant("gone".into(), None);
        let json = decode_return(&resolve, field, Some(Val::Result(Err(Some(Box::new(gone))))), true)
            .expect("decode");
        assert_eq!(
            json,
            json!({
                "__typename": "MeGetGuardedErr",
                "error": { "__typename": "MeFailGone", "_tag": true },
            })
        );
    }

    #[test]
    fn missing_record_field_and_none_option_are_null() {
        let (resolve, map) = resolve_and_map();
        let field = map.get("meGetMe").expect("meGetMe mapping");

        let user = Val::Record(vec![
            ("admin".into(), Val::Option(None)),
            ("login".into(), Val::String("lrlna".into())),
            ("sign-in-count".into(), Val::S64(1)),
            ("plan".into(), Val::Enum("free".into())),
            // `emails` omitted entirely.
        ]);
        let val = Val::Result(Ok(Some(Box::new(user))));
        let json = decode_return(&resolve, field, Some(val), true).expect("decode");

        assert_eq!(
            json,
            json!({
                "__typename": "MeGetMeOk",
                "value": {
                    "admin": null,
                    "login": "lrlna",
                    "signInCount": "1",
                    "plan": "FREE",
                    "emails": null,
                },
            })
        );
    }

    #[test]
    fn non_string_payload_is_no_longer_silently_dropped() {
        let (resolve, map) = resolve_and_map();
        let field = map.get("meGetRaw").expect("meGetRaw mapping");

        // A record where the declared ok type is `string` — a component/SDL mismatch must
        // surface as an error, not degrade to an empty string.
        let val = Val::Result(Ok(Some(Box::new(user_val()))));
        let err = decode_return(&resolve, field, Some(val), true).expect_err("must not decode");
        assert!(
            err.to_string().contains("cannot convert"),
            "unexpected error: {err}"
        );
    }
}
