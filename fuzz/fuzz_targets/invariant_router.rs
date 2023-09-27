//! Fuzz target to generate random invalid query and detect if the gateway and the router are both throwing errors
#![no_main]

use std::char::REPLACEMENT_CHARACTER;
use std::ffi::OsString;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use apollo_router::Configuration;
use apollo_router::ConfigurationSource;
use apollo_router::Executable;
use apollo_router::Opt;
use apollo_router::SchemaSource;
use clap::Parser;
use libfuzzer_sys::fuzz_target;
use serde_json::json;

const ROUTER_URL: &str = "http://localhost:4000";
static ROUTER_INIT: AtomicBool = AtomicBool::new(false);

fuzz_target!(|data: &[u8]| {
    let _ = env_logger::try_init();

    log::info!("start");

    if !ROUTER_INIT.swap(true, std::sync::atomic::Ordering::Relaxed) {
        log::info!("first {:?}", std::env::args_os());

        std::thread::spawn(|| {
            let mut builder = tokio::runtime::Builder::new_multi_thread();
            builder.enable_all();
            let runtime = builder.build()?;
            let schema_sdl = include_str!("../../examples/graphql/local.graphql").to_string();
            runtime.block_on(async move {
                let configuration = Configuration::default();
                let cli_args = Opt::parse_from(Vec::<OsString>::new());
                log::info!("coucou");
                Executable::builder()
                    .config(ConfigurationSource::Static(Box::new(configuration)))
                    .schema(SchemaSource::Static { schema_sdl })
                    .cli_args(cli_args)
                    .start()
                    .await
            })
        });
        std::thread::sleep(Duration::from_secs(1));
    }

    let query = String::from_utf8_lossy(data).replace(REPLACEMENT_CHARACTER, "");
    let http_client = reqwest::blocking::Client::new();
    let _router_response = http_client
        .post(ROUTER_URL)
        .json(&json!({ "query": query }))
        .send()
        .unwrap();
});
