//! Rhai module tests.

use std::str::FromStr;
use std::sync::Arc;
use std::sync::Mutex;
use std::time::SystemTime;

use http::HeaderMap;
use http::Method;
use http::StatusCode;
use rhai::Engine;
use rhai::EvalAltResult;
use serde_json::Value;
use tower::util::BoxService;
use tower::BoxError;
use tower::Service;
use tower::ServiceExt;
use uuid::Uuid;

use super::process_error;
use super::subgraph;
use super::PathBuf;
use super::Rhai;
use crate::graphql::Error;
use crate::graphql::Request;
use crate::http_ext;
use crate::plugin::test::MockExecutionService;
use crate::plugin::test::MockSupergraphService;
use crate::plugin::DynPlugin;
use crate::plugins::rhai::engine::RhaiExecutionDeferredResponse;
use crate::plugins::rhai::engine::RhaiExecutionResponse;
use crate::plugins::rhai::engine::RhaiSupergraphDeferredResponse;
use crate::plugins::rhai::engine::RhaiSupergraphResponse;
use crate::services::ExecutionRequest;
use crate::services::SubgraphRequest;
use crate::services::SupergraphRequest;
use crate::services::SupergraphResponse;
use crate::Context;

#[tokio::test]
async fn rhai_plugin_supergraph_service() -> Result<(), BoxError> {
    let mut mock_service = MockSupergraphService::new();
    mock_service
        .expect_call()
        .times(1)
        .returning(move |req: SupergraphRequest| {
            Ok(SupergraphResponse::fake_builder()
                .header("x-custom-header", "CUSTOM_VALUE")
                .context(req.context)
                .build()
                .unwrap())
        });

    let dyn_plugin: Box<dyn DynPlugin> = crate::plugin::plugins()
        .find(|factory| factory.name == "apollo.rhai")
        .expect("Plugin not found")
        .create_instance_without_schema(
            &Value::from_str(r#"{"scripts":"tests/fixtures", "main":"test.rhai"}"#).unwrap(),
        )
        .await
        .unwrap();
    let mut router_service = dyn_plugin.supergraph_service(BoxService::new(mock_service));
    let context = Context::new();
    context.insert("test", 5i64).unwrap();
    let supergraph_req = SupergraphRequest::fake_builder().context(context).build()?;

    let mut supergraph_resp = router_service.ready().await?.call(supergraph_req).await?;
    assert_eq!(supergraph_resp.response.status(), 200);
    let headers = supergraph_resp.response.headers().clone();
    let context = supergraph_resp.context.clone();
    // Check if it fails
    let resp = supergraph_resp.next_response().await.unwrap();
    if !resp.errors.is_empty() {
        panic!(
            "Contains errors : {}",
            resp.errors
                .into_iter()
                .map(|err| err.to_string())
                .collect::<Vec<String>>()
                .join("\n")
        );
    }

    assert_eq!(headers.get("coucou").unwrap(), &"hello");
    assert_eq!(headers.get("coming_from_entries").unwrap(), &"value_15");
    assert_eq!(context.get::<_, i64>("test").unwrap().unwrap(), 42i64);
    assert_eq!(
        context.get::<_, String>("addition").unwrap().unwrap(),
        "Here is a new element in the context".to_string()
    );
    Ok(())
}

#[tokio::test]
async fn rhai_plugin_execution_service_error() -> Result<(), BoxError> {
    let mut mock_service = MockExecutionService::new();
    mock_service.expect_clone().return_once(move || {
        let mut mock_service = MockExecutionService::new();
        // The execution_service in test.rhai throws an exception, so we never
        // get a call into the mock service...
        mock_service.expect_call().never();
        mock_service
    });

    let dyn_plugin: Box<dyn DynPlugin> = crate::plugin::plugins()
        .find(|factory| factory.name == "apollo.rhai")
        .expect("Plugin not found")
        .create_instance_without_schema(
            &Value::from_str(r#"{"scripts":"tests/fixtures", "main":"test.rhai"}"#).unwrap(),
        )
        .await
        .unwrap();
    let mut router_service = dyn_plugin.execution_service(BoxService::new(mock_service));
    let fake_req = http_ext::Request::fake_builder()
        .header("x-custom-header", "CUSTOM_VALUE")
        .body(Request::builder().query(String::new()).build())
        .build()?;
    let context = Context::new();
    context.insert("test", 5i64).unwrap();
    let exec_req = ExecutionRequest::fake_builder()
        .context(context)
        .supergraph_request(fake_req)
        .build();

    let mut exec_resp = router_service
        .ready()
        .await
        .unwrap()
        .call(exec_req)
        .await
        .unwrap();
    assert_eq!(
        exec_resp.response.status(),
        http::StatusCode::INTERNAL_SERVER_ERROR
    );
    // Check if it fails
    let body = exec_resp.next_response().await.unwrap();
    if body.errors.is_empty() {
        panic!(
            "Must contain errors : {}",
            body.errors
                .into_iter()
                .map(|err| err.to_string())
                .collect::<Vec<String>>()
                .join("\n")
        );
    }

    assert_eq!(
        body.errors.get(0).unwrap().message.as_str(),
        "rhai execution error: 'Runtime error: An error occured (line 30, position 5)\nin call to function 'execution_request''"
    );
    Ok(())
}

// A Rhai engine suitable for minimal testing. There are no scripts and the SDL is an empty
// string.
fn new_rhai_test_engine() -> Engine {
    Rhai::new_rhai_engine(None, "".to_string(), PathBuf::new())
}

// Some of these tests rely extensively on internal implementation details of the tracing_test crate.
// These are unstable, so these test may break if the tracing_test crate is updated.
//
// This is done to avoid using the public interface of tracing_test which installs a global
// subscriber which breaks other tests in our stack which also insert a global subscriber.
// (there can be only one...) which means we cannot test it with #[tokio::test(flavor = "multi_thread")]
#[test]
fn it_logs_messages() {
    let env_filter = "apollo_router=trace";
    let mock_writer = tracing_test::internal::MockWriter::new(&tracing_test::internal::GLOBAL_BUF);
    let subscriber = tracing_test::internal::get_subscriber(mock_writer, env_filter);

    let _guard = tracing::dispatcher::set_default(&subscriber);
    let engine = new_rhai_test_engine();
    let input_logs = vec![
        r#"log_trace("trace log")"#,
        r#"log_debug("debug log")"#,
        r#"log_info("info log")"#,
        r#"log_warn("warn log")"#,
        r#"log_error("error log")"#,
    ];
    for log in input_logs {
        engine.eval::<()>(log).expect("it logged a message");
    }
    assert!(tracing_test::internal::logs_with_scope_contain(
        "apollo_router",
        "trace log"
    ));
    assert!(tracing_test::internal::logs_with_scope_contain(
        "apollo_router",
        "debug log"
    ));
    assert!(tracing_test::internal::logs_with_scope_contain(
        "apollo_router",
        "info log"
    ));
    assert!(tracing_test::internal::logs_with_scope_contain(
        "apollo_router",
        "warn log"
    ));
    assert!(tracing_test::internal::logs_with_scope_contain(
        "apollo_router",
        "error log"
    ));
}

#[test]
fn it_prints_messages_to_log() {
    let env_filter = "apollo_router=trace";
    let mock_writer = tracing_test::internal::MockWriter::new(&tracing_test::internal::GLOBAL_BUF);
    let subscriber = tracing_test::internal::get_subscriber(mock_writer, env_filter);

    let _guard = tracing::dispatcher::set_default(&subscriber);
    let engine = new_rhai_test_engine();
    engine
        .eval::<()>(r#"print("info log")"#)
        .expect("it logged a message");
    assert!(tracing_test::internal::logs_with_scope_contain(
        "apollo_router",
        "info log"
    ));
}

#[tokio::test]
async fn it_can_access_sdl_constant() {
    let dyn_plugin: Box<dyn DynPlugin> = crate::plugin::plugins()
        .find(|factory| factory.name == "apollo.rhai")
        .expect("Plugin not found")
        .create_instance_without_schema(
            &Value::from_str(r#"{"scripts":"tests/fixtures", "main":"test.rhai"}"#).unwrap(),
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

    // Call our function to make sure we can access the sdl
    let sdl: String = block
        .engine
        .call_fn(&mut guard, &block.ast, "get_sdl", ())
        .expect("can get sdl");
    assert_eq!(sdl.as_str(), "");
}

#[test]
fn it_provides_helpful_headermap_errors() {
    let mut engine = new_rhai_test_engine();
    engine.register_fn("new_hm", HeaderMap::new);

    let result = engine.eval::<HeaderMap>(
        r#"
let map = new_hm();
map["ümlaut"] = "will fail";
map
"#,
    );
    assert!(result.is_err());
    assert!(matches!(
        *result.unwrap_err(),
        EvalAltResult::ErrorRuntime(..)
    ));
}

// There is a lot of repetition in these tests, so I've tried to reduce that with these two
// macros. The repetition could probably be reduced further, but ...

macro_rules! gen_request_test {
    ($base: ident, $fn_name: literal) => {
        let dyn_plugin: Box<dyn DynPlugin> = crate::plugin::plugins()
            .find(|factory| factory.name == "apollo.rhai")
            .expect("Plugin not found")
            .create_instance_without_schema(
                &Value::from_str(
                    r#"{"scripts":"tests/fixtures", "main":"request_response_test.rhai"}"#,
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

        // We must wrap our canned request in Arc<Mutex<Option<>>> to keep the rhai runtime
        // happy
        let request = Arc::new(Mutex::new(Some($base::fake_builder().build())));

        // Call our rhai test function. If it return an error, the test failed.
        let result: Result<(), Box<rhai::EvalAltResult>> =
            block
                .engine
                .call_fn(&mut guard, &block.ast, $fn_name, (request,));
        result.expect("test failed");
    };
}

macro_rules! gen_response_test {
    ($base: ident, $fn_name: literal) => {
        let dyn_plugin: Box<dyn DynPlugin> = crate::plugin::plugins()
            .find(|factory| factory.name == "apollo.rhai")
            .expect("Plugin not found")
            .create_instance_without_schema(
                &Value::from_str(
                    r#"{"scripts":"tests/fixtures", "main":"request_response_test.rhai"}"#,
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

        // We must wrap our canned response in Arc<Mutex<Option<>>> to keep the rhai runtime
        // happy
        let response = Arc::new(Mutex::new(Some($base::default())));

        // Call our rhai test function. If it return an error, the test failed.
        let result: Result<(), Box<rhai::EvalAltResult>> =
            block
                .engine
                .call_fn(&mut guard, &block.ast, $fn_name, (response,));
        result.expect("test failed");
    };
}

#[tokio::test]
async fn it_can_process_supergraph_request() {
    let dyn_plugin: Box<dyn DynPlugin> = crate::plugin::plugins()
        .find(|factory| factory.name == "apollo.rhai")
        .expect("Plugin not found")
        .create_instance_without_schema(
            &Value::from_str(
                r#"{"scripts":"tests/fixtures", "main":"request_response_test.rhai"}"#,
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

    // We must wrap our canned request in Arc<Mutex<Option<>>> to keep the rhai runtime
    // happy
    let request = Arc::new(Mutex::new(Some(
        SupergraphRequest::canned_builder()
            .operation_name("canned")
            .build()
            .expect("build canned supergraph request"),
    )));

    // Call our rhai test function. If it return an error, the test failed.
    let result: Result<(), Box<rhai::EvalAltResult>> = block.engine.call_fn(
        &mut guard,
        &block.ast,
        "process_supergraph_request",
        (request,),
    );
    result.expect("test failed");
}

#[tokio::test]
async fn it_can_process_execution_request() {
    gen_request_test!(ExecutionRequest, "process_execution_request");
}

#[tokio::test]
async fn it_can_process_subgraph_request() {
    gen_request_test!(SubgraphRequest, "process_subgraph_request");
}

#[tokio::test]
async fn it_can_process_supergraph_response() {
    gen_response_test!(RhaiSupergraphResponse, "process_supergraph_response");
}

#[tokio::test]
async fn it_can_process_supergraph_response_is_primary() {
    gen_response_test!(
        RhaiSupergraphResponse,
        "process_supergraph_response_is_primary"
    );
}

#[tokio::test]
async fn it_can_process_supergraph_deferred_response() {
    gen_response_test!(
        RhaiSupergraphDeferredResponse,
        "process_supergraph_response"
    );
}

#[tokio::test]
async fn it_can_process_supergraph_deferred_response_is_not_primary() {
    gen_response_test!(
        RhaiSupergraphDeferredResponse,
        "process_supergraph_deferred_response_is_not_primary"
    );
}

#[tokio::test]
async fn it_can_process_execution_response() {
    gen_response_test!(RhaiExecutionResponse, "process_execution_response");
}

#[tokio::test]
async fn it_can_process_execution_response_is_primary() {
    gen_response_test!(
        RhaiExecutionResponse,
        "process_execution_response_is_primary"
    );
}

#[tokio::test]
async fn it_can_process_execution_deferred_response() {
    gen_response_test!(RhaiExecutionDeferredResponse, "process_execution_response");
}

#[tokio::test]
async fn it_can_process_execution_deferred_response_is_not_primary() {
    gen_response_test!(
        RhaiExecutionDeferredResponse,
        "process_execution_deferred_response_is_not_primary"
    );
}

#[tokio::test]
async fn it_can_process_subgraph_response() {
    let dyn_plugin: Box<dyn DynPlugin> = crate::plugin::plugins()
        .find(|factory| factory.name == "apollo.rhai")
        .expect("Plugin not found")
        .create_instance_without_schema(
            &Value::from_str(
                r#"{"scripts":"tests/fixtures", "main":"request_response_test.rhai"}"#,
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

    // We must wrap our canned response in Arc<Mutex<Option<>>> to keep the rhai runtime
    // happy
    let response = Arc::new(Mutex::new(Some(subgraph::Response::fake_builder().build())));

    // Call our rhai test function. If it return an error, the test failed.
    let result: Result<(), Box<rhai::EvalAltResult>> = block.engine.call_fn(
        &mut guard,
        &block.ast,
        "process_subgraph_response",
        (response,),
    );
    result.expect("test failed");
}

#[test]
fn it_can_urlencode_string() {
    let engine = new_rhai_test_engine();
    let encoded: String = engine
        .eval(r#"urlencode("This has an ümlaut in it.")"#)
        .expect("can encode string");
    assert_eq!(encoded, "This%20has%20an%20%C3%BCmlaut%20in%20it.");
}

#[test]
fn it_can_urldecode_string() {
    let engine = new_rhai_test_engine();
    let decoded: String = engine
        .eval(r#"urldecode("This%20has%20an%20%C3%BCmlaut%20in%20it.")"#)
        .expect("can decode string");
    assert_eq!(decoded, "This has an ümlaut in it.");
}

#[test]
fn it_can_base64encode_string() {
    let engine = new_rhai_test_engine();
    let encoded: String = engine
        .eval(r#"base64::encode("This has an ümlaut in it.")"#)
        .expect("can encode string");
    assert_eq!(encoded, "VGhpcyBoYXMgYW4gw7xtbGF1dCBpbiBpdC4=");
}

#[test]
fn it_can_base64decode_string() {
    let engine = new_rhai_test_engine();
    let decoded: String = engine
        .eval(r#"base64::decode("VGhpcyBoYXMgYW4gw7xtbGF1dCBpbiBpdC4=")"#)
        .expect("can decode string");
    assert_eq!(decoded, "This has an ümlaut in it.");
}

#[test]
fn it_can_create_unix_now() {
    let engine = new_rhai_test_engine();
    let st = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .expect("can get system time")
        .as_secs() as i64;
    let unix_now: i64 = engine
        .eval(r#"unix_now()"#)
        .expect("can get unix_now() timestamp");
    // Always difficult to do timing tests. unix_now() should execute within a second of st,
    // so...
    assert!(st <= unix_now && unix_now <= st + 1);
}

#[test]
fn it_can_generate_uuid() {
    let engine = new_rhai_test_engine();
    let uuid_v4_rhai: String = engine.eval(r#"uuid_v4()"#).expect("can get uuid");
    // attempt to parse back to UUID..
    let uuid_parsed = Uuid::parse_str(uuid_v4_rhai.as_str()).expect("can parse uuid from string");
    // finally validate that parsed string equals the returned value
    assert_eq!(uuid_v4_rhai, uuid_parsed.to_string());
}

async fn base_globals_function(fn_name: &str) -> Result<bool, Box<rhai::EvalAltResult>> {
    let dyn_plugin: Box<dyn DynPlugin> = crate::plugin::plugins()
        .find(|factory| factory.name == "apollo.rhai")
        .expect("Plugin not found")
        .create_instance_without_schema(
            &Value::from_str(
                r#"{"scripts":"tests/fixtures", "main":"global_variables_test.rhai"}"#,
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

    // Call our rhai test function. If it doesn't return an error, the test failed.
    block.engine.call_fn(&mut guard, &block.ast, fn_name, ())
}

#[tokio::test]
async fn it_can_find_router_global_variables() {
    if let Err(error) = base_globals_function("process_router_global_variables").await {
        panic!("test failed: {error:?}");
    }
}

async fn base_process_function(fn_name: &str) -> Result<(), Box<rhai::EvalAltResult>> {
    let dyn_plugin: Box<dyn DynPlugin> = crate::plugin::plugins()
        .find(|factory| factory.name == "apollo.rhai")
        .expect("Plugin not found")
        .create_instance_without_schema(
            &Value::from_str(
                r#"{"scripts":"tests/fixtures", "main":"request_response_test.rhai"}"#,
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

    // We must wrap our canned response in Arc<Mutex<Option<>>> to keep the rhai runtime
    // happy
    let response = Arc::new(Mutex::new(Some(subgraph::Response::fake_builder().build())));

    // Call our rhai test function. If it doesn't return an error, the test failed.
    block
        .engine
        .call_fn(&mut guard, &block.ast, fn_name, (response,))
}

#[tokio::test]
async fn it_can_process_om_subgraph_forbidden() {
    if let Err(error) = base_process_function("process_subgraph_response_om_forbidden").await {
        let processed_error = process_error(error);
        assert_eq!(processed_error.status, StatusCode::FORBIDDEN);
        assert_eq!(
            processed_error.message,
            Some("I have raised a 403".to_string())
        );
    } else {
        // Test failed
        panic!("error processed incorrectly");
    }
}

#[tokio::test]
async fn it_can_process_om_subgraph_forbidden_with_graphql_payload() {
    let error = base_process_function("process_subgraph_response_om_forbidden_graphql")
        .await
        .unwrap_err();

    let processed_error = process_error(error);
    assert_eq!(processed_error.status, StatusCode::FORBIDDEN);
    assert_eq!(
        processed_error.body,
        Some(
            crate::response::Response::builder()
                .errors(vec![{
                    Error::builder()
                        .message("I have raised a 403")
                        .extension_code("ACCESS_DENIED")
                        .build()
                }])
                .build()
        )
    );
}

#[tokio::test]
async fn it_can_process_om_subgraph_200_with_graphql_data() {
    let error = base_process_function("process_subgraph_response_om_200_graphql")
        .await
        .unwrap_err();

    let processed_error = process_error(error);
    assert_eq!(processed_error.status, StatusCode::OK);
    assert_eq!(
        processed_error.body,
        Some(
            crate::response::Response::builder()
                .data(serde_json::json!({ "name": "Ada Lovelace"}))
                .build()
        )
    );
}

#[tokio::test]
async fn it_can_process_string_subgraph_forbidden() {
    if let Err(error) = base_process_function("process_subgraph_response_string").await {
        let processed_error = process_error(error);
        assert_eq!(processed_error.status, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(processed_error.message, Some("rhai execution error: 'Runtime error: I have raised an error (line 155, position 5)\nin call to function 'process_subgraph_response_string''".to_string()));
    } else {
        // Test failed
        panic!("error processed incorrectly");
    }
}

#[tokio::test]
async fn it_can_process_ok_subgraph_forbidden() {
    let error = base_process_function("process_subgraph_response_om_ok")
        .await
        .unwrap_err();
    let processed_error = process_error(error);
    assert_eq!(processed_error.status, StatusCode::OK);
    assert_eq!(
        processed_error.message,
        Some("I have raised a 200".to_string())
    );
}

#[tokio::test]
async fn it_cannot_process_om_subgraph_missing_message_and_body() {
    if let Err(error) = base_process_function("process_subgraph_response_om_missing_message").await
    {
        let processed_error = process_error(error);
        assert_eq!(processed_error.status, StatusCode::BAD_REQUEST);
        assert_eq!(processed_error.message, Some("rhai execution error: 'Runtime error: #{\"status\": 400} (line 166, position 5)\nin call to function 'process_subgraph_response_om_missing_message''".to_string()));
    } else {
        // Test failed
        panic!("error processed incorrectly");
    }
}

#[tokio::test]
async fn it_mentions_source_when_syntax_error_occurs() {
    let err: Box<dyn std::error::Error> = crate::plugin::plugins()
        .find(|factory| factory.name == "apollo.rhai")
        .expect("Plugin not found")
        .create_instance_without_schema(
            &Value::from_str(r#"{"scripts":"tests/fixtures", "main":"syntax_errors.rhai"}"#)
                .unwrap(),
        )
        .await
        .err()
        .unwrap();

    assert!(err.to_string().contains("syntax_errors.rhai"));
}

#[test]
#[should_panic(
    expected = "can use env: ErrorRuntime(\"could not expand variable: THIS_SHOULD_NOT_EXIST, environment variable not found\", none)"
)]
fn it_cannot_expand_missing_environment_variable() {
    assert!(std::env::var("THIS_SHOULD_NOT_EXIST").is_err());
    let engine = new_rhai_test_engine();
    let _: String = engine
        .eval(
            r#"
        env::get("THIS_SHOULD_NOT_EXIST")"#,
        )
        .expect("can use env");
}

// POSIX specifies HOME is always set
#[test]
fn it_can_expand_environment_variable() {
    let home = std::env::var("HOME").expect("can always read HOME");
    let engine = new_rhai_test_engine();
    let env_variable: String = engine
        .eval(
            r#"
        env::get("HOME")"#,
        )
        .expect("can use env");
    assert_eq!(home, env_variable);
}

#[test]
fn it_can_compare_method_strings() {
    let mut engine = new_rhai_test_engine();
    engine.register_fn(
        "new_method",
        |method: &str| -> Result<Method, Box<EvalAltResult>> {
            Method::from_str(&method.to_uppercase()).map_err(|e| e.to_string().into())
        },
    );

    let method: bool = engine
        .eval(
            r#"
        let get = new_method("GEt").to_string();
        get == "GET"
        "#,
        )
        .expect("can compare properly");
    assert!(method);
}
