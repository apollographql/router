use std::collections::HashMap;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;

use futures::stream::once;
use futures::StreamExt;
use http::HeaderMap;
use http::HeaderValue;
use serde_json::json;
use tokio::fs;
use tower::BoxError;
use tower::ServiceBuilder;
use tower::ServiceExt as TowerServiceExt;

use super::recording::RequestDetails;
use super::recording::ResponseDetails;
use super::recording::Subgraph;
use super::recording::Subgraphs;
use crate::context::Context;
use crate::layers::ServiceBuilderExt;
use crate::plugin::Plugin;
use crate::plugin::PluginInit;
use crate::services::execution;
use crate::services::router;
use crate::services::subgraph;
use crate::services::supergraph;
use crate::spec::query::Query;
use crate::spec::Schema;
use crate::Configuration;

const RECORD_HEADER: &str = "x-apollo-router-record";
const RECORD: &str = "record";
const CLIENT_REQUEST: &str = "client_request";
const CLIENT_RESPONSE: &str = "client_response";
const QUERY_PLAN: &str = "query_plan";
const SUBGRAPHS: &str = "subgraphs";

/// Request recording configuration.
#[derive(Debug, Clone, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct RecordConfig {
    /// The recording plugin is disabled by default.
    enabled: bool,
    /// The path to the directory where recordings will be stored. Defaults to
    /// the current working directory.
    storage_path: Option<String>,
}

fn default_storage_path() -> String {
    std::env::current_dir()
        .expect("failed to get current directory")
        .to_str()
        .expect("failed to convert current directory to string")
        .to_string()
}

#[derive(Debug)]
struct Record {
    enabled: bool,
    supergraph_sdl: Arc<String>,
    storage_path: Arc<Path>,
    schema: Arc<Schema>,
}

register_plugin!("experimental", "record", Record);

#[async_trait::async_trait]
impl Plugin for Record {
    type Config = RecordConfig;

    async fn new(init: PluginInit<Self::Config>) -> Result<Self, BoxError> {
        let storage_path = PathBuf::from(
            init.config
                .storage_path
                .unwrap_or_else(default_storage_path),
        );

        write_file(
            storage_path.clone().into(),
            "README.md",
            include_str!("recording-readme.md").as_bytes(),
        )
        .await?;

        let plugin = Self {
            enabled: init.config.enabled,
            supergraph_sdl: init.supergraph_sdl.clone(),
            storage_path: storage_path.into(),
            schema: Arc::new(Schema::parse(
                init.supergraph_sdl.clone().as_str(),
                &Configuration::default(),
            )?),
        };

        Ok(plugin)
    }

    fn router_service(&self, service: router::BoxService) -> router::BoxService {
        if !self.enabled {
            return service;
        }

        let dir = self.storage_path.clone();
        let supergraph_sdl = self.supergraph_sdl.clone();

        ServiceBuilder::new()
            .map_future(move |future| {
                let dir = dir.clone();
                let supergraph_sdl = supergraph_sdl.clone();

                async move {
                    let res: router::Response = future.await?;
                    let (parts, stream) = res.response.into_parts();

                    let headers = parts.headers.clone();
                    let context = res.context.clone();

                    let after_complete = once(async move {
                        if recording_enabled(&context) {
                            let client_request =
                                context.get::<_, RequestDetails>(CLIENT_REQUEST)?;
                            let client_response =
                                context.get::<_, ResponseDetails>(CLIENT_RESPONSE)?;

                            if let (Some(client_request), Some(client_response)) =
                                (client_request, client_response)
                            {
                                let res_headers = externalize_header_map(&headers)?;
                                let client_response = ResponseDetails {
                                    headers: res_headers,
                                    ..client_response
                                };

                                let operation_name = client_request
                                    .operation_name
                                    .clone()
                                    .unwrap_or("UnnamedOperation".to_string());

                                let unix_time = std::time::SystemTime::now()
                                    .duration_since(std::time::UNIX_EPOCH)?
                                    .as_secs();

                                let filename =
                                    format!("{}-{}.json", operation_name, unix_time).to_string();

                                tokio::spawn(async move {
                                  tracing::info!("Writing recording to {}", filename);
                                  let contents = json!({
                                      "supergraph_sdl": &supergraph_sdl,
                                      "client_request": &client_request,
                                      "client_response": &client_response,
                                      "formatted_query_plan": &context.get::<_, String>(QUERY_PLAN)?,
                                      "subgraph_fetches": &context.get::<_, Subgraphs>(SUBGRAPHS)?,
                                  });

                                  write_file(
                                      dir,
                                      filename.as_str(),
                                      serde_json::to_string_pretty(&contents)?.as_bytes(),
                                  )
                                  .await?;

                                  Ok::<(), BoxError>(())
                                }).await??;
                            }
                        }
                        Ok::<Option<_>, BoxError>(None)
                    })
                    .filter_map(|a| async move { a.unwrap() });

                    let stream = stream.chain(after_complete);

                    Ok(router::Response {
                        context: res.context,
                        response: http::Response::from_parts(
                            parts,
                            hyper::Body::wrap_stream(stream),
                        ),
                    })
                }
            })
            .service(service)
            .boxed()
    }

