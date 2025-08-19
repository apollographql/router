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

use futures::FutureExt as _;
use libtest_mimic::Arguments;
use libtest_mimic::Failed;
use libtest_mimic::Trial;
use mediatype::MediaTypeList;
use mediatype::ReadParams;
use multer::Multipart;
use serde::Deserialize;
use serde_json::Value;
use tokio::runtime::Runtime;
use wiremock::Mock;
use wiremock::MockServer;
use wiremock::ResponseTemplate;
use wiremock::matchers::body_partial_json;
use wiremock::matchers::header;
use wiremock::matchers::method;

#[path = "./common.rs"]
pub(crate) mod common;
pub(crate) use common::IntegrationTest;

use crate::common::Query;
use crate::common::graph_os_enabled;

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

            let plan: Option<Plan> = if path.join("plan.json").exists() {
                let mut file = File::open(path.join("plan.json")).map_err(|e| {
                    format!(
                        "could not open file at path '{:?}': {e}",
                        &path.join("plan.json")
                    )
                })?;
                let mut s = String::new();
                file.read_to_string(&mut s).map_err(|e| {
                    format!(
                        "could not read file at path: '{:?}': {e}",
                        &path.join("plan.json")
                    )
                })?;

                match serde_json::from_str(&s) {
                    Ok(data) => Some(data),
                    Err(e) => {
                        return Err(format!(
                            "could not deserialize test plan at {}: {e}",
                            path.display()
                        )
                        .into());
                    }
                }
            } else if path.join("plan.yaml").exists() {
                let mut file = File::open(path.join("plan.yaml")).map_err(|e| {
                    format!(
                        "could not open file at path '{:?}': {e}",
                        &path.join("plan.yaml")
                    )
                })?;
                let mut s = String::new();
                file.read_to_string(&mut s).map_err(|e| {
                    format!(
                        "could not read file at path: '{:?}': {e}",
                        &path.join("plan.yaml")
                    )
                })?;

                match serde_yaml::from_str(&s) {
                    Ok(data) => Some(data),
                    Err(e) => {
                        return Err(format!(
                            "could not deserialize test plan at {}: {e}",
                            path.display()
                        )
                        .into());
                    }
                }
            } else {
                None
            };

            if let Some(plan) = plan {
                if plan.enterprise && !graph_os_enabled() {
                    continue;
                }

                #[cfg(all(feature = "ci", not(all(target_arch = "x86_64", target_os = "linux"))))]
                if plan.redis {
                    continue;
                }

                #[cfg(not(feature = "snapshot"))]
                if plan.snapshot {
                    continue;
                }

                tests.push(Trial::test(name, move || test(&path, plan)));
            } else {
                lookup_dir(&path, &name, tests)?;
            }
        }
    }

    Ok(())
}

fn test(path: &PathBuf, plan: Plan) -> Result<(), Failed> {
    //libtest_mimic does not support stdout capture
    let mut out = String::new();
    writeln!(&mut out, "test at path: {path:?}").unwrap();
    if let Ok(file) = open_file(&path.join("README.md"), &mut out) {
        writeln!(&mut out, "{file}\n\n============\n\n").unwrap();
    }

    let rt = Runtime::new()?;

    // Spawn the root task
    let caught = rt.block_on(
        std::panic::AssertUnwindSafe(async {
            let mut execution = TestExecution::new();
            for action in plan.actions {
                execution.execute_action(&action, path, &mut out).await?;
            }

            Ok(())
        })
        .catch_unwind(),
    );

    match caught {
        Ok(result) => result,
        Err(any) => {
            print!("{out}");
            std::panic::resume_unwind(any);
        }
    }
}

struct TestExecution {
    router: Option<IntegrationTest>,
    subgraphs_server: Option<MockServer>,
    subgraphs: HashMap<String, Subgraph>,
    configuration_path: Option<String>,
}

