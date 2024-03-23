#![no_main]

use apollo_compiler::ExecutableDocument;
use apollo_compiler::Schema;
use apollo_router_studio_interop::generate_usage_reporting;
//use apollo_router_studio_interop::compare_ref_fields_by_type;
use libfuzzer_sys::fuzz_target;
// use log::debug;
use router_bridge::planner::{PlanOptions, Planner, QueryPlannerConfig};
use router_fuzz::generate_valid_operation;
use tokio::runtime::Runtime;

fuzz_target!(|data: &[u8]| {
    let (op_str, schema_str) = match generate_valid_operation(data, "fuzz/supergraph-fed2.graphql")
    {
        Ok(d) => (d.0, d.1),
        Err(_err) => {
            return;
        }
    };

    println!("======= op =======");
    println!("{}", &op_str);
    println!("========================");
    println!("======= schema =======");
    println!("{}", &schema_str);
    println!("========================");

    let schema = Schema::parse_and_validate(&schema_str, "schema.graphql").unwrap();
    let doc = ExecutableDocument::parse(&schema, &op_str, "query.graphql").unwrap();

    let generated = generate_usage_reporting(&doc, &doc, &None, &schema);

    println!("======= RUST SIGNATURE =======");
    println!("{}", generated.result.stats_report_key);
    println!("========================");

    println!("======= RUST REFERENCED FIELDS =======");
    println!("{:?}", generated.result.referenced_fields_by_type);
    println!("========================");

    let runtime = Runtime::new().unwrap();
    let planner = runtime.block_on(planner(&schema_str));
    let js_sig = runtime.block_on(generate(planner, &op_str));

    println!("======= RUST SIGNATURE =======");
    println!("{}", js_sig);
    println!("========================");

    panic!();
});

async fn planner(ts: &str) -> Planner<serde_json::Value> {
    let result =
        Planner::<serde_json::Value>::new(ts.to_string(), QueryPlannerConfig::default()).await;
    println!("======= got past Planner::new =======");

    // todo better
    match result {
        Ok(planner) => planner,
        Err(err) => {
            println!("======= PLANNER ERROR =======");
            println!("{:?}", err);
            println!("========================");
            panic!()
        }
    }
}

async fn generate(planner: Planner<serde_json::Value>, op: &str) -> String {
    let maybe_plan = planner
        .plan(op.to_string(), None, PlanOptions::default())
        .await;

    // todo better
    match maybe_plan {
        Ok(result) => result.usage_reporting.stats_report_key,
        Err(err) => {
            println!("======= ERROR =======");
            println!("{}", err);
            println!("========================");
            "".into()
        }
    }
}
