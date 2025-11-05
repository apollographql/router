//! Rhai module tests.

use std::str::FromStr;
use std::sync::Arc;
use std::time::SystemTime;

use http::HeaderMap;
use http::HeaderValue;
use http::Method;
use http::StatusCode;
use parking_lot::Mutex;
use rhai::Engine;
use rhai::EvalAltResult;
use serde_json::Value;
use sha2::Digest;
use tower::BoxError;
use tower::Service;
use tower::ServiceExt;
use tower::util::BoxService;
use tracing_futures::WithSubscriber;
use uuid::Uuid;

use super::PathBuf;
use super::Rhai;
use super::process_error;
use super::subgraph;
use crate::Context;
use crate::assert_response_eq_ignoring_error_id;
use crate::assert_snapshot_subscriber;
use crate::graphql;
use crate::graphql::Error;
use crate::graphql::Request;
use crate::http_ext;
use crate::plugin::DynPlugin;
use crate::plugin::test::MockExecutionService;
use crate::plugin::test::MockRouterService;
use crate::plugin::test::MockSubgraphService;
use crate::plugin::test::MockSupergraphService;
use crate::plugins::rhai::engine::RhaiExecutionDeferredResponse;
use crate::plugins::rhai::engine::RhaiExecutionResponse;
use crate::plugins::rhai::engine::RhaiRouterChunkedResponse;
use crate::plugins::rhai::engine::RhaiRouterFirstRequest;
use crate::plugins::rhai::engine::RhaiRouterResponse;
use crate::plugins::rhai::engine::RhaiSupergraphDeferredResponse;
use crate::plugins::rhai::engine::RhaiSupergraphResponse;
use crate::services::ExecutionRequest;
use crate::services::SubgraphRequest;
use crate::services::SupergraphRequest;
use crate::services::SupergraphResponse;
use crate::test_harness::tracing_test;

// There is a lot of repetition in these tests, so I've tried to reduce that with these two
// functions. The repetition could probably be reduced further, but ...
async fn call_rhai_function(fn_name: &str) -> Result<(), Box<rhai::EvalAltResult>> {
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

    // Get a scope to use for our test
    let scope = rhai_instance.scope.clone();

    let mut guard = scope.lock();

    // We must wrap our canned response in Arc<Mutex<Option<>>> to keep the rhai runtime
    // happy
    let response = Arc::new(Mutex::new(Some(subgraph::Response::fake_builder().build())));

    // Call our rhai test function. If it doesn't return an error, the test failed.
    rhai_instance
        .engine
        .call_fn(&mut guard, &rhai_instance.ast, fn_name, (response,))
}

async fn call_rhai_function_with_arg<T: Sync + Send + 'static>(
    fn_name: &str,
    arg: T,
) -> Result<(), Box<rhai::EvalAltResult>> {
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

    // Get a scope to use for our test
    let scope = rhai_instance.scope.clone();

    let mut guard = scope.lock();

    // We must wrap our canned request in Arc<Mutex<Option<>>> to keep the rhai runtime
    // happy
    let wrapped_arg = Arc::new(Mutex::new(Some(arg)));

    rhai_instance
        .engine
        .call_fn(&mut guard, &rhai_instance.ast, fn_name, (wrapped_arg,))
}

