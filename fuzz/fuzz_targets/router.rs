#![no_main]
use apollo_router_core::Response;
use libfuzzer_sys::fuzz_target;
use log::debug;
use router_fuzz::generate_valid_operation;
use serde_json::json;
use std::fs::OpenOptions;
use std::io::Write;

const GATEWAY_URL: &str = "http://localhost:4100/graphql";
const ROUTER_URL: &str = "http://localhost:4000/graphql";

fuzz_target!(|data: &[u8]| {
    let generated_operation = match generate_valid_operation(data) {
        Ok(d) => d,
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
        .json::<Response>();
    let gateway_response = http_client
        .post(GATEWAY_URL)
        .json(&json!({ "query": generated_operation }))
        .send()
        .unwrap()
        .json::<Response>();

    debug!("======= DOCUMENT =======");
    debug!("{}", generated_operation);
    debug!("========================");
    debug!("======= RESPONSE =======");
    assert_eq!(router_response.is_ok(), gateway_response.is_ok());
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
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
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
    } else if router_response.is_ok() {
        let router_response = serde_json::to_string_pretty(&router_response.unwrap()).unwrap();
        let gateway_response = serde_json::to_string_pretty(&gateway_response.unwrap()).unwrap();
        if router_response != gateway_response {
            let mut file = OpenOptions::new()
                .read(true)
                .write(true)
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
        }
    }
    debug!("========================");
});
