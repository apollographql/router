//! plugins implementing router customizations.
//!
//! These plugins are compiled into the router and configured via YAML configuration.

use std::collections::HashMap;

use access_json::JSONQuery;
use futures::future::ready;
use futures::stream::once;
use futures::stream::BoxStream;
use futures::StreamExt;
use http::header::HeaderName;
use http::HeaderMap;
use regex::Regex;
use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;
use serde_json::Value;

use crate::graphql::Request;
use crate::graphql::Response;
use crate::plugin::serde::deserialize_header_name;
use crate::plugin::serde::deserialize_json_query;
use crate::plugin::serde::deserialize_regex;
use crate::services::RouterResponse;
use crate::Context;

pub mod csrf;
mod forbid_mutations;
mod headers;
mod include_subgraph_errors;
pub mod override_url;
pub mod rhai;
pub mod telemetry;
pub(crate) mod traffic_shaping;

// TODO
// Maybe a best implementation would be to provide method like get_static_names to fetch a static array of names.
// Use it in our code to declare fieldSet
// And then use the normal logics to compute different attributes
/// -------------------

#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
/// Configuration to add custom attributes/labels on metrics
pub struct CommonLocationConf {
    /// Configuration to forward header values or body values from router request/response in metric attributes/labels
    pub(crate) router: Option<LocationForwardConf>,
    /// Configuration to forward header values or body values from subgraph request/response in metric attributes/labels
    pub(crate) subgraph: Option<SubgraphConf>,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct SubgraphConf {
    // Apply to all subgraphs
    pub(crate) all: Option<LocationForwardConf>,
    // Apply to specific subgraph
    pub(crate) subgraphs: Option<HashMap<String, LocationForwardConf>>,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct LocationForwardConf {
    /// Configuration to insert custom attributes/labels in metrics
    #[serde(rename = "static")]
    pub(crate) insert: Option<Vec<Insert>>,
    /// Configuration to forward headers or body values from the request custom attributes/labels in metrics
    pub(crate) request: Option<Forward>,
    /// Configuration to forward headers or body values from the response custom attributes/labels in metrics
    pub(crate) response: Option<Forward>,
    /// Configuration to forward values from the context custom attributes/labels in metrics
    pub(crate) context: Option<Vec<ContextForward>>,
}

#[derive(Clone, JsonSchema, Deserialize, Debug)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
/// Configuration to insert custom attributes/labels in metrics
pub(crate) struct Insert {
    pub(crate) name: String,
    pub(crate) value: String,
}

#[derive(Debug, Clone, Deserialize, JsonSchema, Default)]
#[serde(deny_unknown_fields)]
pub(crate) struct Forward {
    /// Forward header values as custom attributes/labels in metrics
    pub(crate) header: Option<Vec<HeaderForward>>,
    /// Forward body values as custom attributes/labels in metrics
    pub(crate) body: Option<Vec<BodyForward>>,
}

#[derive(Clone, JsonSchema, Deserialize, Debug)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
#[serde(untagged)]
/// Configuration to forward header values in metric labels
pub(crate) enum HeaderForward {
    /// Using a named header
    Named {
        #[schemars(schema_with = "string_schema")]
        #[serde(deserialize_with = "deserialize_header_name")]
        named: HeaderName,
        rename: Option<String>,
        default: Option<String>,
    },
    /// Using a regex on the header name
    Matching {
        #[schemars(schema_with = "string_schema")]
        #[serde(deserialize_with = "deserialize_regex")]
        matching: Regex,
    },
}

#[derive(Clone, JsonSchema, Deserialize, Debug)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
/// Configuration to forward body values in metric attributes/labels
pub(crate) struct BodyForward {
    #[schemars(schema_with = "string_schema")]
    #[serde(deserialize_with = "deserialize_json_query")]
    pub(crate) path: JSONQuery,
    pub(crate) name: String,
    pub(crate) default: Option<String>,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
/// Configuration to forward context values in metric attributes/labels
pub(crate) struct ContextForward {
    pub(crate) named: String,
    pub(crate) rename: Option<String>,
    pub(crate) default: Option<String>,
}

impl HeaderForward {
    pub(crate) fn get_from_headers(&self, headers: &HeaderMap) -> HashMap<String, String> {
        let mut attributes = HashMap::new();
        match self {
            HeaderForward::Named {
                named,
                rename,
                default,
            } => {
                let value = headers.get(named);
                if let Some(value) = value
                    .and_then(|v| v.to_str().ok()?.to_string().into())
                    .or_else(|| default.clone())
                {
                    attributes.insert(rename.clone().unwrap_or_else(|| named.to_string()), value);
                }
            }
            HeaderForward::Matching { matching } => {
                headers
                    .iter()
                    .filter(|(name, _)| matching.is_match(name.as_str()))
                    .for_each(|(name, value)| {
                        if let Ok(value) = value.to_str() {
                            attributes.insert(name.to_string(), value.to_string());
                        }
                    });
            }
        }

        attributes
    }
}

impl Forward {
    pub(crate) fn merge(&mut self, to_merge: Self) {
        match (&mut self.body, to_merge.body) {
            (Some(body), Some(body_to_merge)) => {
                body.extend(body_to_merge);
            }
            (None, Some(body_to_merge)) => {
                self.body = Some(body_to_merge);
            }
            _ => {}
        }
        match (&mut self.header, to_merge.header) {
            (Some(header), Some(header_to_merge)) => {
                header.extend(header_to_merge);
            }
            (None, Some(header_to_merge)) => {
                self.header = Some(header_to_merge);
            }
            _ => {}
        }
    }
}

impl LocationForwardConf {
    pub(crate) async fn get_from_router_response(
        &self,
        response: RouterResponse,
    ) -> (RouterResponse, HashMap<String, String>) {
        let mut attributes = HashMap::new();

        // Fill from static
        if let Some(to_insert) = &self.insert {
            for Insert { name, value } in to_insert {
                attributes.insert(name.clone(), value.clone());
            }
        }
        let context = response.context;
        // Fill from context
        if let Some(from_context) = &self.context {
            for ContextForward {
                named,
                default,
                rename,
            } in from_context
            {
                match context.get::<_, String>(named) {
                    Ok(Some(value)) => {
                        attributes.insert(rename.as_ref().unwrap_or(named).clone(), value);
                    }
                    _ => {
                        if let Some(default_val) = default {
                            attributes.insert(
                                rename.as_ref().unwrap_or(named).clone(),
                                default_val.clone(),
                            );
                        }
                    }
                };
            }
        }
        let (parts, stream) = http::Response::from(response.response).into_parts();
        let (first, rest) = stream.into_future().await;
        // Fill from response
        if let Some(from_response) = &self.response {
            if let Some(header_forward) = &from_response.header {
                attributes.extend(header_forward.iter().fold(
                    HashMap::new(),
                    |mut acc, current| {
                        acc.extend(current.get_from_headers(&parts.headers));
                        acc
                    },
                ));
            }

            if let Some(body_forward) = &from_response.body {
                if let Some(body) = &first {
                    for body_fw in body_forward {
                        let output = body_fw.path.execute(body).unwrap();
                        if let Some(val) = output {
                            if let Value::String(val_str) = val {
                                attributes.insert(body_fw.name.clone(), val_str);
                            } else {
                                attributes.insert(body_fw.name.clone(), val.to_string());
                            }
                        } else if let Some(default_val) = &body_fw.default {
                            attributes.insert(body_fw.name.clone(), default_val.clone());
                        }
                    }
                }
            }
        }

        let response = http::Response::from_parts(
            parts,
            once(ready(first.unwrap_or_default())).chain(rest).boxed(),
        )
        .into();

        (RouterResponse { context, response }, attributes)
    }

    /// Get attributes from context
    pub(crate) fn get_from_context(&self, context: &Context) -> HashMap<String, String> {
        let mut attributes = HashMap::new();

        if let Some(from_context) = &self.context {
            for ContextForward {
                named,
                default,
                rename,
            } in from_context
            {
                match context.get::<_, String>(named) {
                    Ok(Some(value)) => {
                        attributes.insert(rename.as_ref().unwrap_or(named).clone(), value);
                    }
                    _ => {
                        if let Some(default_val) = default {
                            attributes.insert(
                                rename.as_ref().unwrap_or(named).clone(),
                                default_val.clone(),
                            );
                        }
                    }
                };
            }
        }

        attributes
    }

    pub(crate) fn get_from_response<T: Serialize>(
        &self,
        headers: &HeaderMap,
        body: &T,
    ) -> HashMap<String, String> {
        let mut attributes = HashMap::new();

        // Fill from static
        if let Some(to_insert) = &self.insert {
            for Insert { name, value } in to_insert {
                attributes.insert(name.clone(), value.clone());
            }
        }
        // Fill from response
        if let Some(from_response) = &self.response {
            if let Some(headers_forward) = &from_response.header {
                attributes.extend(headers_forward.iter().fold(
                    HashMap::new(),
                    |mut acc, current| {
                        acc.extend(current.get_from_headers(headers));
                        acc
                    },
                ));
            }
            if let Some(body_forward) = &from_response.body {
                for body_fw in body_forward {
                    let output = body_fw.path.execute(body).unwrap();
                    if let Some(val) = output {
                        if let Value::String(val_str) = val {
                            attributes.insert(body_fw.name.clone(), val_str);
                        } else {
                            attributes.insert(body_fw.name.clone(), val.to_string());
                        }
                    } else if let Some(default_val) = &body_fw.default {
                        attributes.insert(body_fw.name.clone(), default_val.clone());
                    }
                }
            }
        }

        attributes
    }

    pub(crate) fn get_from_request(
        &self,
        headers: &HeaderMap,
        body: &Request,
    ) -> HashMap<String, String> {
        let mut attributes = HashMap::new();

        // Fill from static
        if let Some(to_insert) = &self.insert {
            for Insert { name, value } in to_insert {
                attributes.insert(name.clone(), value.clone());
            }
        }
        // Fill from response
        if let Some(from_request) = &self.request {
            if let Some(headers_forward) = &from_request.header {
                attributes.extend(headers_forward.iter().fold(
                    HashMap::new(),
                    |mut acc, current| {
                        acc.extend(current.get_from_headers(headers));
                        acc
                    },
                ));
            }
            if let Some(body_forward) = &from_request.body {
                for body_fw in body_forward {
                    let output = body_fw.path.execute(body).unwrap(); //FIXME do not use unwrap
                    if let Some(val) = output {
                        if let Value::String(val_str) = val {
                            attributes.insert(body_fw.name.clone(), val_str);
                        } else {
                            attributes.insert(body_fw.name.clone(), val.to_string());
                        }
                    } else if let Some(default_val) = &body_fw.default {
                        attributes.insert(body_fw.name.clone(), default_val.clone());
                    }
                }
            }
        }

        attributes
    }

    pub(crate) fn get_static_names(&self) -> &'static [&'static str] {
        let mut static_names: Vec<&'static str> = Vec::new();
        if let Some(context) = &self.context {
            static_names.extend(context.iter().map(|ctx| {
                let current_str: &'static str = Box::leak::<'static>(
                    ctx.rename
                        .as_ref()
                        .unwrap_or(&ctx.named)
                        .clone()
                        .into_boxed_str(),
                );
                current_str
            }));
        }
        if let Some(insert) = &self.insert {
            static_names.extend(insert.iter().map(|i| {
                let current_str: &'static str =
                    Box::leak::<'static>(i.name.clone().into_boxed_str());
                current_str
            }));
        }
        if let Some(request) = &self.request {
            if let Some(req_body) = &request.body {
                static_names.extend(req_body.iter().map(|rb| {
                    let current_str: &'static str =
                        Box::leak::<'static>(rb.name.clone().into_boxed_str());
                    current_str
                }));
            }
            if let Some(req_header) = &request.header {
                static_names.extend(req_header.iter().map(|rh| match rh {
                    HeaderForward::Named { named, rename, .. } => {
                        let current_str: &'static str = Box::leak(
                            rename
                                .as_ref()
                                .unwrap_or(&named.to_string())
                                .clone()
                                .into_boxed_str(),
                        );
                        current_str
                    }
                    HeaderForward::Matching { .. } => {
                        unimplemented!(
                            "currently not supported, cannot add dynamic attribute name on tracing"
                        )
                    }
                }));
            }
        }
        if let Some(response) = &self.response {
            if let Some(res_body) = &response.body {
                static_names.extend(res_body.iter().map(|rb| {
                    let current_str: &'static str =
                        Box::leak::<'static>(rb.name.clone().into_boxed_str());
                    current_str
                }));
            }
            if let Some(res_header) = &response.header {
                static_names.extend(res_header.iter().map(|rh| match rh {
                    HeaderForward::Named { named, rename, .. } => {
                        let current_str: &'static str = Box::leak(
                            rename
                                .as_ref()
                                .unwrap_or(&named.to_string())
                                .clone()
                                .into_boxed_str(),
                        );
                        current_str
                    }
                    HeaderForward::Matching { .. } => {
                        unimplemented!(
                            "currently not supported, cannot add dynamic attribute name on tracing"
                        )
                    }
                }));
            }
        }

        dbg!(static_names.leak())
    }
}

impl SubgraphConf {
    pub(crate) fn get_static_names(&self) -> &'static [&'static str] {
        let mut static_names: Vec<&'static str> = Vec::new();
        if let Some(all) = &self.all {
            static_names.extend(all.get_static_names());
        }
        if let Some(subgraphs) = &self.subgraphs {
            static_names.extend(subgraphs.iter().flat_map(|(_, s)| s.get_static_names()));
        }

        static_names.leak()
    }
}

fn string_schema(gen: &mut schemars::gen::SchemaGenerator) -> schemars::schema::Schema {
    String::json_schema(gen)
}
