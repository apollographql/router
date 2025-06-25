use std::collections::HashMap;
use std::sync::Arc;

use apollo_compiler::collections::HashSet;
use apollo_compiler::collections::IndexMap;
use http::HeaderMap;
use http::HeaderValue;
use http::response::Parts;
use serde_json::Value as JsonValue;
use serde_json_bytes::ByteString;
use serde_json_bytes::Map;
use serde_json_bytes::Value;
use serde_json_bytes::json;

use crate::connectors::Namespace;

pub trait ContextReader {
    fn to_json_map(&self) -> Map<ByteString, Value>;
}

/// Convert a HeaderMap into a HashMap
pub(crate) fn externalize_header_map(
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
            serde_json::to_string(&self.args).unwrap_or_else(|_| "<invalid JSON>".to_string()),
            serde_json::to_string(&self.this).unwrap_or_else(|_| "<invalid JSON>".to_string()),
            serde_json::to_string(&self.batch).unwrap_or_else(|_| "<invalid JSON>".to_string()),
        )
    }
}

pub struct MappingContextMerger<'merger> {
    pub inputs: RequestInputs,
    pub variables_used: &'merger HashSet<Namespace>,
    pub config: Option<Value>,
    pub context: Option<Value>,
    pub status: Option<Value>,
    pub request: Option<Value>,
    pub response: Option<Value>,
    pub env: Option<Value>,
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

    pub fn context<'a>(mut self, context: impl ContextReader + 'a) -> Self {
        // $context could be a large object, so we only convert it to JSON
        // if it's used. It can also be mutated between requests, so we have
        // to convert it each time.
        if self.variables_used.contains(&Namespace::Context) {
            self.context = Some(Value::Object(context.to_json_map()));
        }
        self
    }

    pub fn config(mut self, config: Option<&Arc<HashMap<String, JsonValue>>>) -> Self {
        // $config doesn't change unless the schema reloads, but we can avoid
        // the allocation if it's unused.
        // We should always have a value for $config, even if it's an empty object, or we end up with "Variable $config not found" which is a confusing error for users
        if self.variables_used.contains(&Namespace::Config) {
            self.config = config.map(|c| json!(c)).or_else(|| Some(json!({})));
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
