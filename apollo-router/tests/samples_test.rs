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

use libtest_mimic::{Arguments, Failed, Trial};
use serde::Deserialize;
use serde_json::Value;
use tokio::runtime::Runtime;

#[path = "./common.rs"]
pub(crate) mod common;
pub(crate) use common::IntegrationTest;
use wiremock::matchers::body_partial_json;
use wiremock::Mock;
use wiremock::ResponseTemplate;

fn main() -> Result<ExitCode, Box<dyn Error>> {
    let args = Arguments::from_args();
    let mut tests = Vec::new();
    let path = env::current_dir()?.join("tests/samples");

    for entry in fs::read_dir(path)? {
        let entry = entry?;

        // Handle files
        let path = entry.path();
        let name = path.file_name().unwrap().to_str().unwrap().to_string();
        tests.push(Trial::test(name, move || test(&path)));
        //tests.push(test_from_path(&path));
        /*if file_type.is_file() {
            if path.extension() == Some(OsStr::new("rs")) {
                let name = path
                    .strip_prefix(env::current_dir()?)?
                    .display()
                    .to_string();

                let test = Trial::test(name, move || check_file(&path)).with_kind("tidy");
                tests.push(test);
            }
        } else if file_type.is_dir() {
            // Handle directories
            visit_dir(&path, tests)?;
        }*/
    }

    //let tests = collect_tests()?;
    Ok(libtest_mimic::run(&args, tests).exit_code())
}

fn test(path: &PathBuf) -> Result<(), Failed> {
    //libtest_mimic does not support stdout capture
    let mut out = String::new();
    writeln!(&mut out, "test at path: {path:?}").unwrap();
    if let Ok(file) = open_file(&path.join("README.md"), &mut out) {
        writeln!(&mut out, "{file}\n\n============\n\n").unwrap();
    }
    let query: Value = match serde_json::from_str(&open_file(&path.join("query.json"), &mut out)?) {
        Ok(data) => data,
        Err(e) => {
            writeln!(&mut out, "could not deserialize subgraph responses: {e}").unwrap();
            return Err(out.into());
        }
    };
    let expected_response: Value =
        match serde_json::from_str(&open_file(&path.join("response.json"), &mut out)?) {
            Ok(data) => data,
            Err(e) => {
                writeln!(&mut out, "could not deserialize subgraph responses: {e}").unwrap();
                return Err(out.into());
            }
        };
    let subgraphs: HashMap<String, Subgraph> =
        match serde_json::from_str(&open_file(&path.join("subgraphs.json"), &mut out)?) {
            Ok(data) => data,
            Err(e) => {
                writeln!(&mut out, "could not deserialize subgraph responses: {e}").unwrap();
                return Err(out.into());
            }
        };

    let config = open_file(&path.join("configuration.yaml"), &mut out)?;

    let schema_path = path.join("supergraph.graphql");
    check_path(&schema_path, &mut out)?;

    let rt = Runtime::new()?;

    // Spawn the root task
    rt.block_on(async {
        let listener = TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0))).unwrap();
        let address = listener.local_addr().unwrap();
        let url = format!("http://{address}/");

        let subgraphs_server = wiremock::MockServer::builder()
            .listener(listener)
            .start()
            .await;
        let mut subgraph_overrides = HashMap::new();

        for (name, subgraph) in subgraphs {
            for SubgraphRequest { request, response } in subgraph.requests {
                Mock::given(body_partial_json(&request))
                    .respond_with(ResponseTemplate::new(200).set_body_json(response))
                    .mount(&subgraphs_server)
                    .await;
            }

            // Add a default override for products, if not specified
            subgraph_overrides.entry(name).or_insert(url.clone());
        }

        let mut router = IntegrationTest::builder()
            .config(&config)
            .supergraph(schema_path)
            .subgraph_overrides(subgraph_overrides)
            .build()
            .await;
        router.start().await;
        router.assert_started().await;

        writeln!(
            &mut out,
            "query: {}\n",
            serde_json::to_string(&query).unwrap()
        )
        .unwrap();
        let (_, response) = router.execute_query(&query).await;
        let body = response.bytes().await.map_err(|e| {
            writeln!(&mut out, "could not get graphql response data: {e}").unwrap();
            let f: Failed = out.clone().into();
            f
        })?;
        let graphql_response: Value = serde_json::from_slice(&body).map_err(|e| {
            writeln!(&mut out, "could not deserialize graphql response data: {e}").unwrap();
            let f: Failed = out.clone().into();
            f
        })?;

        if expected_response != graphql_response {
            writeln!(&mut out, "assertion `left == right` failed").unwrap();
            writeln!(
                &mut out,
                " left: {}",
                serde_json::to_string(&expected_response).unwrap()
            )
            .unwrap();
            writeln!(
                &mut out,
                "right: {}",
                serde_json::to_string(&graphql_response).unwrap()
            )
            .unwrap();
            return Err(out.into());
        }
        router.graceful_shutdown().await;

        Ok(())
    })
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
struct Subgraph {
    requests: Vec<SubgraphRequest>,
}

#[derive(Deserialize)]
struct SubgraphRequest {
    request: Value,
    response: Value,
}
