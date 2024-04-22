#![no_main]
use std::fs::OpenOptions;
use std::io::Write;

use libfuzzer_sys::fuzz_target;
use log::debug;
use router_fuzz::generate_valid_operation;
use serde_json::json;
use serde_json::Value;

const GATEWAY_FED1_URL: &str = "http://localhost:4100/graphql";
const GATEWAY_FED2_URL: &str = "http://localhost:4200/graphql";

fuzz_target!(|data: &[u8]| {
    let generated_operation = match generate_valid_operation(data, "fuzz/supergraph.graphql") {
        Ok((d, _)) => d,
        Err(_err) => {
            return;
        }
    };

    let http_client = reqwest::blocking::Client::new();
    let gateway_fed1_response = http_client
        .post(GATEWAY_FED1_URL)
        .json(&json!({ "query": generated_operation }))
        .send()
        .unwrap()
        .json::<Value>();
    let gateway_fed2_response = http_client
        .post(GATEWAY_FED2_URL)
        .json(&json!({ "query": generated_operation }))
        .send()
        .unwrap()
        .json::<Value>();

    debug!("======= DOCUMENT =======");
    debug!("{}", generated_operation);
    debug!("========================");
    debug!("======= RESPONSE =======");
    if gateway_fed1_response.is_ok() != gateway_fed2_response.is_ok() {
        let gateway_fed1_error = if let Err(err) = &gateway_fed1_response {
            Some(err)
        } else {
            None
        };
        let gateway_fed2_error = if let Err(err) = &gateway_fed2_response {
            Some(err)
        } else {
            None
        };
        if gateway_fed1_error.is_some() && gateway_fed2_error.is_some() {
            // Do not check errors for now
            return;
        }
        let mut file = OpenOptions::new()
            .read(true)
            .create(true)
            .append(true)
            .open("federation.txt")
            .unwrap();

        let errors = format!(
            r#"


====DOCUMENT===
{generated_operation}

====GATEWAY FED 1====
{gateway_fed1_error:?}

====GATEWAY FED 2====
{gateway_fed2_error:?}

"#
        );
        debug!("{errors}");
        file.write_all(errors.as_bytes()).unwrap();
        file.flush().unwrap();

        // panic!()
    } else if gateway_fed1_response.is_ok() {
        let gateway_fed2_errors_detected = gateway_fed2_response
            .as_ref()
            .unwrap()
            .as_object()
            .unwrap()
            .get("errors")
            .map(|e| !e.as_array().unwrap().len())
            .unwrap_or(0);
        let federation_detected = gateway_fed1_response
            .as_ref()
            .unwrap()
            .as_object()
            .unwrap()
            .get("errors")
            .map(|e| !e.as_array().unwrap().len())
            .unwrap_or(0);
        if gateway_fed2_errors_detected > 0 && gateway_fed2_errors_detected == federation_detected {
            // Do not check the shape of errors right now
            return;
        }
        let gateway_fed1_response =
            serde_json::to_string_pretty(&gateway_fed1_response.unwrap()).unwrap();
        let gateway_fed2_response =
            serde_json::to_string_pretty(&gateway_fed2_response.unwrap()).unwrap();
        if gateway_fed1_response != gateway_fed2_response {
            let mut file = OpenOptions::new()
                .read(true)
                .create(true)
                .append(true)
                .open("federation.txt")
                .unwrap();

            let errors = format!(
                r#"


====DOCUMENT===
{generated_operation}

====GATEWAY FED 1====
{gateway_fed1_response}

====GATEWAY FED 2====
{gateway_fed2_response}

"#
            );
            debug!("{errors}");
            file.write_all(errors.as_bytes()).unwrap();
            file.flush().unwrap();

            // panic!();
        }
    }
    debug!("========================");
});