impl TestExecution {
    fn new() -> Self {
        TestExecution {
            router: None,
            subgraphs_server: None,
            subgraphs: HashMap::new(),
            configuration_path: None,
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
            Action::ReloadSubgraphs {
                subgraphs,
                update_url_overrides,
            } => {
                self.reload_subgraphs(subgraphs, *update_url_overrides, path, out)
                    .await
            }
            Action::Request {
                description,
                request,
                query_path,
                headers,
                expected_response,
                expected_headers,
            } => {
                self.request(
                    description.clone(),
                    request.clone(),
                    query_path.as_deref(),
                    headers,
                    expected_response,
                    expected_headers,
                    path,
                    out,
                )
                .await
            }
            Action::EndpointRequest { url, request } => {
                self.endpoint_request(url, request.clone(), out).await
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
        self.subgraphs = subgraphs.clone();
        let (mut subgraphs_server, url) = self.start_subgraphs(out).await;

        let subgraph_overrides = self
            .load_subgraph_mocks(&mut subgraphs_server, &url, path, out)
            .await;
        writeln!(out, "got subgraph mocks: {subgraph_overrides:?}")?;

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
        self.configuration_path = Some(configuration_path.to_string());

        Ok(())
    }

    async fn reload_configuration(
        &mut self,
        configuration_path: &str,
        path: &Path,
        out: &mut String,
    ) -> Result<(), Failed> {
        if let Some(requests) = self
            .subgraphs_server
            .as_ref()
            .unwrap()
            .received_requests()
            .await
        {
            writeln!(out, "Will reload config, subgraphs received requests:").unwrap();
            for request in requests {
                writeln!(out, "\tmethod: {}", request.method).unwrap();
                writeln!(out, "\tpath: {}", request.url).unwrap();
                writeln!(out, "\t{}\n", std::str::from_utf8(&request.body).unwrap()).unwrap();
            }
        } else {
            writeln!(out, "subgraphs received no requests").unwrap();
        }
        let mut subgraphs_server = match self.subgraphs_server.take() {
            Some(subgraphs_server) => subgraphs_server,
            None => self.start_subgraphs(out).await.0,
        };
        subgraphs_server.reset().await;

        let subgraph_url = Self::subgraph_url(&subgraphs_server);
        let subgraph_overrides = self
            .load_subgraph_mocks(&mut subgraphs_server, &subgraph_url, path, out)
            .await;

        let config = open_file(&path.join(configuration_path), out)?;
        self.configuration_path = Some(configuration_path.to_string());
        self.subgraphs_server = Some(subgraphs_server);

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

        router.update_subgraph_overrides(subgraph_overrides);
        router.update_config(&config).await;
        router.assert_reloaded().await;

        Ok(())
    }

    async fn start_subgraphs(&mut self, out: &mut String) -> (MockServer, String) {
        let listener = TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0))).unwrap();
        let address = listener.local_addr().unwrap();
        let url = format!("http://{address}/");

        let subgraphs_server = wiremock::MockServer::builder()
            .listener(listener)
            .start()
            .await;

        writeln!(out, "subgraphs listening on {url}").unwrap();

