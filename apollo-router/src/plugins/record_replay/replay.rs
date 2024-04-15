use std::borrow::Cow;
use std::collections::HashSet;
use std::ops::ControlFlow;
use std::path::Path;
use std::sync::Arc;
use std::sync::Mutex;

use console::style;
use http::Method;
use http::Uri;
use multimap::MultiMap;
use serde_json_bytes::ByteString;
use serde_json_bytes::Map;
use serde_json_bytes::Value;
use tokio::fs;
use tower::BoxError;
use tower::ServiceBuilder;
use tower::ServiceExt as TowerServiceExt;

use super::recording::Recording;
use crate::context::Context;
use crate::layers::ServiceBuilderExt;
use crate::plugin::Plugin;
use crate::plugin::PluginInit;
use crate::services::execution;
use crate::services::router;
use crate::services::subgraph;
use crate::services::supergraph;
use crate::services::TryIntoHeaderName;
use crate::services::TryIntoHeaderValue;

#[derive(Debug)]
pub(crate) struct Replay {
    recording: Recording,
    pub(crate) report: Arc<Mutex<Vec<ReplayReport>>>,
}

#[allow(dead_code)]
impl Replay {
    pub(crate) async fn from_file(recording_file: &Path) -> Result<Self, BoxError> {
        let recording = fs::read_to_string(recording_file).await?;
        let recording: Recording = serde_json::from_str(&recording)?;
        Ok(Self::new(recording))
    }

    pub(crate) fn new(recording: Recording) -> Self {
        Self {
            recording,
            report: Arc::default(),
        }
    }

    pub(crate) fn make_client_request(&self) -> Result<router::Request, BoxError> {
        let client_request = self.recording.client_request.clone();

        let mut request_headers: MultiMap<TryIntoHeaderName, TryIntoHeaderValue> = MultiMap::new();
        for (k, v) in client_request.headers {
            for v in v {
                let k = k.clone();
                request_headers.insert(k.into(), v.into());
            }
        }

        let req = supergraph::Request::builder()
            .query(client_request.query.unwrap().clone())
            .and_operation_name(client_request.operation_name.clone())
            .variables(client_request.variables.clone())
            .headers(request_headers)
            .context(Context::default())
            .uri(client_request.uri.parse::<Uri>().expect("uri is valid"))
            .method(
                client_request
                    .method
                    .parse::<Method>()
                    .expect("method is valid"),
            )
            .build()?;

        Ok(req.try_into()?)
    }

    pub(crate) fn supergraph_sdl(&self) -> String {
        self.recording.supergraph_sdl.clone()
    }
}

#[async_trait::async_trait]
impl Plugin for Replay {
    type Config = ();

    async fn new(_init: PluginInit<Self::Config>) -> Result<Self, BoxError> {
        unreachable!()
    }

    fn router_service(&self, service: router::BoxService) -> router::BoxService {
        let report = self.report.clone();
        let recorded_headers = self.recording.client_response.headers.clone();

        ServiceBuilder::new()
            .map_response(move |res: router::Response| {
                // TODO - check matching headers?
                for (k, recorded_values) in recorded_headers.iter() {
                    let recorded_set = recorded_values
                        .iter()
                        .map(|v| v.to_string())
                        .collect::<HashSet<_>>();
                    let runtime_values = res
                        .response
                        .headers()
                        .get_all(k)
                        .iter()
                        .map(|v| String::from(v.to_str().unwrap()))
                        .collect::<HashSet<_>>();

                    let missing_values =
                        recorded_set.difference(&runtime_values).collect::<Vec<_>>();

                    if !missing_values.is_empty() {
                        report.lock().unwrap().push(ReplayReport::HeaderDifference {
                            name: k.clone(),
                            recorded: recorded_values.clone(),
                            runtime: runtime_values.iter().map(|v| v.to_string()).collect(),
                        });
                    }
                }

                res
            })
            .service(service)
            .boxed()
    }

