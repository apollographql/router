use std::collections::HashMap;
use std::sync::Arc;

use apollo_compiler::collections::HashSet;
use apollo_compiler::collections::IndexMap;
use apollo_federation::sources::connect::Namespace;
use http::response::Parts;
use serde_json::Value as JsonValue;
use serde_json_bytes::ByteString;
use serde_json_bytes::Map;
use serde_json_bytes::Value;
use serde_json_bytes::json;

use crate::Context;
use crate::plugins::connectors::make_requests::RequestInputs;
use crate::services::external::externalize_header_map;

pub(crate) struct MappingContextMerger<'merger> {
    pub(super) inputs: RequestInputs,
    pub(super) variables_used: &'merger HashSet<Namespace>,
    pub(super) config: Option<Value>,
    pub(super) context: Option<Value>,
    pub(super) status: Option<Value>,
    pub(super) request: Option<Value>,
    pub(super) response: Option<Value>,
}

impl MappingContextMerger<'_> {
    pub(crate) fn merge(self) -> IndexMap<String, Value> {
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

        map
    }

    pub(crate) fn context(mut self, context: &Context) -> Self {
        // $context could be a large object, so we only convert it to JSON
        // if it's used. It can also be mutated between requests, so we have
        // to convert it each time.
        if self.variables_used.contains(&Namespace::Context) {
            let context: Map<ByteString, Value> = context
                .iter()
                .map(|r| (r.key().as_str().into(), r.value().clone()))
                .collect();
            self.context = Some(Value::Object(context));
        }
        self
    }

    pub(crate) fn config(mut self, config: Option<&Arc<HashMap<String, JsonValue>>>) -> Self {
        // $config doesn't change unless the schema reloads, but we can avoid
        // the allocation if it's unused.
        if let (true, Some(config)) = (self.variables_used.contains(&Namespace::Config), config) {
            self.config = Some(json!(config));
        }
        self
    }

    pub(crate) fn status(mut self, status: u16) -> Self {
        // $status is available only for response mapping
        if self.variables_used.contains(&Namespace::Status) {
            self.status = Some(Value::Number(status.into()));
        }
        self
    }

    pub(crate) fn request(
        mut self,
        headers_used: &HashSet<String>,
        supergraph_request: &Arc<http::Request<crate::graphql::Request>>,
    ) -> Self {
        // Add headers from the original router request.
        // Only include headers that are actually referenced to save on passing around unused headers in memory.
        if self.variables_used.contains(&Namespace::Request) {
            let new_headers = externalize_header_map(supergraph_request.headers())
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

    pub(crate) fn response(
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
}
