#![no_main]

use std::fs::OpenOptions;
use std::io::Write;

use libfuzzer_sys::fuzz_target;
use log::debug;
use router_fuzz::generate_valid_operation;
use serde_json::json;
use serde_json::Value;

const SUBGRAPH_URL: &str = "http://localhost:4005";
const ROUTER_URL: &str = "http://localhost:4000";

fuzz_target!(|data: &[u8]| {
    let generated_operation = match generate_valid_operation(data, "fuzz/subgraph/api.graphql") {
        Ok((d, _)) => d,
        Err(_err) => {
            return;
        }
    };

    let http_client = reqwest::blocking::Client::new();
    let router_response = http_client
        .post(ROUTER_URL)
        .json(&json!({ "query": generated_operation }))
        .send()
        .unwrap()
        .json::<Value>();
    let subgraph_response = http_client
        .post(SUBGRAPH_URL)
        .json(&json!({ "query": generated_operation }))
        .send()
        .unwrap()
        .json::<Value>();

    debug!("======= OPERATION ======");
    debug!("{}", generated_operation);
    debug!("========================");
    debug!("======= RESPONSE =======");
    if router_response.is_ok() != subgraph_response.is_ok() {
        let router_error = if let Err(err) = &router_response {
            Some(err)
        } else {
            None
        };
        let subgraph_error = if let Err(err) = &subgraph_response {
            Some(err)
        } else {
            None
        };
        if router_error.is_some() && subgraph_error.is_some() {
            // Do not check errors for now
            return;
        }

        let mut file = OpenOptions::new()
            .read(true)
            .create(true)
            .append(true)
            .open("router_errors.txt")
            .unwrap();

        let errors = format!(
            r#"


====DOCUMENT===
{generated_operation}

====SUBGRAPH====
{subgraph_error:?}

====ROUTER====
{router_error:?}


"#
        );
        debug!("{errors}");
        file.write_all(errors.as_bytes()).unwrap();
        file.flush().unwrap();

        panic!()
    } else if router_response.is_ok() {
        let subgraph_errors_detected = subgraph_response
            .as_ref()
            .unwrap()
            .as_object()
            .unwrap()
            .get("errors")
            .map(|e| !e.as_array().unwrap().len())
            .unwrap_or(0);
        let router_errors_detected = router_response
            .as_ref()
            .unwrap()
            .as_object()
            .unwrap()
            .get("errors")
            .map(|e| !e.as_array().unwrap().len())
            .unwrap_or(0);
        if subgraph_errors_detected > 0 && router_errors_detected > 0 {
            // Do not check the shape of errors right now
            return;
        }
        let router_response = serde_json::to_string_pretty(&router_response.unwrap()).unwrap();
        let subgraph_response = serde_json::to_string_pretty(&subgraph_response.unwrap()).unwrap();
        if router_response != subgraph_response {
            let mut file = OpenOptions::new()
                .read(true)
                .create(true)
                .append(true)
                .open("router_errors.txt")
                .unwrap();

            let errors = format!(
                r#"


====DOCUMENT===
{generated_operation}

====SUBGRAPH====
{subgraph_response}

====ROUTER====
{router_response}


"#
            );
            debug!("ERRORS: {errors}");
            file.write_all(errors.as_bytes()).unwrap();
            file.flush().unwrap();

            panic!();
        }
    }
    debug!("========================");
});
