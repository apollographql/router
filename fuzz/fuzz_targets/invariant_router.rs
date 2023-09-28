//! Fuzz target to generate random invalid body and query to the router and check it doesn't panic
#![no_main]

use std::char::REPLACEMENT_CHARACTER;
use std::process::Command;
use std::process::Stdio;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use libfuzzer_sys::fuzz_target;
use serde_json::json;

const ROUTER_CMD: &str = "/home/bnj/rust/router/target/debug/router";
const ROUTER_CONFIG_PATH: &str = "./examples/graphql/local.graphql";
const ROUTER_URL: &str = "http://localhost:4000";
static ROUTER_INIT: AtomicBool = AtomicBool::new(false);

fuzz_target!(|data: &[u8]| {
    let _ = env_logger::try_init();

    log::info!("start");

    if !ROUTER_INIT.swap(true, std::sync::atomic::Ordering::Relaxed) {
        log::info!("first {:?}", std::env::args_os());

        let mut cmd = Command::new(ROUTER_CMD)
            .arg("--supergraph")
            .arg(ROUTER_CONFIG_PATH)
            .arg("--hot-reload")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("cannot launch the router");

        std::thread::sleep(Duration::from_secs(3));
        if let Ok(exit_status) = cmd.try_wait() {
            match exit_status {
                Some(exit_status) => panic!("the router exited with exit code : {}", exit_status),
                None => panic!("the router can't start"),
            }
        }
    }

    let query = data.to_vec();
    let http_client = reqwest::blocking::Client::new();
    let router_response = http_client.post(ROUTER_URL).body(query.clone()).send();
    if let Err(err) = router_response {
        eprintln!("bad body: {query:?}");
        panic!("{}", err);
    }
    let query = String::from_utf8_lossy(data).replace(REPLACEMENT_CHARACTER, "");
    let http_client = reqwest::blocking::Client::new();
    let router_response = http_client
        .post(ROUTER_URL)
        .json(&json!({"query": query}))
        .send();

    if let Err(err) = router_response {
        eprintln!("bad query: {query:?}");
        panic!("{}", err);
    }
});
