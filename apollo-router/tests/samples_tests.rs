use std::collections::HashMap;
use std::env;
use std::error::Error;
use std::fmt::Write;
use std::fs;
use std::fs::File;
use std::io::Read;
use std::net::SocketAddr;
use std::net::TcpListener;
use std::path::Path;
use std::path::PathBuf;
use std::process::ExitCode;

use libtest_mimic::Arguments;
use libtest_mimic::Failed;
use libtest_mimic::Trial;
use serde::Deserialize;
use serde_json::Value;
use tokio::runtime::Runtime;
use wiremock::matchers::body_partial_json;
use wiremock::Mock;
use wiremock::MockServer;
use wiremock::ResponseTemplate;

#[path = "./common.rs"]
pub(crate) mod common;
pub(crate) use common::IntegrationTest;

fn main() -> Result<ExitCode, Box<dyn Error>> {
    let args = Arguments::from_args();
    let mut tests = Vec::new();
    let path = env::current_dir()?.join("tests/samples");

    lookup_dir(&path, "", &mut tests)?;

    Ok(libtest_mimic::run(&args, tests).exit_code())
}

fn lookup_dir(
    path: &Path,
    name_prefix: &str,
    tests: &mut Vec<Trial>,
) -> Result<(), Box<dyn Error>> {
    for entry in fs::read_dir(path)? {
        let entry = entry?;

        if entry.file_type()?.is_dir() {
            let path = entry.path();
            let name = format!(
                "{name_prefix}/{}",
                path.file_name().unwrap().to_str().unwrap()
            );

            if path.join("plan.json").exists() {
                tests.push(Trial::test(name, move || test(&path)));
            } else {
                lookup_dir(&path, &name, tests)?;
            }
        }
    }

    Ok(())
}

fn test(path: &PathBuf) -> Result<(), Failed> {
    //libtest_mimic does not support stdout capture
    let mut out = String::new();
    writeln!(&mut out, "test at path: {path:?}").unwrap();
    if let Ok(file) = open_file(&path.join("README.md"), &mut out) {
        writeln!(&mut out, "{file}\n\n============\n\n").unwrap();
    }

    let plan: Plan = match serde_json::from_str(&open_file(&path.join("plan.json"), &mut out)?) {
        Ok(data) => data,
        Err(e) => {
            writeln!(&mut out, "could not deserialize test plan: {e}").unwrap();
            return Err(out.into());
        }
    };

    let rt = Runtime::new()?;

    // Spawn the root task
    rt.block_on(async {
        let mut execution = TestExecution::new();
        for action in plan.actions {
            execution.execute_action(&action, path, &mut out).await?;
        }

        Ok(())
    })
}

struct TestExecution {
    router: Option<IntegrationTest>,
    subgraphs_server: Option<MockServer>,
    subgraphs: HashMap<String, Subgraph>,
}

impl TestExecution {
    fn new() -> Self {
        TestExecution {
            router: None,
            subgraphs_server: None,
            subgraphs: HashMap::new(),
        }
    }

    async fn execute_action(
        &mut self,
        action: &Action,
        path: &Path,
        out: &mut String,
    ) -> Result<(), Failed> {
        match action {
            Action::Start {
                schema_path,
                configuration_path,
                subgraphs,
            } => {
                self.start(schema_path, configuration_path, subgraphs, path, out)
                    .await
            }
            Action::ReloadConfiguration { configuration_path } => {
                self.reload_configuration(configuration_path, path, out)
                    .await
            }
            Action::ReloadSchema { schema_path } => {
                self.reload_schema(schema_path, path, out).await
            }
            Action::Request {
                request,
                query_path,
                expected_response,
            } => {
                self.request(
                    request.clone(),
                    query_path.as_deref(),
                    expected_response,
                    path,
                    out,
                )
                .await
            }
            Action::Stop => self.stop(out).await,
        }
    }

