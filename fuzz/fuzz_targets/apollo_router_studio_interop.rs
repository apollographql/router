#![no_main]

use std::env;
use std::process::Child;
use std::process::Command;
use std::process::Stdio;
use std::sync::atomic::AtomicBool;
use std::sync::OnceLock;

use apollo_compiler::ExecutableDocument;
use apollo_compiler::Schema;
#[path = "../../apollo-router/src/apollo_studio_interop/mod.rs"]
mod apollo_router_usage_reporting;
use apollo_router_usage_reporting::generate_usage_reporting;
use apollo_router_usage_reporting::UsageReportingComparisonResult;
use libfuzzer_sys::fuzz_target;
use router_bridge::planner::UsageReporting;
use router_fuzz::generate_valid_operation;
use serde_json::json;

const ROUTER_CMD: &str = "./target/debug/examples/usage_reporting_router";
// const SCHEMA_PATH: &str = "fuzz/supergraph.graphql";
// const SCHEMA_PATH: &str = "fuzz/supergraph-fed2.graphql";
// This schema contains more types and fields and directive so we can test as much of signature and referenced field
// generation as possible. apollo_smith doesn't support random generation of input objects, union types, etc so it's
// still not comprehensive.
const SCHEMA_PATH: &str = "fuzz/supergraph-moretypes.graphql";
const ROUTER_CONFIG_PATH: &str = "fuzz/router.yaml";
const ROUTER_URL: &str = "http://localhost:4100";
static ROUTER_INIT: AtomicBool = AtomicBool::new(false);

static mut ROUTER_PROCESS: OnceLock<ChildProcessGuard> = OnceLock::new();

#[derive(Debug)]
struct ChildProcessGuard(Child);
impl Drop for ChildProcessGuard {
    fn drop(&mut self) {
        if let Err(e) = self.0.kill() {
            eprintln!("Could not kill child process: {}", e);
        }
    }
}

/*
Ideally this fuzzer would just call the router-bridge's Planner.plan function directly instead of spinning up a new
router executable, but when we tried to do that, we ran into some very confusing serialization issues. The running
theory is that the fuzzer runs a couple of sanitizers / custom flags, which deno was not happy with. We work around
this by spawning a router in a separate process and sending requests to the router instead. The usage_reporting
payload is not usually exposed from router responses, so we have to use a plugin to extract it. This was done as an
example so we could avoid polluting the main fuzzer dependencies.

To run this fuzzer:
* if this is the first time running it, or you've made changes to router code
  * go to the /fuzz directory (you need to be there because fuzz is not in the workspace)
  * run `cargo build --example usage_reporting_router`
* start the fuzzer using `cargo +nightly fuzz run apollo_router_studio_interop` from the root directory
  * if you get an Address already in use error, make sure you `killall usage_reporting_router` before a new run
*/

fuzz_target!(|data: &[u8]| {
    let _ = env_logger::try_init();

    if !ROUTER_INIT.swap(true, std::sync::atomic::Ordering::Relaxed) {
        let mut cmd =
            Command::new(env::var("ROUTER_CMD").unwrap_or_else(|_| ROUTER_CMD.to_string()))
                .arg("--supergraph")
                .arg(
                    env::var("ROUTER_SCHEMA_PATH")
                        .unwrap_or_else(|_| SCHEMA_PATH.to_string()),
                ).arg("--config")
                .arg(
                    env::var("ROUTER_CONFIG_PATH")
                        .unwrap_or_else(|_| ROUTER_CONFIG_PATH.to_string()),
                )
                .arg("--hot-reload")
                .stdout(Stdio::inherit())
                .stderr(Stdio::inherit())
                .spawn()
                .expect("cannot launch the router\nThe fuzzer cannot work unless you run `cargo build --example usage_reporting_router` in the `fuzz` directory.\nDid you forget to run cargo build before you run the fuzzer?");

        println!("waiting for router to start up");
        std::thread::sleep(std::time::Duration::from_secs(5));
        if let Ok(Some(exit_status)) = cmd.try_wait() {
            panic!("the router exited with exit code : {}", exit_status);
        }
        unsafe { ROUTER_PROCESS.set(ChildProcessGuard(cmd)) }
            .expect("cannot set the router child process");
    }

    let (op_str, schema_str) = match generate_valid_operation(data, SCHEMA_PATH) {
        Ok(d) => (d.0, d.1),
        Err(_err) => {
            println!("Failed to generate valid operation");
            return;
        }
    };

    // If the generated schema or operation doesn't pass validation, the call to the router will
    // fail, so we don't want to continue with the test.
    let schema = match Schema::parse_and_validate(schema_str, "schema.graphql") {
        Ok(s) => s,
        Err(_err) => {
            // println!("Failed to parse and validate schema");
            return;
        }
    };
    let doc = match ExecutableDocument::parse_and_validate(&schema, &op_str, "query.graphql") {
        Ok(d) => d,
        Err(_err) => {
            // println!("Failed to parse and validate operation");
            return;
        }
    };

    let rust_generated = generate_usage_reporting(&doc, &doc, &None, &schema);

    let http_client = reqwest::blocking::Client::new();
    let router_response = http_client
        .post(ROUTER_URL)
        .json(&json!({
            "query": op_str
        }))
        .send();
    if let Err(err) = router_response {
        println!("Bad response from router: [{err}] for operation: [{op_str:?}]");
        unsafe { ROUTER_PROCESS.get_mut() }
            .unwrap()
            .0
            .kill()
            .unwrap();
        panic!("{}", err);
    }

    let response: serde_json::Value = router_response.unwrap().json().unwrap();

    let bridge_generated: UsageReporting = serde_json::from_value(
        response
            .get("extensions")
            .unwrap()
            .as_object()
            .unwrap()
            .get("usageReporting")
            .unwrap()
            .clone(),
    )
    .unwrap();

    if !matches!(
        rust_generated.compare(&bridge_generated),
        UsageReportingComparisonResult::Equal
    ) {
        unsafe { ROUTER_PROCESS.get_mut() }
            .unwrap()
            .0
            .kill()
            .unwrap();
        panic!(
            "New rust implementation:\n{:?}\nExisting router-bridge implementation:\n{:?}",
            rust_generated.result, bridge_generated
        );
    }
});