    fn supergraph_service(&self, service: supergraph::BoxService) -> supergraph::BoxService {
        let report = self.report.clone();
        let recorded_chunks = self.recording.client_response.chunks.clone();

        ServiceBuilder::new()
            .map_response(|res: supergraph::Response| {
                let mut i = 0;
                res.map_stream(move |chunk| {
                    let recorded_chunk = &recorded_chunks[i];
                    let chunk = chunk.clone();

                    // TODO - json string equality is sufficient?
                    let recorded_chunk_str = serde_json::to_string_pretty(&recorded_chunk).unwrap();
                    let chunk_str = serde_json::to_string_pretty(&chunk).unwrap();

                    if recorded_chunk_str != chunk_str {
                        report
                            .lock()
                            .unwrap()
                            .push(ReplayReport::ClientResponseChunkDifference(
                                i,
                                recorded_chunk_str.clone(),
                                chunk_str.clone(),
                            ));
                    }

                    i += 1;
                    chunk
                })
            })
            .service(service)
            .boxed()
    }

    fn execution_service(&self, service: execution::BoxService) -> execution::BoxService {
        let recorded = self.recording.formatted_query_plan.clone();
        let report = self.report.clone();
        ServiceBuilder::new()
            .map_request(move |req: execution::Request| {
                let recorded = recorded.clone().unwrap_or_default();
                let runtime = req
                    .query_plan
                    .formatted_query_plan
                    .clone()
                    .unwrap_or_default();

                if recorded != runtime {
                    report
                        .lock()
                        .unwrap()
                        .push(ReplayReport::QueryPlanDifference(
                            recorded.clone(),
                            runtime.clone(),
                        ));
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
        let subgraph_name = String::from(subgraph_name);

        let report = self.report.clone();
        let fetches = self.recording.subgraph_fetches.clone().unwrap_or_default();

        ServiceBuilder::new()
            .checkpoint(move |req: subgraph::Request| {
                let report = report.clone();
                let operation_name = req
                    .subgraph_request
                    .body()
                    .operation_name
                    .clone()
                    .unwrap_or("UnnamedOperation".to_string());

                // Note - not doing an equality check (yet) here because the query plan
                // would mismatch if the request is wrong
                if let Some(fetch) = fetches.get(&operation_name) {
                    let subgraph_response = subgraph::Response::new_from_response(
                        http::Response::new(fetch.response.chunks[0].clone()),
                        req.context.clone(),
                    );

                    let runtime_variables = req.subgraph_request.body().variables.clone();
                    let recorded_variables = fetch.request.variables.clone();

                    if runtime_variables != recorded_variables {
                        report
                            .lock()
                            .unwrap()
                            .push(ReplayReport::VariablesDifference {
                                name: operation_name.clone(),
                                runtime: runtime_variables,
                                recorded: recorded_variables,
                            });
                    }

                    Ok(ControlFlow::Break(subgraph_response))
                } else {
                    report
                        .lock()
                        .unwrap()
                        .push(ReplayReport::SubgraphRequestMissed(
                            subgraph_name.clone(),
                            operation_name.clone(),
                        ));

                    // TODO: break with an empty response or error instead? If
                    // the subgraph routing url is accessible this will hit the
                    // network
                    Ok(ControlFlow::Continue(req))
                }
            })
            .service(service)
            .boxed()
    }
}

#[derive(Debug)]
pub(crate) enum ReplayReport {
    QueryPlanDifference(String, String),
    ClientResponseChunkDifference(usize, String, String),
    SubgraphRequestMissed(String, String),
    HeaderDifference {
        name: String,
        recorded: Vec<String>,
        runtime: Vec<String>,
    },
    VariablesDifference {
        name: String,
        recorded: Map<ByteString, Value>,
        runtime: Map<ByteString, Value>,
    },
}

// Aspects of this are liberally borrowed from [insta](https://insta.rs/)
#[allow(dead_code)]
impl ReplayReport {
    pub(crate) fn print(&self) {
        match self {
            ReplayReport::QueryPlanDifference(recorded, runtime) => {
                println!("{}", style("Query Plan").red().bold());
                self.print_changeset(
                    recorded.as_str(),
                    runtime.as_str(),
                    "From Recording",
                    "From Runtime",
                )
            }
            ReplayReport::ClientResponseChunkDifference(index, recorded, runtime) => {
                println!(
                    "{}{}",
                    style("Client response chunk #").bold(),
                    style(index).bold()
                );
                self.print_changeset(
                    recorded.as_str(),
                    runtime.as_str(),
                    "From Recording",
                    "From Runtime",
                )
            }
            ReplayReport::SubgraphRequestMissed(_subgraph, operation_name) => {
                println!(
                    "{} {}",
                    style("Missing subgraph request:").bold(),
                    style(operation_name).red().bold()
                );
                print_line(74);
            }
            ReplayReport::HeaderDifference {
                name,
                recorded,
                runtime,
            } => {
                println!(
                    "{} {}",
                    style("Mismatched Header:").bold(),
                    style(name).red().bold()
                );
                self.print_changeset(
                    serde_json::to_string_pretty(&recorded).unwrap().as_str(),
                    serde_json::to_string_pretty(&runtime).unwrap().as_str(),
                    "From Recording",
                    "From Runtime",
                );
            }
            ReplayReport::VariablesDifference {
                name,
                recorded,
                runtime,
            } => {
                println!(
                    "{} {}",
                    style("Mismatched Variables:").bold(),
                    style(name).red().bold()
                );
                self.print_changeset(
                    serde_json::to_string_pretty(&recorded).unwrap().as_str(),
                    serde_json::to_string_pretty(&runtime).unwrap().as_str(),
                    "From Recording",
                    "From Runtime",
                );
            }
        }
    }

    pub(crate) fn print_changeset(&self, old: &str, new: &str, old_hint: &str, new_hint: &str) {
        let newlines_matter = false; //newlines_matter(old, new);

        let width = 74; //term_width();
        let diff = similar::TextDiff::configure()
            .algorithm(similar::Algorithm::Patience)
            .timeout(std::time::Duration::from_millis(500))
            .diff_lines(old, new);
        print_line(width);

        if !old.is_empty() {
            println!("{}", style(format_args!("-{}", old_hint)).red());
        }
        println!("{}", style(format_args!("+{}", new_hint)).green());

        println!("────────────┬{:─^1$}", "", width.saturating_sub(13));
        let mut has_changes = false;
        for (idx, group) in diff.grouped_ops(10).iter().enumerate() {
            if idx > 0 {
                println!("┈┈┈┈┈┈┈┈┈┈┈┈┼{:┈^1$}", "", width.saturating_sub(13));
            }
            for op in group {
                for change in diff.iter_inline_changes(op) {
                    match change.tag() {
                        similar::ChangeTag::Insert => {
                            has_changes = true;
                            print!(
                                "{:>5} {:>5} │{}",
                                "",
                                style(change.new_index().unwrap()).cyan().dim().bold(),
                                style("+").green(),
                            );
                            for &(emphasized, change) in change.values() {
                                let change = render_invisible(change, newlines_matter);
                                if emphasized {
                                    print!("{}", style(change).green().underlined());
                                } else {
                                    print!("{}", style(change).green());
                                }
                            }
                        }
                        similar::ChangeTag::Delete => {
                            has_changes = true;
                            print!(
                                "{:>5} {:>5} │{}",
                                style(change.old_index().unwrap()).cyan().dim(),
                                "",
                                style("-").red(),
                            );
                            for &(emphasized, change) in change.values() {
                                let change = render_invisible(change, newlines_matter);
                                if emphasized {
                                    print!("{}", style(change).red().underlined());
                                } else {
                                    print!("{}", style(change).red());
                                }
                            }
                        }
                        similar::ChangeTag::Equal => {
                            print!(
                                "{:>5} {:>5} │ ",
                                style(change.old_index().unwrap()).cyan().dim(),
                                style(change.new_index().unwrap()).cyan().dim().bold(),
                            );
                            for &(_, change) in change.values() {
                                let change = render_invisible(change, newlines_matter);
                                print!("{}", style(change).dim());
                            }
                        }
                    }
                    if change.missing_newline() {
                        println!();
                    }
                }
            }
        }

        if !has_changes {
            println!(
                "{:>5} {:>5} │{}",
                "",
                style("-").dim(),
                style(" snapshots are matching").cyan(),
            );
        }

        println!("────────────┴{:─^1$}", "", width.saturating_sub(13));
    }
}

fn print_line(width: usize) {
    println!("{:═^1$}", "", width);
}

fn render_invisible(s: &str, newlines_matter: bool) -> Cow<'_, str> {
    if newlines_matter || s.find(&['\x1b', '\x07', '\x08', '\x7f'][..]).is_some() {
        Cow::Owned(
            s.replace('\r', "␍\r")
                .replace('\n', "␊\n")
                .replace("␍\r␊\n", "␍␊\r\n")
                .replace('\x07', "␇")
                .replace('\x08', "␈")
                .replace('\x1b', "␛")
                .replace('\x7f', "␡"),
        )
    } else {
        Cow::Borrowed(s)
    }
}

#[path = "replay_tests.rs"]
mod tests;