#[tokio::test]
async fn rhai_plugin_supergraph_service() -> Result<(), BoxError> {
    async {
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
    .with_subscriber(assert_snapshot_subscriber!())
    .await
}

#[tokio::test]
async fn rhai_plugin_execution_service_error() -> Result<(), BoxError> {
    async {
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
            body.errors.first().unwrap().message.as_str(),
            "rhai execution error: 'Runtime error: An error occured (line 30, position 5)'"
        );
        Ok(())
    }
    .with_subscriber(assert_snapshot_subscriber!({r#"[].message"# => "[message]"}))
    .await
}

// A Rhai engine suitable for minimal testing. There are no scripts and the SDL is an empty
// string.
fn new_rhai_test_engine() -> Engine {
    Rhai::new_rhai_engine(None, "".to_string(), PathBuf::new())
}

#[test]
fn it_logs_messages() {
    let _guard = tracing_test::dispatcher_guard();

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

    assert!(tracing_test::logs_contain("trace log"));
    assert!(tracing_test::logs_contain("debug log"));
    assert!(tracing_test::logs_contain("info log"));
    assert!(tracing_test::logs_contain("warn log"));
    assert!(tracing_test::logs_contain("error log"));
}

#[test]
fn it_prints_messages_to_log() {
    use tracing::subscriber;

    use crate::assert_snapshot_subscriber;

    subscriber::with_default(assert_snapshot_subscriber!(), || {
        let engine = new_rhai_test_engine();
        engine
            .eval::<()>(r#"print("info log")"#)
            .expect("it logged a message");
    });
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

    // Get a scope to use for our test
    let scope = rhai_instance.scope.clone();

    let mut guard = scope.lock();

    // Call our function to make sure we can access the sdl
    let sdl: String = rhai_instance
        .engine
        .call_fn(&mut guard, &rhai_instance.ast, "get_sdl", ())
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

#[tokio::test]
async fn it_can_process_router_request() {
    let mut request = RhaiRouterFirstRequest::default();
    request.request.headers_mut().insert(
        "content-type",
        HeaderValue::from_str("application/json").unwrap(),
    );
    *request.request.method_mut() = http::Method::GET;

    call_rhai_function_with_arg("process_router_request", request)
        .await
        .expect("test failed");
}

#[tokio::test]
async fn it_can_process_supergraph_request() {
    let request = SupergraphRequest::canned_builder()
        .operation_name("canned")
        .build()
        .expect("build canned supergraph request");

    call_rhai_function_with_arg("process_supergraph_request", request)
        .await
        .expect("test failed");
}

#[tokio::test]
async fn it_can_process_execution_request() {
    let request = ExecutionRequest::fake_builder().build();
    call_rhai_function_with_arg("process_execution_request", request)
        .await
        .expect("test failed");
}

#[tokio::test]
async fn it_can_process_subgraph_request() {
    let request = SubgraphRequest::fake_builder().build();
    call_rhai_function_with_arg("process_subgraph_request", request)
        .await
        .expect("test failed");
}

#[tokio::test]
async fn it_can_process_router_response() {
    let response = RhaiRouterResponse::default();
    call_rhai_function_with_arg("process_router_response", response)
        .await
        .expect("test failed");
}

#[tokio::test]
async fn it_can_process_router_chunked_response() {
    let response = RhaiRouterChunkedResponse::default();
    call_rhai_function_with_arg("process_router_chunked_response", response)
        .await
        .expect("test failed");
}

#[tokio::test]
async fn it_can_process_supergraph_response() {
    let response = RhaiSupergraphResponse::default();
    call_rhai_function_with_arg("process_supergraph_response", response)
        .await
        .expect("test failed");
}

#[tokio::test]
async fn it_can_process_supergraph_deferred_response() {
    let response = RhaiSupergraphDeferredResponse::default();
    call_rhai_function_with_arg("process_supergraph_deferred_response", response)
        .await
        .expect("test failed");
}

#[tokio::test]
async fn it_can_process_execution_response() {
    let response = RhaiExecutionResponse::default();
    call_rhai_function_with_arg("process_execution_response", response)
        .await
        .expect("test failed");
}

#[tokio::test]
async fn it_can_process_execution_deferred_response() {
    let response = RhaiExecutionDeferredResponse::default();
    call_rhai_function_with_arg("process_execution_deferred_response", response)
        .await
        .expect("test failed");
}

#[tokio::test]
async fn it_can_process_subgraph_response() {
    let response = subgraph::Response::fake_builder()
        .status_code(StatusCode::OK)
        .build();
    call_rhai_function_with_arg("process_subgraph_response", response)
        .await
        .expect("test failed");
}

#[tokio::test]
async fn it_can_parse_request_uri() {
    let mut request = SupergraphRequest::canned_builder()
        .operation_name("canned")
        .build()
        .expect("build canned supergraph request");
    *request.supergraph_request.uri_mut() = "https://not-default:8080/path".parse().unwrap();
    call_rhai_function_with_arg("test_parse_request_details", request)
        .await
        .expect("test failed");
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
fn it_can_base64encode_string_with_alphabet() {
    let engine = new_rhai_test_engine();
    let encoded: String = engine
        .eval(r#"base64::encode("<<???>>", base64::STANDARD)"#)
        .expect("can encode string");
    assert_eq!(encoded, "PDw/Pz8+Pg==");
    let encoded: String = engine
        .eval(r#"base64::encode("<<???>>", base64::URL_SAFE)"#)
        .expect("can encode string");
    assert_eq!(encoded, "PDw_Pz8-Pg==");
}

#[test]
fn it_can_base64decode_string_with_alphabet() {
    let engine = new_rhai_test_engine();
    let decoded: String = engine
        .eval(r#"base64::decode("PDw/Pz8+Pg==", base64::STANDARD)"#)
        .expect("can decode string");
    assert_eq!(decoded, "<<???>>");
    let decoded: String = engine
        .eval(r#"base64::decode("PDw_Pz8-Pg==", base64::URL_SAFE)"#)
        .expect("can decode string");
    assert_eq!(decoded, "<<???>>");
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
fn it_can_create_unix_ms_now() {
    let engine = new_rhai_test_engine();
    let st = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .expect("can get system time")
        .as_millis() as i64;
    let unix_ms_now: i64 = engine
        .eval(r#"unix_ms_now()"#)
        .expect("can get unix_ms_now() timestamp");
    // Always difficult to do timing tests. unix_ms_now() should execute within a second of st,
    // so...
    assert!(st <= unix_ms_now && unix_ms_now <= st + 1000);
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

#[test]
fn it_can_sha256_string() {
    let engine = new_rhai_test_engine();
    let hash = sha2::Sha256::digest("hello world".as_bytes());
    let hash_rhai: String = engine
        .eval(r#"sha256::digest("hello world")"#)
        .expect("can decode string");
    assert_eq!(hash_rhai, hex::encode(hash));
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

    // Get a scope to use for our test
    let scope = rhai_instance.scope.clone();

    let mut guard = scope.lock();

    // Call our rhai test function. If it doesn't return an error, the test failed.
    rhai_instance
        .engine
        .call_fn(&mut guard, &rhai_instance.ast, fn_name, ())
}

#[tokio::test]
async fn it_can_find_router_global_variables() {
    if let Err(error) = base_globals_function("process_router_global_variables").await {
        panic!("test failed: {error:?}");
    }
}

#[tokio::test]
async fn it_can_process_om_subgraph_forbidden() {
    if let Err(error) = call_rhai_function("process_subgraph_response_om_forbidden").await {
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
    let error = call_rhai_function("process_subgraph_response_om_forbidden_graphql")
        .await
        .unwrap_err();

    let processed_error = process_error(error);
    assert_eq!(processed_error.status, StatusCode::FORBIDDEN);
    assert_response_eq_ignoring_error_id!(
        processed_error.body.unwrap(),
        graphql::Response::builder()
            .errors(vec![{
                Error::builder()
                    .message("I have raised a 403")
                    .extension_code("ACCESS_DENIED")
                    .build()
            }])
            .build()
    );
}

#[tokio::test]
async fn it_can_process_om_subgraph_200_with_graphql_data() {
    let error = call_rhai_function("process_subgraph_response_om_200_graphql")
        .await
        .unwrap_err();

    let processed_error = process_error(error);
    assert_eq!(processed_error.status, StatusCode::OK);
    assert_eq!(
        processed_error.body,
        Some(
            graphql::Response::builder()
                .data(serde_json::json!({ "name": "Ada Lovelace"}))
                .build()
        )
    );
}

#[tokio::test]
async fn it_can_process_string_subgraph_forbidden() {
    if let Err(error) = call_rhai_function("process_subgraph_response_string").await {
        let processed_error = process_error(error);
        assert_eq!(processed_error.status, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(processed_error.message, Some("rhai execution error: 'Runtime error: I have raised an error (line 257, position 5)'".to_string()));
    } else {
        // Test failed
        panic!("error processed incorrectly");
    }
}

#[tokio::test]
async fn it_can_process_ok_subgraph_forbidden() {
    let error = call_rhai_function("process_subgraph_response_om_ok")
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
    if let Err(error) = call_rhai_function("process_subgraph_response_om_missing_message").await {
        let processed_error = process_error(error);
        assert_eq!(processed_error.status, StatusCode::BAD_REQUEST);
        assert_eq!(
            processed_error.message,
            Some(
                "rhai execution error: 'Runtime error: #{\"status\": 400} (line 268, position 5)'"
                    .to_string()
            )
        );
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

#[tokio::test]
async fn test_router_service_adds_timestamp_header() -> Result<(), BoxError> {
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
            &Value::from_str(r#"{"scripts":"tests/fixtures", "main":"remove_header.rhai"}"#)
                .unwrap(),
        )
        .await
        .unwrap();

    let mut router_service = dyn_plugin.supergraph_service(BoxService::new(mock_service));
    let context = Context::new();
    context.insert("test", 5i64).unwrap();
    let supergraph_req = SupergraphRequest::fake_builder()
        .header("x-custom-header", "CUSTOM_VALUE")
        .context(context)
        .build()?;

    let service_response = router_service.ready().await?.call(supergraph_req).await?;
    assert_eq!(StatusCode::OK, service_response.response.status());

    let headers = service_response.response.headers().clone();
    assert!(headers.get("x-custom-header").is_none());

    Ok(())
}

#[tokio::test]
async fn it_can_access_demand_control_context() -> Result<(), BoxError> {
    let mut mock_service = MockSupergraphService::new();
    mock_service
        .expect_call()
        .times(1)
        .returning(move |req: SupergraphRequest| {
            Ok(SupergraphResponse::fake_builder()
                .context(req.context)
                .build()
                .unwrap())
        });

    let dyn_plugin: Box<dyn DynPlugin> = crate::plugin::plugins()
        .find(|factory| factory.name == "apollo.rhai")
        .expect("Plugin not found")
        .create_instance_without_schema(
            &Value::from_str(r#"{"scripts":"tests/fixtures", "main":"demand_control.rhai"}"#)
                .unwrap(),
        )
        .await
        .unwrap();

    let mut router_service = dyn_plugin.supergraph_service(BoxService::new(mock_service));
    let context = Context::new();
    context.insert_estimated_cost(50.0).unwrap();
    context.insert_actual_cost(35.0).unwrap();
    context
        .insert_cost_strategy("test_strategy".to_string())
        .unwrap();
    context.insert_cost_result("COST_OK".to_string()).unwrap();
    let supergraph_req = SupergraphRequest::fake_builder().context(context).build()?;

    let service_response = router_service.ready().await?.call(supergraph_req).await?;
    assert_eq!(StatusCode::OK, service_response.response.status());

    let headers = service_response.response.headers().clone();
    let demand_control_header = headers
        .get("demand-control-estimate")
        .map(|h| h.to_str().unwrap());
    assert_eq!(demand_control_header, Some("50.0"));

    let demand_control_header = headers
        .get("demand-control-actual")
        .map(|h| h.to_str().unwrap());
    assert_eq!(demand_control_header, Some("35.0"));

    let demand_control_header = headers
        .get("demand-control-strategy")
        .map(|h| h.to_str().unwrap());
    assert_eq!(demand_control_header, Some("test_strategy"));

    let demand_control_header = headers
        .get("demand-control-result")
        .map(|h| h.to_str().unwrap());
    assert_eq!(demand_control_header, Some("COST_OK"));

    Ok(())
}

#[tokio::test]
async fn test_rhai_header_removal_with_non_utf8_header() -> Result<(), BoxError> {
    let bytes = b"\x80";
    // Prove that the bytes are not valid UTF-8
    assert!(String::from_utf8(bytes.to_vec()).is_err());

    let mut mock_service = MockSupergraphService::new();
    mock_service
        .expect_call()
        .times(1)
        .returning(move |req: SupergraphRequest| {
            let mut response_builder = SupergraphResponse::fake_builder().context(req.context);
            let header_value = HeaderValue::from_bytes(bytes).unwrap();
            response_builder = response_builder.header("x-binary-header", header_value);

            Ok(response_builder.build().unwrap())
        });

    let dyn_plugin: Box<dyn DynPlugin> = crate::plugin::plugins()
        .find(|factory| factory.name == "apollo.rhai")
        .expect("Plugin not found")
        .create_instance_without_schema(
            &Value::from_str(
                r#"{"scripts":"tests/fixtures", "main":"non_utf8_header_removal.rhai"}"#,
            )
            .unwrap(),
        )
        .await
        .unwrap();

    let mut router_service = dyn_plugin.supergraph_service(BoxService::new(mock_service));
    let context = Context::new();
    let supergraph_req = SupergraphRequest::fake_builder().context(context).build()?;

    let mut service_response = router_service.ready().await?.call(supergraph_req).await?;

    assert_eq!(StatusCode::OK, service_response.response.status());

    // Removing a non-UTF-8 header should be OK
    let body = service_response.next_response().await.unwrap();
    if body.errors.is_empty() {
        // yay, no errors
    } else {
        let rhai_error = body
            .errors
            .iter()
            .find(|e| e.message.contains("rhai execution error"))
            .expect("unexpected non-rhai error");
        panic!("Got an unexpected rhai error: {rhai_error:?}");
    }

    // Check that the header was actually removed
    let headers = service_response.response.headers().clone();
    assert!(
        headers.get("x-binary-header").is_none(),
        "x-binary-header should have been removed but it's still present"
    );

    Ok(())
}

async fn test_supergraph_error_logging(script_name: &str) -> Result<(), BoxError> {
    let mut mock_service = MockSupergraphService::new();
    mock_service.expect_call().never();

    let dyn_plugin = create_plugin(script_name).await?;

    let mut service = dyn_plugin.supergraph_service(BoxService::new(mock_service));
    let req = SupergraphRequest::fake_builder()
        .context(Context::new())
        .build()?;

    let _response = service.ready().await?.call(req).await?;
    Ok(())
}

async fn create_plugin(script_name: &str) -> Result<Box<dyn DynPlugin>, BoxError> {
    let config = format!(
        r#"{{"scripts":"tests/fixtures", "main":"{}"}}"#,
        script_name
    );
    let dyn_plugin: Box<dyn DynPlugin> = crate::plugin::plugins()
        .find(|factory| factory.name == "apollo.rhai")
        .expect("Plugin not found")
        .create_instance_without_schema(&Value::from_str(&config)?)
        .await?;
    Ok(dyn_plugin)
}

async fn test_execution_error_logging(script_name: &str) -> Result<(), BoxError> {
    let mut mock_service = MockExecutionService::new();
    mock_service.expect_clone().return_once(move || {
        let mut mock_service = MockExecutionService::new();
        mock_service.expect_call().never();
        mock_service
    });
    let dyn_plugin = create_plugin(script_name).await?;
    let mut service = dyn_plugin.execution_service(BoxService::new(mock_service));
    let fake_req = http_ext::Request::fake_builder()
        .body(Request::builder().query(String::new()).build())
        .build()?;
    let req = ExecutionRequest::fake_builder()
        .context(Context::new())
        .supergraph_request(fake_req)
        .build();

    let _response = service.ready().await?.call(req).await?;
    Ok(())
}

async fn test_router_error_logging(script_name: &str) -> Result<(), BoxError> {
    let mut mock_service = MockRouterService::new();
    mock_service.expect_call().never();

    let dyn_plugin = create_plugin(script_name).await?;

    let mut service = dyn_plugin.router_service(BoxService::new(mock_service));
    let req = crate::services::RouterRequest::fake_builder()
        .context(Context::new())
        .build()?;

    let _response = service.ready().await?.call(req).await?;
    Ok(())
}

async fn test_subgraph_error_logging(script_name: &str) -> Result<(), BoxError> {
    let mut mock_service = MockSubgraphService::new();
    mock_service.expect_call().never();

    let dyn_plugin = create_plugin(script_name).await?;

    let mut service = dyn_plugin.subgraph_service("test_subgraph", BoxService::new(mock_service));
    let req = SubgraphRequest::fake_builder()
        .context(Context::new())
        .build();

    let _response = service.ready().await?.call(req).await?;
    Ok(())
}

#[tokio::test]
async fn test_supergraph_error_logging_without_body() -> Result<(), BoxError> {
    test_supergraph_error_logging("error_without_body.rhai")
        .with_subscriber(assert_snapshot_subscriber!())
        .await
}

#[tokio::test]
async fn test_supergraph_error_logging_with_body() -> Result<(), BoxError> {
    test_supergraph_error_logging("error_with_body.rhai")
        .with_subscriber(assert_snapshot_subscriber!())
        .await
}

#[tokio::test]
async fn test_execution_error_logging_without_body() -> Result<(), BoxError> {
    test_execution_error_logging("error_without_body.rhai")
        .with_subscriber(assert_snapshot_subscriber!())
        .await
}

#[tokio::test]
async fn test_execution_error_logging_with_body() -> Result<(), BoxError> {
    test_execution_error_logging("error_with_body.rhai")
        .with_subscriber(assert_snapshot_subscriber!())
        .await
}

#[tokio::test]
async fn test_router_error_logging_without_body() -> Result<(), BoxError> {
    test_router_error_logging("error_without_body.rhai")
        .with_subscriber(assert_snapshot_subscriber!())
        .await
}

#[tokio::test]
async fn test_router_error_logging_with_body() -> Result<(), BoxError> {
    test_router_error_logging("error_with_body.rhai")
        .with_subscriber(assert_snapshot_subscriber!())
        .await
}

#[tokio::test]
async fn test_subgraph_error_logging_without_body() -> Result<(), BoxError> {
    test_subgraph_error_logging("error_without_body.rhai")
        .with_subscriber(assert_snapshot_subscriber!())
        .await
}

#[tokio::test]
async fn test_subgraph_error_logging_with_body() -> Result<(), BoxError> {
    test_subgraph_error_logging("error_with_body.rhai")
        .with_subscriber(assert_snapshot_subscriber!())
        .await
}

// Helper for calling property mutation test functions
async fn call_property_mutation_test(
    fn_name: &str,
    arg: impl Sync + Send + 'static,
) -> Result<(), Box<rhai::EvalAltResult>> {
    let dyn_plugin: Box<dyn DynPlugin> = crate::plugin::plugins()
        .find(|factory| factory.name == "apollo.rhai")
        .expect("Plugin not found")
        .create_instance_without_schema(
            &Value::from_str(
                r#"{"scripts":"tests/fixtures", "main":"test_property_mutations.rhai"}"#,
            )
            .unwrap(),
        )
        .await
        .unwrap();

    let it: &dyn std::any::Any = dyn_plugin.as_any();
    let rhai_instance: &Rhai = it.downcast_ref::<Rhai>().expect("downcast");

    let scope = rhai_instance.scope.clone();
    let mut guard = scope.lock();

    let wrapped_arg = Arc::new(Mutex::new(Some(arg)));

    rhai_instance
        .engine
        .call_fn(&mut guard, &rhai_instance.ast, fn_name, (wrapped_arg,))
}

#[tokio::test]
async fn test_supergraph_header_mutation() {
    let request = SupergraphRequest::fake_builder().build().unwrap();
    call_property_mutation_test("test_supergraph_header_mutation", request)
        .await
        .expect("test failed");
}

#[tokio::test]
async fn test_supergraph_body_mutation() {
    let request = SupergraphRequest::fake_builder().build().unwrap();
    call_property_mutation_test("test_supergraph_body_mutation", request)
        .await
        .expect("test failed");
}

#[tokio::test]
async fn test_execution_header_mutation() {
    let request = ExecutionRequest::fake_builder().build();
    call_property_mutation_test("test_execution_header_mutation", request)
        .await
        .expect("test failed");
}

#[tokio::test]
async fn test_router_header_mutation() {
    let request = RhaiRouterFirstRequest::default();
    call_property_mutation_test("test_router_header_mutation", request)
        .await
        .expect("test failed");
}

#[tokio::test]
async fn test_subgraph_read_only_headers() {
    let request = SubgraphRequest::fake_builder().build();
    call_property_mutation_test("test_subgraph_read_only_headers", request)
        .await
        .expect("test failed");
}

#[tokio::test]
async fn test_subgraph_property_chain_with_split() {
    let supergraph_req = http::Request::builder()
        .header("cookie", "session=abc; user=john; theme=dark")
        .body(graphql::Request::builder().query(String::new()).build())
        .unwrap();

    let request = SubgraphRequest::fake_builder()
        .supergraph_request(Arc::new(supergraph_req))
        .build();

    call_property_mutation_test("test_subgraph_property_chain_with_split", request)
        .await
        .expect("test failed - property chains should work with read-only properties");
}

#[tokio::test]
async fn test_subgraph_property_chain_with_trim() {
    let supergraph_req = http::Request::builder()
        .header("auth", "  token  ")
        .body(graphql::Request::builder().query(String::new()).build())
        .unwrap();

    let request = SubgraphRequest::fake_builder()
        .supergraph_request(Arc::new(supergraph_req))
        .build();

    call_property_mutation_test("test_subgraph_property_chain_with_trim", request)
        .await
        .expect("test failed - property chains should work with read-only properties");
}

#[tokio::test]
async fn test_complex_property_chain() {
    let supergraph_req = http::Request::builder()
        .header("cookie", " session=abc ; user=john")
        .body(graphql::Request::builder().query(String::new()).build())
        .unwrap();

    let request = SubgraphRequest::fake_builder()
        .supergraph_request(Arc::new(supergraph_req))
        .build();

    call_property_mutation_test("test_complex_property_chain", request)
        .await
        .expect("test failed - complex property chains should work");
}
