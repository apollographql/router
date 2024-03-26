#![no_main]

use apollo_compiler::ExecutableDocument;
use apollo_compiler::Schema;
use apollo_router_studio_interop::generate_usage_reporting;
//use apollo_router_studio_interop::compare_ref_fields_by_type;
use libfuzzer_sys::fuzz_target;
use router_bridge::planner::UsageReporting;
use router_fuzz::generate_valid_operation;
use serde_json::json;
use std::env;
use std::process::Child;
use std::process::Command;
use std::process::Stdio;
use std::sync::atomic::AtomicBool;
use std::sync::OnceLock;

const ROUTER_CMD: &str = "./target/debug/examples/fuzz_target";
const ROUTER_SCHEMA_PATH: &str = "fuzz/supergraph-fed2.graphql";
const ROUTER_CONFIG_PATH: &str = "fuzz/router.yaml";
const ROUTER_URL: &str = "http://localhost:4100";
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
                    env::var("ROUTER_SCHEMA_PATH")
                        .unwrap_or_else(|_| ROUTER_SCHEMA_PATH.to_string()),
                ).arg("--config")
                .arg(
                    env::var("ROUTER_CONFIG_PATH")
                        .unwrap_or_else(|_| ROUTER_CONFIG_PATH.to_string()),
                )
                .arg("--hot-reload")
                .stdout(Stdio::inherit())
                .stderr(Stdio::inherit())
                .spawn()
                .expect("cannot launch the router\nThe fuzzer cannot work unless you run `cargo build --example fuzz_target` in the `fuzz` directory.\nDid you forget to run cargo build before you run the fuzzer?");
        // if you get an Address already in use error, make sure you `killall fuzz_target` before a new run

        println!("waiting for router to start up");
        std::thread::sleep(std::time::Duration::from_secs(5));
        if let Ok(Some(exit_status)) = cmd.try_wait() {
            panic!("the router exited with exit code : {}", exit_status);
        }
        ROUTER_PROCESS
            .set(ChildProcessGuard(cmd))
            .expect("cannot set the router child process");
    }

    let (op_str, schema_str) = match generate_valid_operation(data, "fuzz/supergraph-fed2.graphql")
    {
        Ok(d) => (d.0, d.1),
        Err(_err) => {
            return;
        }
    };

    // println!("======= op =======");
    // println!("{}", &op_str);
    // println!("========================");
    // println!("======= schema =======");
    // println!("{}", &schema_str);
    // println!("========================");

    let schema = Schema::parse_and_validate(&schema_str, "schema.graphql").unwrap();
    let doc = ExecutableDocument::parse(&schema, &op_str, "query.graphql").unwrap();

    let generated = generate_usage_reporting(&doc, &doc, &None, &schema);

    // println!("======= RUST SIGNATURE =======");
    // println!("{}", generated.result.stats_report_key);
    // println!("========================");

    // println!("======= RUST REFERENCED FIELDS =======");
    // println!("{:?}", generated.result.referenced_fields_by_type);
    // println!("========================");

    let http_client = reqwest::blocking::Client::new();
    let router_response = http_client
        .post(ROUTER_URL)
        .json(&json!({
            "query": op_str
        }))
        .send();
    if let Err(err) = router_response {
        eprintln!("bad response from router: {op_str:?}");
        panic!("{}", err);
    }

    let response: serde_json::Value = router_response.unwrap().json().unwrap();

    let usage_reporting: UsageReporting = serde_json::from_value(
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

    assert_eq!(
        generated.result.stats_report_key,
        usage_reporting.stats_report_key
    );
    assert!(
        generated.compare_referenced_fields(&usage_reporting.referenced_fields_by_type),
        "generated\n{:?}\nand router's\n{:?}",
        generated.result,
        usage_reporting
    );
});