    async fn start(
        &mut self,
        schema_path: &str,
        configuration_path: &str,
        subgraphs: &HashMap<String, Subgraph>,
        path: &Path,
        out: &mut String,
    ) -> Result<(), Failed> {
        let listener = TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0))).unwrap();
        let address = listener.local_addr().unwrap();
        let url = format!("http://{address}/");

        let subgraphs_server = wiremock::MockServer::builder()
            .listener(listener)
            .start()
            .await;

        writeln!(out, "subgraphs listening on {url}").unwrap();

        let mut subgraph_overrides = HashMap::new();

        for (name, subgraph) in subgraphs {
            for SubgraphRequest { request, response } in &subgraph.requests {
                Mock::given(body_partial_json(request))
                    .respond_with(ResponseTemplate::new(200).set_body_json(response))
                    .mount(&subgraphs_server)
                    .await;
            }

            // Add a default override for products, if not specified
            subgraph_overrides
                .entry(name.to_string())
                .or_insert(url.clone());
        }

        let config = open_file(&path.join(configuration_path), out)?;
        let schema_path = path.join(schema_path);
        check_path(&schema_path, out)?;

        let mut router = IntegrationTest::builder()
            .config(&config)
            .supergraph(schema_path)
            .subgraph_overrides(subgraph_overrides)
            .build()
            .await;
        router.start().await;
        router.assert_started().await;

        self.router = Some(router);
        self.subgraphs_server = Some(subgraphs_server);
        self.subgraphs = subgraphs.clone();

        Ok(())
    }

    async fn reload_configuration(
        &mut self,
        configuration_path: &str,
        path: &Path,
        out: &mut String,
    ) -> Result<(), Failed> {
        let router = match self.router.as_mut() {
            None => {
                writeln!(
                    out,
                    "cannot reload router configuration: router was not started"
                )
                .unwrap();
                return Err(out.into());
            }
            Some(router) => router,
        };

        let listener = TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0))).unwrap();
        let address = listener.local_addr().unwrap();
        let url = format!("http://{address}/");

        let subgraphs_server = wiremock::MockServer::builder()
            .listener(listener)
            .start()
            .await;

        writeln!(out, "subgraphs listening on {url}").unwrap();

        let mut subgraph_overrides = HashMap::new();

        for (name, subgraph) in &self.subgraphs {
            for SubgraphRequest { request, response } in &subgraph.requests {
                Mock::given(body_partial_json(request))
                    .respond_with(ResponseTemplate::new(200).set_body_json(response))
                    .mount(&subgraphs_server)
                    .await;
            }

            // Add a default override for products, if not specified
            subgraph_overrides
                .entry(name.to_string())
                .or_insert(url.clone());
        }

        let config = open_file(&path.join(configuration_path), out)?;

        router.update_config(&config).await;
        router.assert_reloaded().await;

        Ok(())
    }

    async fn reload_schema(
        &mut self,
        schema_path: &str,
        path: &Path,
        out: &mut String,
    ) -> Result<(), Failed> {
        let router = match self.router.as_mut() {
            None => {
                writeln!(
                    out,
                    "cannot reload router configuration: router was not started"
                )
                .unwrap();
                return Err(out.into());
            }
            Some(router) => router,
        };

        let schema_path = path.join(schema_path);

        router.update_schema(&schema_path).await;
        router.assert_reloaded().await;

        Ok(())
    }

    async fn stop(&mut self, out: &mut String) -> Result<(), Failed> {
        if let Some(mut router) = self.router.take() {
            router.graceful_shutdown().await;
            Ok(())
        } else {
            writeln!(out, "could not shutdown router: router was not started").unwrap();
            Err(out.into())
        }
    }

    async fn request(
        &mut self,
        mut request: Value,
        query_path: Option<&str>,
        expected_response: &Value,
        path: &Path,
        out: &mut String,
    ) -> Result<(), Failed> {
        let router = match self.router.as_mut() {
            None => {
                writeln!(
                    out,
                    "cannot send request to the router: router was not started"
                )
                .unwrap();
                return Err(out.into());
            }
            Some(router) => router,
        };

        if let Some(query_path) = query_path {
            let query: String = open_file(&path.join(query_path), out)?;
            if let Some(req) = request.as_object_mut() {
                req.insert("query".to_string(), query.into());
            }
        }

        writeln!(out, "query: {}\n", serde_json::to_string(&request).unwrap()).unwrap();
        let (_, response) = router.execute_query(&request).await;
        let body = response.bytes().await.map_err(|e| {
            writeln!(out, "could not get graphql response data: {e}").unwrap();
            let f: Failed = out.clone().into();
            f
        })?;
        let graphql_response: Value = serde_json::from_slice(&body).map_err(|e| {
            writeln!(out, "could not deserialize graphql response data: {e}").unwrap();
            let f: Failed = out.clone().into();
            f
        })?;

        if expected_response != &graphql_response {
            if let Some(requests) = self
                .subgraphs_server
                .as_ref()
                .unwrap()
                .received_requests()
                .await
            {
                writeln!(out, "subgraphs received requests:").unwrap();
                for request in requests {
                    writeln!(out, "\t{}\n", std::str::from_utf8(&request.body).unwrap()).unwrap();
                }
            } else {
                writeln!(out, "subgraphs received no requests").unwrap();
            }

            writeln!(out, "assertion `left == right` failed").unwrap();
            writeln!(
                out,
                " left: {}",
                serde_json::to_string(&expected_response).unwrap()
            )
            .unwrap();
            writeln!(
                out,
                "right: {}",
                serde_json::to_string(&graphql_response).unwrap()
            )
            .unwrap();
            return Err(out.into());
        }

        Ok(())
    }
}

fn open_file(path: &Path, out: &mut String) -> Result<String, Failed> {
    let mut file = File::open(path).map_err(|e| {
        writeln!(out, "could not open file at path '{path:?}': {e}").unwrap();
        let f: Failed = out.into();
        f
    })?;

    let mut s = String::new();
    file.read_to_string(&mut s).map_err(|e| {
        writeln!(out, "could not read file at path: '{path:?}': {e} ").unwrap();
        let f: Failed = out.into();
        f
    })?;
    Ok(s)
}

fn check_path(path: &Path, out: &mut String) -> Result<(), Failed> {
    if !path.is_file() {
        writeln!(out, "could not find file at path: {path:?}").unwrap();
        return Err(out.into());
    }
    Ok(())
}

#[derive(Deserialize)]
struct Plan {
    actions: Vec<Action>,
}

#[derive(Deserialize)]
#[serde(tag = "type")]
enum Action {
    Start {
        schema_path: String,
        configuration_path: String,
        subgraphs: HashMap<String, Subgraph>,
    },
    ReloadConfiguration {
        configuration_path: String,
    },
    ReloadSchema {
        schema_path: String,
    },
    Request {
        request: Value,
        query_path: Option<String>,
        expected_response: Value,
    },
    Stop,
}

#[derive(Clone, Deserialize)]
struct Subgraph {
    requests: Vec<SubgraphRequest>,
}

#[derive(Clone, Deserialize)]
struct SubgraphRequest {
    request: Value,
    response: Value,
}