    fn supergraph_service(&self, service: supergraph::BoxService) -> supergraph::BoxService {
        if !self.enabled {
            return service;
        }

        let schema = self.schema.clone();

        ServiceBuilder::new()
            .map_request(move |req: supergraph::Request| {
                if is_introspection(
                    req.supergraph_request.body().query.clone().unwrap(),
                    schema.clone(),
                ) {
                    return req;
                }

                let recording_enabled =
                    if req.supergraph_request.headers().contains_key(RECORD_HEADER) {
                        req.context.insert(RECORD, true).unwrap();
                        true
                    } else {
                        false
                    };

                if recording_enabled {
                    let query = req.supergraph_request.body().query.clone();
                    let operation_name = req.supergraph_request.body().operation_name.clone();
                    let variables = req.supergraph_request.body().variables.clone();
                    let headers = externalize_header_map(req.supergraph_request.headers())
                        .expect("failed to externalize header map");
                    req.context
                        .upsert::<_, RequestDetails>(CLIENT_REQUEST, |_value| RequestDetails {
                            query,
                            operation_name,
                            variables,
                            headers,
                        })
                        .expect("failed to insert client request into context");
                }
                req
            })
            .map_response(|res: supergraph::Response| {
                if recording_enabled(&res.context) {
                    let context = res.context.clone();

                    return res.map_stream(move |chunk| {
                        context
                            .upsert::<_, ResponseDetails>(CLIENT_RESPONSE, |mut value| {
                                value.chunks.push(chunk.clone());
                                value
                            })
                            .expect("failed to insert client response into context");
                        chunk
                    });
                }
                res
            })
            .service(service)
            .boxed()
    }

    fn execution_service(&self, service: execution::BoxService) -> execution::BoxService {
        ServiceBuilder::new()
            .map_request(|req: execution::Request| {
                if recording_enabled(&req.context) {
                    req.context
                        .insert(QUERY_PLAN, req.query_plan.formatted_query_plan.clone())
                        .unwrap();
                }
                req
            })
            .service(service)
            .boxed()
    }

    fn subgraph_service(
        &self,
        subgraph_name: &str,
        service: subgraph::BoxService,
    ) -> subgraph::BoxService {
        if !self.enabled {
            return service;
        }

        let subgraph_name = String::from(subgraph_name);

        ServiceBuilder::new()
            .map_future_with_request_data(
                |req: &subgraph::Request| RequestDetails {
                    query: req.subgraph_request.body().query.clone(),
                    operation_name: req.subgraph_request.body().operation_name.clone(),
                    variables: req.subgraph_request.body().variables.clone(),
                    headers: externalize_header_map(req.subgraph_request.headers())
                        .expect("failed to externalize header map"),
                },
                move |req: RequestDetails, future| {
                    let subgraph_name = subgraph_name.clone();
                    async move {
                        let res: subgraph::ServiceResult = future.await;

                        let operation_name = req
                            .operation_name
                            .clone()
                            .unwrap_or_else(|| "UnnamedOperation".to_string());

                        let res = match res {
                            Ok(res) => {
                                let subgraph = Subgraph {
                                    subgraph_name,
                                    response: ResponseDetails {
                                        headers: externalize_header_map(
                                            &res.response.headers().clone(),
                                        )
                                        .expect("failed to externalize header map"),
                                        chunks: vec![res.response.body().clone()],
                                    },
                                    request: req,
                                };

                                res.context
                                    .upsert::<_, Subgraphs>(SUBGRAPHS, |mut value| {
                                        value.insert(operation_name, subgraph);
                                        value
                                    })
                                    .expect("failed to insert subgraph into context");
                                Ok(res)
                            }
                            Err(err) => Err(err),
                        };

                        res
                    }
                },
            )
            .service(service)
            .boxed()
    }
}

async fn write_file(dir: Arc<Path>, path: &str, contents: &[u8]) -> Result<(), BoxError> {
    let path = dir.join(path);
    let dir = path.parent().unwrap();
    fs::create_dir_all(dir).await?;
    fs::write(path, contents).await?;
    Ok(())
}

fn recording_enabled(context: &Context) -> bool {
    context
        .get::<_, bool>(RECORD)
        .unwrap_or(None)
        .unwrap_or(false)
}

fn is_introspection(query: String, schema: Arc<Schema>) -> bool {
    let query = Query::parse(query, &schema, &Configuration::default()).expect("query must valid");
    query.contains_introspection()
}

fn externalize_header_map(
    input: &HeaderMap<HeaderValue>,
) -> Result<HashMap<String, Vec<String>>, BoxError> {
    let mut output = HashMap::new();
    for (k, v) in input {
        let k = k.as_str().to_owned();
        let v = String::from_utf8(v.as_bytes().to_vec()).map_err(|e| e.to_string())?;
        output.entry(k).or_insert_with(Vec::new).push(v)
    }
    Ok(output)
}
