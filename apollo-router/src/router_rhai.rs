use std::str::FromStr;
use std::sync::Arc;
use std::sync::Mutex;

use crate::plugin::DynPlugin;
use crate::plugins::rhai::Rhai;
use crate::services::subgraph;

use anyhow::Result;
use serde_json::Value;

pub(crate) async fn base_process_function<S: AsRef<str>>(
    fn_name: S,
) -> Result<(), Box<rhai::EvalAltResult>> {
    let dyn_plugin: Box<dyn DynPlugin> = crate::plugin::plugins()
        .find(|factory| factory.name == "apollo.rhai")
        .expect("Plugin not found")
        .create_instance_without_schema(
            &Value::from_str(
                r#"{"scripts":"apollo-router/tests/fixtures", "main":"request_response_test.rhai"}"#,
            )
            .unwrap(),
        )
        .await
        .unwrap();

    // Downcast our generic plugin. We know it must be Rhai
    let it: &dyn std::any::Any = dyn_plugin.as_any();
    let rhai_instance: &Rhai = it.downcast_ref::<Rhai>().expect("downcast");

    let block = rhai_instance.block.load();

    // Get a scope to use for our test
    let scope = block.scope.clone();

    let mut guard = scope.lock().unwrap();

    let fake_response = subgraph::Response::fake_builder().build();
    /*
    println!("fake: {:?}", fake_response);
    let j =
        serde_json::to_string_pretty(&fake_response.response.body()).map_err(|e| e.to_string())?;
    println!("j: {}", j);
    */
    // We must wrap our canned response in Arc<Mutex<Option<>>> to keep the rhai runtime
    // happy
    let response = Arc::new(Mutex::new(Some(fake_response)));

    // Call our rhai test function.
    let output = block
        .engine
        .call_fn(&mut guard, &block.ast, fn_name, (response,));
    println!("output: {:?}", output);
    output
}
