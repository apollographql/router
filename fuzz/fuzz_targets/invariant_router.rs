//! Fuzz target to generate random invalid body and query to the router and check it doesn't panic
#![allow(unused_attributes)]
#![no_main]

use std::char::REPLACEMENT_CHARACTER;
use std::env;
use std::process::Child;
use std::process::Command;
use std::process::Stdio;
use std::sync::atomic::AtomicBool;
use std::sync::OnceLock;
use std::time::Duration;

use libfuzzer_sys::fuzz_target;
use serde_json::json;

const ROUTER_CMD: &str = "router";
const ROUTER_CONFIG_PATH: &str = "./examples/graphql/local.graphql";
const ROUTER_URL: &str = "http://localhost:4000";
static ROUTER_INIT: AtomicBool = AtomicBool::new(false);

static ROUTER_PROCESS: OnceLock<ChildProcessGuard> = OnceLock::new();

#[derive(Debug)]
struct ChildProcessGuard(Child);
impl Drop for ChildProcessGuard {
    fn drop(&mut self) {
        if let Err(e) = self.0.kill() {
            eprintln!("Could not kill child process: {}", e);
        }
    }
}

fuzz_target!(|data: &[u8]| {
    let _ = env_logger::try_init();

    if !ROUTER_INIT.swap(true, std::sync::atomic::Ordering::Relaxed) {
        let mut cmd =
            Command::new(env::var("ROUTER_CMD").unwrap_or_else(|_| ROUTER_CMD.to_string()))
                .arg("--supergraph")
                .arg(
                    env::var("ROUTER_CONFIG_PATH")
                        .unwrap_or_else(|_| ROUTER_CONFIG_PATH.to_string()),
                )
                .arg("--hot-reload")
                .stdout(Stdio::inherit())
                .stderr(Stdio::inherit())
                .spawn()
                .expect("cannot launch the router");

        std::thread::sleep(Duration::from_secs(3));
        if let Ok(Some(exit_status)) = cmd.try_wait() {
            panic!("the router exited with exit code : {}", exit_status);
        }
        ROUTER_PROCESS
            .set(ChildProcessGuard(cmd))
            .expect("cannot set the router child process");
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