        (subgraphs_server, url)
    }

    fn subgraph_url(server: &MockServer) -> String {
        format!("http://{}/", server.address())
    }

    async fn load_subgraph_mocks(
        &mut self,
        subgraphs_server: &mut MockServer,
        url: &str,
        #[cfg_attr(not(feature = "snapshot"), allow(unused_variables))] path: &Path,
        #[cfg_attr(not(feature = "snapshot"), allow(unused_variables, clippy::ptr_arg))]
        out: &mut String,
    ) -> HashMap<String, String> {
        let mut subgraph_overrides = HashMap::new();

        #[cfg_attr(not(feature = "snapshot"), allow(unused_variables))]
        for (name, subgraph) in &self.subgraphs {
            if let Some(snapshot) = subgraph.snapshot.as_ref() {
                #[cfg(feature = "snapshot")]
                {
                    use std::str::FromStr;

                    use http::Uri;
                    use http::header::CONTENT_LENGTH;
                    use http::header::CONTENT_TYPE;

                    let snapshot_server = apollo_router::SnapshotServer::spawn(
                        &path.join(&snapshot.path),
                        Uri::from_str(&snapshot.base_url).unwrap(),
                        true,
                        snapshot.update.unwrap_or(false),
                        Some(vec![CONTENT_TYPE.to_string(), CONTENT_LENGTH.to_string()]),
                        snapshot.port,
                    )
                    .await;
                    let snapshot_url = snapshot_server.uri();
                    writeln!(
                        out,
                        "snapshot server for {name} listening on {snapshot_url}"
                    )
                    .unwrap();
                    subgraph_overrides
                        .entry(name.to_string())
                        .or_insert(snapshot_url.clone());
                }
                #[cfg(not(feature = "snapshot"))]
                panic!("Tests using the snapshot feature must have `snapshot` set to `true`")
            } else {
                for SubgraphRequestMock { request, response } in
                    subgraph.requests.as_ref().unwrap_or(&vec![])
                {
                    let mut builder = match &request.body {
                        Some(body) => Mock::given(body_partial_json(body)),
                        None => Mock::given(wiremock::matchers::AnyMatcher),
                    };

                    if let Some(s) = request.method.as_deref() {
                        builder = builder.and(method(s));
                    }

                    if let Some(s) = request.path.as_deref() {
                        builder = builder.and(wiremock::matchers::path(s));
                    }

                    for (header_name, header_value) in &request.headers {
                        builder = builder.and(header(header_name.as_str(), header_value.as_str()));
                    }

                    let mut res = ResponseTemplate::new(response.status.unwrap_or(200));
                    for (header_name, header_value) in &response.headers {
                        res = res.append_header(header_name.as_str(), header_value.as_str());
                    }
                    builder
                        .respond_with(res.set_body_json(&response.body))
                        .mount(subgraphs_server)
                        .await;
                }
            }

            // Add a default override for products, if not specified
            subgraph_overrides
                .entry(name.to_string())
                .or_insert(url.to_owned());
        }

        subgraph_overrides
    }

    async fn reload_subgraphs(
        &mut self,
        subgraphs: &HashMap<String, Subgraph>,
        update_url_overrides: bool,
        path: &Path,
        out: &mut String,
    ) -> Result<(), Failed> {
        writeln!(out, "reloading subgraphs with: {subgraphs:?}").unwrap();

        let mut subgraphs_server = match self.subgraphs_server.take() {
            Some(subgraphs_server) => subgraphs_server,
            None => self.start_subgraphs(out).await.0,
        };
        subgraphs_server.reset().await;

        self.subgraphs = subgraphs.clone();

        let subgraph_url = Self::subgraph_url(&subgraphs_server);
        let subgraph_overrides = self
            .load_subgraph_mocks(&mut subgraphs_server, &subgraph_url, path, out)
            .await;
        self.subgraphs_server = Some(subgraphs_server);

        let router = match self.router.as_mut() {
            None => {
                writeln!(
                    out,
                    "cannot reload subgraph overrides: router was not started"
                )
                .unwrap();
                return Err(out.into());
            }
            Some(router) => router,
        };

        if update_url_overrides {
            router.update_subgraph_overrides(subgraph_overrides);
            router.touch_config().await;
            router.assert_reloaded().await;
        }

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

    #[allow(clippy::too_many_arguments)]
    async fn request(
        &mut self,
        description: Option<String>,
        mut request: Value,
        query_path: Option<&str>,
        headers: &HashMap<String, String>,
        expected_response: &Value,
        expected_headers: &HashMap<String, String>,
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

        writeln!(out).unwrap();
        if let Some(description) = description {
            writeln!(out, "description: {description}").unwrap();
        }

        writeln!(out, "query: {}\n", serde_json::to_string(&request).unwrap()).unwrap();
        writeln!(out, "header: {headers:?}\n").unwrap();

        let (_, response) = router
            .execute_query(
                Query::builder()
                    .body(request)
                    .headers(headers.clone())
                    .build(),
            )
            .await;
        writeln!(out, "response headers: {:?}", response.headers()).unwrap();

        let mut failed = false;
        for (key, value) in expected_headers {
            if !response.headers().contains_key(key) {
                failed = true;
                writeln!(out, "expected header {key} to be present").unwrap();
            } else if response.headers().get(key).unwrap() != value {
                failed = true;
                writeln!(
                    out,
                    "expected header {} to be {}, got {:?}",
                    key,
                    value,
                    response.headers().get(key).unwrap()
                )
                .unwrap();
            }
        }
        if failed {
            self.print_received_requests(out).await;
            let f: Failed = out.clone().into();
            return Err(f);
        }

        let content_type = response
            .headers()
            .get("content-type")
            .unwrap()
            .to_str()
            .unwrap();
        let mut is_multipart = false;
        let mut boundary = None;
        for mime in MediaTypeList::new(content_type).flatten() {
            if mime.ty == mediatype::names::MULTIPART && mime.subty == mediatype::names::MIXED {
                is_multipart = true;
                boundary = mime.get_param(mediatype::names::BOUNDARY).map(|v| {
                    // multer does not strip quotes from the boundary: https://github.com/rwf2/multer/issues/64
                    let mut s = v.as_str();
                    if s.starts_with('\"') && s.ends_with('\"') {
                        s = &s[1..s.len() - 1];
                    }

                    s.to_string()
                });
            }
        }

        let graphql_response: Value = if !is_multipart {
            let body = response.bytes().await.map_err(|e| {
                writeln!(out, "could not get graphql response data: {e}").unwrap();
                let f: Failed = out.clone().into();
                f
            })?;
            serde_json::from_slice(&body).map_err(|e| {
                writeln!(
                    out,
                    "could not deserialize graphql response data: {e}\nfrom:\n{}",
                    std::str::from_utf8(&body).unwrap()
                )
                .unwrap();
                let f: Failed = out.clone().into();
                f
            })?
        } else {
            let mut chunks = Vec::new();

            let mut multipart = Multipart::new(response.bytes_stream(), boundary.unwrap());

            // Iterate over the fields, use `next_field()` to get the next field.
            while let Some(mut field) = multipart.next_field().await.map_err(|e| {
                writeln!(out, "could not get next field from multipart body: {e}",).unwrap();
                let f: Failed = out.clone().into();
                f
            })? {
                while let Some(chunk) = field.chunk().await.map_err(|e| {
                    writeln!(out, "could not get next chunk from multipart body: {e}",).unwrap();
                    let f: Failed = out.clone().into();
                    f
                })? {
                    writeln!(out, "multipart chunk: {:?}\n", std::str::from_utf8(&chunk)).unwrap();

                    let parsed: Value = serde_json::from_slice(&chunk).map_err(|e| {
                        writeln!(
                            out,
                            "could not deserialize graphql response data: {e}\nfrom:\n{}",
                            std::str::from_utf8(&chunk).unwrap()
                        )
                        .unwrap();
                        let f: Failed = out.clone().into();
                        f
                    })?;
                    chunks.push(parsed);
                }
            }
            Value::Array(chunks)
        };

        if expected_response != &graphql_response {
            self.print_received_requests(out).await;

            writeln!(out, "assertion `left == right` failed").unwrap();
            writeln!(
                out,
                "expected: {}",
                serde_json::to_string(&expected_response).unwrap()
            )
            .unwrap();
            writeln!(
                out,
                "received: {}",
                serde_json::to_string(&graphql_response).unwrap()
            )
            .unwrap();
            return Err(out.into());
        }

        Ok(())
    }

    async fn print_received_requests(&mut self, out: &mut String) {
        if let Some(requests) = self
            .subgraphs_server
            .as_ref()
            .unwrap()
            .received_requests()
            .await
        {
            writeln!(out, "subgraphs received requests:").unwrap();
            for request in requests {
                writeln!(out, "\tmethod: {}", request.method).unwrap();
                writeln!(out, "\tpath: {}", request.url).unwrap();
                writeln!(out, "\t{}\n", std::str::from_utf8(&request.body).unwrap()).unwrap();
            }
        } else {
            writeln!(out, "subgraphs received no requests").unwrap();
        }
    }

    async fn endpoint_request(
        &mut self,
        url: &url::Url,
        request: HttpRequest,
        out: &mut String,
    ) -> Result<(), Failed> {
        let client = reqwest::Client::new();

        let mut builder = client.request(
            request
                .method
                .as_deref()
                .unwrap_or("POST")
                .try_into()
                .unwrap(),
            url.clone(),
        );
        for (name, value) in request.headers {
            builder = builder.header(name, value);
        }

        let request = builder.json(&request.body).build().unwrap();
        let response = client.execute(request).await.map_err(|e| {
            writeln!(
                out,
                "could not send request to Router endpoint at {url}: {e}"
            )
            .unwrap();
            let f: Failed = out.clone().into();
            f
        })?;

        writeln!(out, "Endpoint returned: {response:?}").unwrap();

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
        writeln!(out, "could not read file at path: '{path:?}': {e}").unwrap();
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
#[allow(dead_code)]
#[serde(deny_unknown_fields)]
struct Plan {
    #[serde(default)]
    enterprise: bool,
    #[serde(default)]
    redis: bool,
    #[serde(default)]
    snapshot: bool,
    actions: Vec<Action>,
}

#[derive(Deserialize)]
#[serde(tag = "type", deny_unknown_fields)]
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
    ReloadSubgraphs {
        subgraphs: HashMap<String, Subgraph>,
        // set to true if subgraph URL overrides should be updated (ex: a new subgraph is added)
        #[serde(default)]
        update_url_overrides: bool,
    },
    Request {
        description: Option<String>,
        request: Value,
        query_path: Option<String>,
        #[serde(default)]
        headers: HashMap<String, String>,
        expected_response: Value,
        #[serde(default)]
        expected_headers: HashMap<String, String>,
    },
    EndpointRequest {
        url: url::Url,
        request: HttpRequest,
    },
    Stop,
}

#[derive(Clone, Debug, Deserialize)]
#[cfg_attr(not(feature = "snapshot"), allow(dead_code))]
struct Snapshot {
    path: String,
    base_url: String,
    update: Option<bool>,
    port: Option<u16>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Subgraph {
    snapshot: Option<Snapshot>,
    requests: Option<Vec<SubgraphRequestMock>>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SubgraphRequestMock {
    request: HttpRequest,
    response: HttpResponse,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct HttpRequest {
    method: Option<String>,
    path: Option<String>,
    #[serde(default)]
    headers: HashMap<String, String>,
    body: Option<Value>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct HttpResponse {
    status: Option<u16>,
    #[serde(default)]
    headers: HashMap<String, String>,
    body: Value,
}
