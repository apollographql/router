#![no_main]

use std::fs::OpenOptions;
use std::io::Write;

use libfuzzer_sys::fuzz_target;
use log::debug;
use router_fuzz::generate_valid_operation;
use serde_json::json;
use serde_json::Value;

const GATEWAY_URL: &str = "http://localhost:4100/graphql";
const ROUTER_URL: &str = "http://localhost:4000";

fuzz_target!(|data: &[u8]| {
    let generated_operation = match generate_valid_operation(data, "fuzz/supergraph-fed2.graphql") {
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
    let gateway_response = http_client
        .post(GATEWAY_URL)
        .json(&json!({ "query": generated_operation }))
        .send()
        .unwrap()
        .json::<Value>();

    debug!("======= DOCUMENT =======");
    debug!("{}", generated_operation);
    debug!("========================");
    debug!("======= RESPONSE =======");
    if router_response.is_ok() != gateway_response.is_ok() {
        let router_error = if let Err(err) = &router_response {
            Some(err)
        } else {
            None
        };
        let gateway_error = if let Err(err) = &gateway_response {
            Some(err)
        } else {
            None
        };
        if router_error.is_some() && gateway_error.is_some() {
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

====GATEWAY====
{gateway_error:?}

====ROUTER====
{router_error:?}


"#
        );
        debug!("{errors}");
        file.write_all(errors.as_bytes()).unwrap();
        file.flush().unwrap();

        panic!()
    } else if router_response.is_ok() {
        let gateway_errors_detected = gateway_response
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
        if gateway_errors_detected > 0 && gateway_errors_detected == router_errors_detected {
            // Do not check the shape of errors right now
            return;
        }
        let router_response = serde_json::to_string_pretty(&router_response.unwrap()).unwrap();
        let gateway_response = serde_json::to_string_pretty(&gateway_response.unwrap()).unwrap();
        if router_response != gateway_response {
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

====GATEWAY====
{gateway_response}

====ROUTER====
{router_response}


"#
            );
            debug!("{errors}");
            file.write_all(errors.as_bytes()).unwrap();
            file.flush().unwrap();

            panic!();
        }
    }
    debug!("========================");
});
