use std::collections::HashMap;
use std::sync::Arc;

use crate::connectors::JSONSelection;
use crate::connectors::Namespace;
use apollo_compiler::Name;
use apollo_compiler::collections::HashSet;
use apollo_compiler::collections::IndexMap;
use apollo_compiler::executable::FieldSet;
use apollo_compiler::validation::Valid;
use http::HeaderMap;
use http::HeaderValue;
use http::response::Parts;
use serde_json::Value as JsonValue;
use serde_json_bytes::ByteString;
use serde_json_bytes::Map;
use serde_json_bytes::Value;
use serde_json_bytes::json;

fn externalize_header_map(
    input: &HeaderMap<HeaderValue>,
) -> Result<HashMap<String, Vec<String>>, String> {
    let mut output = HashMap::new();
    for (k, v) in input {
        let k = k.as_str().to_owned();
        let v = String::from_utf8(v.as_bytes().to_vec()).map_err(|e| e.to_string())?;
        output.entry(k).or_insert_with(Vec::new).push(v)
    }
    Ok(output)
}

#[derive(Clone, Default)]
pub struct RequestInputs {
    pub args: Map<ByteString, Value>,
    pub this: Map<ByteString, Value>,
    pub batch: Vec<Map<ByteString, Value>>,
}

impl RequestInputs {
    /// Creates a map for use in JSONSelection::apply_with_vars. It only clones
    /// values into the map if the variable namespaces (`$args`, `$this`, etc.)
    /// are actually referenced in the expressions for URLs, headers, body, or selection.
    pub fn merger(self, variables_used: &HashSet<Namespace>) -> MappingContextMerger {
        MappingContextMerger {
            inputs: self,
            variables_used,
            config: None,
            context: None,
            status: None,
            request: None,
            response: None,
            env: None,
        }
    }
}

impl std::fmt::Debug for RequestInputs {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "RequestInputs {{\n    args: {},\n    this: {},\n    batch: {}\n}}",
            serde_json::to_string(&self.args).unwrap_or("<invalid JSON>".to_string()),
            serde_json::to_string(&self.this).unwrap_or("<invalid JSON>".to_string()),
            serde_json::to_string(&self.batch).unwrap_or("<invalid JSON>".to_string()),
        )
    }
}

pub struct MappingContextMerger<'merger> {
    pub(super) inputs: RequestInputs,
    pub(super) variables_used: &'merger HashSet<Namespace>,
    pub(super) config: Option<Value>,
    pub(super) context: Option<Value>,
    pub(super) status: Option<Value>,
    pub(super) request: Option<Value>,
    pub(super) response: Option<Value>,
    pub(super) env: Option<Value>,
}

