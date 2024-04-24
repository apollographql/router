use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;

use futures::stream::once;
use futures::StreamExt;
use tokio::fs;
use tower::BoxError;
use tower::ServiceBuilder;
use tower::ServiceExt as TowerServiceExt;

use super::recording::Recording;
use super::recording::RequestDetails;
use super::recording::ResponseDetails;
use super::recording::Subgraph;
use crate::layers::ServiceBuilderExt;
use crate::plugin::Plugin;
use crate::plugin::PluginInit;
use crate::services::execution;
use crate::services::external::externalize_header_map;
use crate::services::router;
use crate::services::subgraph;
use crate::services::supergraph;
use crate::spec::query::Query;
use crate::spec::Schema;
use crate::Configuration;

const RECORD_HEADER: &str = "x-apollo-router-record";

/// Request recording configuration.
#[derive(Debug, Clone, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct RecordConfig {
    /// The recording plugin is disabled by default.
    enabled: bool,
    /// The path to the directory where recordings will be stored. Defaults to
    /// the current working directory.
    storage_path: Option<PathBuf>,
}

fn default_storage_path() -> PathBuf {
    std::env::current_dir().expect("failed to get current directory")
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
        let storage_path = init
            .config
            .storage_path
            .unwrap_or_else(default_storage_path);

        let plugin = Self {
            enabled: init.config.enabled,
            supergraph_sdl: init.supergraph_sdl.clone(),
            storage_path: storage_path.clone().into(),
            schema: Arc::new(Schema::parse(
                init.supergraph_sdl.clone().as_str(),
                &Default::default(),
            )?),
        };

        if init.config.enabled {
            write_file(
                storage_path.into(),
                &PathBuf::from("README.md"),
                include_str!("recording-readme.md").as_bytes(),
            )
            .await?;
        }

        Ok(plugin)
    }

    fn router_service(&self, service: router::BoxService) -> router::BoxService {
        if !self.enabled {
            return service;
        }

        let dir = self.storage_path.clone();

        ServiceBuilder::new()
            .map_future(move |future| {
                let dir = dir.clone();

                async move {
                    let res: router::Response = future.await?;
                    let (parts, stream) = res.response.into_parts();

                    let headers = parts.headers.clone();
                    let context = res.context.clone();

                    let after_complete = once(async move {
                        let recording = context.extensions().lock().remove::<Recording>();

                        if let Some(mut recording) = recording {
                            let res_headers = externalize_header_map(&headers)?;
                            recording.client_response.headers = res_headers;

                            let filename = recording.filename();
                            let contents = serde_json::to_value(recording)?;

                            tokio::spawn(async move {
                                tracing::info!("Writing recording to {:?}", filename);

                                write_file(
                                    dir,
                                    &filename,
                                    serde_json::to_string_pretty(&contents)?.as_bytes(),
                                )
                                .await?;

                                Ok::<(), BoxError>(())
                            })
                            .await??;
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
        let supergraph_sdl = self.supergraph_sdl.clone();

        ServiceBuilder::new()
            .map_request(move |req: supergraph::Request| {
                if is_introspection(
                    req.supergraph_request
                        .body()
                        .query
                        .clone()
                        .unwrap_or_default(),
                    req.supergraph_request.body().operation_name.as_deref(),
                    schema.clone(),
                ) {
                    return req;
                }

                let recording_enabled =
                    if req.supergraph_request.headers().contains_key(RECORD_HEADER) {
                        req.context.extensions().lock().insert(Recording {
                            supergraph_sdl: supergraph_sdl.clone().to_string(),
                            client_request: Default::default(),
                            client_response: Default::default(),
                            formatted_query_plan: Default::default(),
                            subgraph_fetches: Default::default(),
                        });
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
                    let method = req.supergraph_request.method().to_string();
                    let uri = req.supergraph_request.uri().to_string();

                    if let Some(recording) = req.context.extensions().lock().get_mut::<Recording>()
                    {
                        recording.client_request = RequestDetails {
                            query,
                            operation_name,
                            variables,
                            headers,
                            method,
                            uri,
                        };
                    }
                }
                req
            })
            .map_response(|res: supergraph::Response| {
                let context = res.context.clone();
                res.map_stream(move |chunk| {
                    if let Some(recording) = context.extensions().lock().get_mut::<Recording>() {
                        recording.client_response.chunks.push(chunk.clone());
                    }

                    chunk
                })
            })
            .service(service)
            .boxed()
    }

    fn execution_service(&self, service: execution::BoxService) -> execution::BoxService {
        ServiceBuilder::new()
            .map_request(|req: execution::Request| {
                if let Some(recording) = req.context.extensions().lock().get_mut::<Recording>() {
                    recording.formatted_query_plan = req.query_plan.formatted_query_plan.clone();
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
                    method: req.subgraph_request.method().to_string(),
                    uri: req.subgraph_request.uri().to_string(),
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

                                if let Some(recording) =
                                    res.context.extensions().lock().get_mut::<Recording>()
                                {
                                    if recording.subgraph_fetches.is_none() {
                                        recording.subgraph_fetches = Some(Default::default());
                                    }

                                    if let Some(fetches) = &mut recording.subgraph_fetches {
                                        fetches.insert(operation_name, subgraph);
                                    }
                                }
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

async fn write_file(dir: Arc<Path>, path: &PathBuf, contents: &[u8]) -> Result<(), BoxError> {
    let path = dir.join(path);
    let dir = path.parent().ok_or("invalid record directory")?;
    fs::create_dir_all(dir).await?;
    fs::write(path, contents).await?;
    Ok(())
}

fn is_introspection(query: String, operation_name: Option<&str>, schema: Arc<Schema>) -> bool {
    Query::parse(query, operation_name, &schema, &Configuration::default())
        .map(|q| q.contains_introspection())
        .unwrap_or_default()
}