impl MappingContextMerger<'_> {
    pub fn merge(self) -> IndexMap<String, Value> {
        let mut map =
            IndexMap::with_capacity_and_hasher(self.variables_used.len(), Default::default());
        // Not all connectors reference $args
        if self.variables_used.contains(&Namespace::Args) {
            map.insert(
                Namespace::Args.as_str().into(),
                Value::Object(self.inputs.args),
            );
        }

        // $this only applies to fields on entity types (not Query or Mutation)
        if self.variables_used.contains(&Namespace::This) {
            map.insert(
                Namespace::This.as_str().into(),
                Value::Object(self.inputs.this),
            );
        }

        // $batch only applies to entity resolvers on types
        if self.variables_used.contains(&Namespace::Batch) {
            map.insert(
                Namespace::Batch.as_str().into(),
                Value::Array(self.inputs.batch.into_iter().map(Value::Object).collect()),
            );
        }

        if let Some(config) = self.config.iter().next() {
            map.insert(Namespace::Config.as_str().into(), config.to_owned());
        }

        if let Some(context) = self.context.iter().next() {
            map.insert(Namespace::Context.as_str().into(), context.to_owned());
        }

        if let Some(status) = self.status.iter().next() {
            map.insert(Namespace::Status.as_str().into(), status.to_owned());
        }

        if let Some(request) = self.request.iter().next() {
            map.insert(Namespace::Request.as_str().into(), request.to_owned());
        }

        if let Some(response) = self.response.iter().next() {
            map.insert(Namespace::Response.as_str().into(), response.to_owned());
        }

        if let Some(env) = self.env.iter().next() {
            map.insert(Namespace::Env.as_str().into(), env.to_owned());
        }
        map
    }

    pub fn context(mut self, context: Map<ByteString, Value>) -> Self {
        // $context could be a large object, so we only convert it to JSON
        // if it's used. It can also be mutated between requests, so we have
        // to convert it each time.
        if self.variables_used.contains(&Namespace::Context) {
            self.context = Some(Value::Object(context));
        }
        self
    }

    pub fn config(mut self, config: Option<&Arc<HashMap<String, JsonValue>>>) -> Self {
        // $config doesn't change unless the schema reloads, but we can avoid
        // the allocation if it's unused.
        if let (true, Some(config)) = (self.variables_used.contains(&Namespace::Config), config) {
            self.config = Some(json!(config));
        }
        self
    }

    pub fn status(mut self, status: u16) -> Self {
        // $status is available only for response mapping
        if self.variables_used.contains(&Namespace::Status) {
            self.status = Some(Value::Number(status.into()));
        }
        self
    }

    pub fn request(
        mut self,
        headers_used: &HashSet<String>,
        headers: &HeaderMap<HeaderValue>,
    ) -> Self {
        // Add headers from the original router request.
        // Only include headers that are actually referenced to save on passing around unused headers in memory.
        if self.variables_used.contains(&Namespace::Request) {
            let new_headers = externalize_header_map(headers)
                .unwrap_or_default()
                .iter()
                .filter_map(|(key, value)| {
                    headers_used.contains(key.as_str()).then_some((
                        key.as_str().into(),
                        value
                            .iter()
                            .map(|s| Value::String(s.as_str().into()))
                            .collect(),
                    ))
                })
                .collect();
            let request_object = json!({
                "headers": Value::Object(new_headers)
            });
            self.request = Some(request_object);
        }
        self
    }

    pub fn response(
        mut self,
        headers_used: &HashSet<String>,
        response_parts: Option<&Parts>,
    ) -> Self {
        // Add headers from the connectors response
        // Only include headers that are actually referenced to save on passing around unused headers in memory.
        if let (true, Some(response_parts)) = (
            self.variables_used.contains(&Namespace::Response),
            response_parts,
        ) {
            let new_headers: Map<ByteString, Value> =
                externalize_header_map(&response_parts.headers)
                    .unwrap_or_default()
                    .iter()
                    .filter_map(|(key, value)| {
                        headers_used.contains(key.as_str()).then_some((
                            key.as_str().into(),
                            value
                                .iter()
                                .map(|s| Value::String(s.as_str().into()))
                                .collect(),
                        ))
                    })
                    .collect();
            let response_object = json!({
                "headers": Value::Object(new_headers)
            });
            self.response = Some(response_object);
        }
        self
    }

    pub fn env(mut self, env_vars_used: &HashSet<String>) -> Self {
        if self.variables_used.contains(&Namespace::Env) {
            let env_vars: Map<ByteString, Value> = env_vars_used
                .iter()
                .flat_map(|key| {
                    std::env::var(key)
                        .ok()
                        .map(|value| (key.as_str().into(), Value::String(value.into())))
                })
                .collect();
            self.env = Some(Value::Object(env_vars));
        }
        self
    }
}

#[derive(Clone)]
pub enum ResponseKey {
    RootField {
        name: String,
        selection: Arc<JSONSelection>,
        inputs: RequestInputs,
    },
    Entity {
        index: usize,
        selection: Arc<JSONSelection>,
        inputs: RequestInputs,
    },
    EntityField {
        index: usize,
        field_name: String,
        /// Is Some only if the output type is a concrete object type. If it's
        /// an interface, it's treated as an interface object and we can't emit
        /// a __typename in the response.
        typename: Option<Name>,
        selection: Arc<JSONSelection>,
        inputs: RequestInputs,
    },
    BatchEntity {
        selection: Arc<JSONSelection>,
        keys: Valid<FieldSet>,
        inputs: RequestInputs,
    },
}

impl ResponseKey {
    pub fn selection(&self) -> &JSONSelection {
        match self {
            ResponseKey::RootField { selection, .. } => selection,
            ResponseKey::Entity { selection, .. } => selection,
            ResponseKey::EntityField { selection, .. } => selection,
            ResponseKey::BatchEntity { selection, .. } => selection,
        }
    }

    pub fn inputs(&self) -> &RequestInputs {
        match self {
            ResponseKey::RootField { inputs, .. } => inputs,
            ResponseKey::Entity { inputs, .. } => inputs,
            ResponseKey::EntityField { inputs, .. } => inputs,
            ResponseKey::BatchEntity { inputs, .. } => inputs,
        }
    }
}

impl std::fmt::Debug for ResponseKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::RootField {
                name,
                selection,
                inputs,
            } => f
                .debug_struct("RootField")
                .field("name", name)
                .field("selection", &selection.to_string())
                .field("inputs", inputs)
                .finish(),
            Self::Entity {
                index,
                selection,
                inputs,
            } => f
                .debug_struct("Entity")
                .field("index", index)
                .field("selection", &selection.to_string())
                .field("inputs", inputs)
                .finish(),
            Self::EntityField {
                index,
                field_name,
                typename,
                selection,
                inputs,
            } => f
                .debug_struct("EntityField")
                .field("index", index)
                .field("field_name", field_name)
                .field("typename", typename)
                .field("selection", &selection.to_string())
                .field("inputs", inputs)
                .finish(),
            Self::BatchEntity {
                selection,
                keys,
                inputs,
            } => f
                .debug_struct("BatchEntity")
                .field("selection", &selection.to_string())
                .field("key_selection", &keys.serialize().no_indent().to_string())
                .field("inputs", inputs)
                .finish(),
        }
    }
}
