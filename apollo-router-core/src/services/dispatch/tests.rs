use super::*;
use crate::test_utils::TowerTest;
use futures::StreamExt;
use futures::stream;
use serde_json::json;
use tower::ServiceExt;

// Test request types
#[derive(Debug, Clone, PartialEq)]
struct StringRequest(String);

#[derive(Debug, Clone, PartialEq)]
struct NumberRequest(i32);

#[derive(Debug, Clone, PartialEq)]
struct UnhandledRequest;


#[tokio::test]
async fn test_dispatch_requests() {
    let usize_handler = TowerTest::builder().service::<Request<usize>, _, _, _>(|mut h| async move {
        h.allow(1);
        let r = h.next_request().await.expect("no request");
        r.1.send_response(Response {
            extensions: Default::default(),
            responses: Box::pin(stream::iter([json! {
                "test"
            }])),
        })
    });

    let string_handler = TowerTest::builder().service::<Request<String>, _, _, _>(|mut h| async move {
        h.allow(1);
        let r = h.next_request().await.expect("hello");
        r.1.send_response(Response {
            extensions: Default::default(),
            responses: Box::pin(stream::iter([json! {
                "test"
            }])),
        })
    });
    // Build dispatcher with handlers
    let mut handlers = RequestDispatcher::builder();
    handlers.register_handler(usize_handler);
    handlers.register_handler(string_handler);
    let mut dispatcher = handlers.into_dispatcher();

    // Create a string request
    let extensions = Extensions::new();
    let request = Request {
        extensions: extensions.clone(),
        service_name: "test".to_string(),
        body: Box::new("hello".to_string()) as Box<dyn Any + Send>,
        variables: HashMap::new(),
    };

    // Dispatch the request
    let response = dispatcher
        .ready()
        .await
        .unwrap()
        .call(request)
        .await
        .unwrap();

    // Verify response
    let mut stream = response.responses;
    let value = stream.next().await.unwrap();
    assert_eq!(value, json!("test"));
}

#[tokio::test]
async fn test_dispatch_number_request() {
    let number_handler = TowerTest::builder().service::<Request<NumberRequest>, _, _, _>(|mut h| async move {
        h.allow(1);
        let (req, resp) = h.next_request().await.expect("should receive request");
        let value = req.body.0 * 2;
        resp.send_response(Response {
            extensions: req.extensions,
            responses: Box::pin(stream::iter([json!(value)])),
        })
    });

    // Build dispatcher with handlers
    let mut handlers = RequestDispatcher::builder();
    handlers.register_handler(number_handler);
    let mut dispatcher = handlers.into_dispatcher();

    // Create a number request
    let extensions = Extensions::new();
    let request = Request {
        extensions: extensions.clone(),
        service_name: "test".to_string(),
        body: Box::new(NumberRequest(21)) as Box<dyn Any + Send>,
        variables: HashMap::new(),
    };

    // Dispatch the request
    let response = dispatcher
        .ready()
        .await
        .unwrap()
        .call(request)
        .await
        .unwrap();

    // Verify response
    let mut stream = response.responses;
    let value = stream.next().await.unwrap();
    assert_eq!(value, json!(42));
}

#[tokio::test]
async fn test_no_handler_for_type() {
    let string_handler = TowerTest::builder().service::<Request<String>, _, _, _>(|mut h| async move {
        h.allow(0); // Don't expect any requests
    });

    // Build dispatcher with some handlers but not for UnhandledRequest
    let mut handlers = RequestDispatcher::builder();
    handlers.register_handler(string_handler);
    let mut dispatcher = handlers.into_dispatcher();

    // Create an unhandled request
    let unhandled = UnhandledRequest;
    let request = Request {
        extensions: Extensions::new(),
        service_name: "test".to_string(),
        body: Box::new(unhandled) as Box<dyn Any + Send>,
        variables: HashMap::new(),
    };

    // Dispatch should fail
    let error = dispatcher
        .ready()
        .await
        .unwrap()
        .call(request)
        .await
        .unwrap_err();

    // Check if it's a NoHandlerForType error by downcasting
    if let Some(Error::NoHandlerForType { type_name, .. }) = error.downcast_ref::<Error>() {
        // When we box a type into dyn Any, the type_name becomes the trait object type
        // So we just verify we got the NoHandlerForType error
        assert!(
            type_name.contains("Any"),
            "Type name '{}' doesn't contain Any",
            type_name
        );
    } else {
        panic!("Expected NoHandlerForType error, got {:?}", error);
    }
}

#[tokio::test]
async fn test_builder_pattern() {
    let string_handler = TowerTest::builder().service::<Request<StringRequest>, _, _, _>(|mut h| async move {
        h.allow(1);
        let (req, resp) = h.next_request().await.expect("should receive request");
        resp.send_response(Response {
            extensions: req.extensions,
            responses: Box::pin(stream::iter([json!(format!("Handled: {}", req.body.0))])),
        })
    });

    let number_handler = TowerTest::builder().service::<Request<NumberRequest>, _, _, _>(|mut h| async move {
        h.allow(0); // Not used in this test
    });

    // Use the handler registration trait directly
    let mut handlers = RequestDispatcher::builder();
    handlers.register_handler(string_handler);
    handlers.register_handler(number_handler);
    let mut dispatcher = handlers.into_dispatcher();

    // Test that it works
    let request = Request {
        extensions: Extensions::new(),
        service_name: "test".to_string(),
        body: Box::new(StringRequest("builder test".to_string())) as Box<dyn Any + Send>,
        variables: HashMap::new(),
    };

    let response = dispatcher
        .ready()
        .await
        .unwrap()
        .call(request)
        .await
        .unwrap();
    let mut stream = response.responses;
    let value = stream.next().await.unwrap();
    assert_eq!(
        value,
        json!("Handled: builder test")
    );
}


#[tokio::test]
async fn test_handler_error_propagation() {
    let failing_handler = TowerTest::builder().service::<Request<StringRequest>, _, _, _>(|mut h| async move {
        h.allow(1);
        let (_req, resp) = h.next_request().await.expect("should receive request");
        resp.send_error(tower::BoxError::from("Intentional failure"));
    });

    // Build dispatcher with failing handler
    let mut handlers = RequestDispatcher::builder();
    handlers.register_handler(failing_handler);
    let mut dispatcher = handlers.into_dispatcher();

    // Create a request
    let request = Request {
        extensions: Extensions::new(),
        service_name: "test".to_string(),
        body: Box::new(StringRequest("will fail".to_string())) as Box<dyn Any + Send>,
        variables: HashMap::new(),
    };

    // Dispatch should propagate the error
    let error = dispatcher
        .ready()
        .await
        .unwrap()
        .call(request)
        .await
        .unwrap_err();

    // Just verify we got an error - the exact error message depends on the underlying service
    assert!(!error.to_string().is_empty());
}

#[tokio::test]
async fn test_multiple_requests_same_type() {
    let string_handler = TowerTest::builder().service::<Request<StringRequest>, _, _, _>(|mut h| async move {
        h.allow(3);
        for _i in 0..3 {
            let (req, resp) = h.next_request().await.expect("should receive request");
            resp.send_response(Response {
                extensions: req.extensions,
                responses: Box::pin(stream::iter([json!(format!("Handled: {}", req.body.0))])),
            });
        }
    });

    // Build dispatcher
    let mut handlers = RequestDispatcher::builder();
    handlers.register_handler(string_handler);
    let mut dispatcher = handlers.into_dispatcher();

    // Send multiple requests of the same type
    for i in 0..3 {
        let request = Request {
            extensions: Extensions::new(),
            service_name: format!("test{}", i),
            body: Box::new(StringRequest(format!("request {}", i))) as Box<dyn Any + Send>,
            variables: HashMap::new(),
        };

        let response = dispatcher
            .ready()
            .await
            .unwrap()
            .call(request)
            .await
            .unwrap();
        let mut stream = response.responses;
        let value = stream.next().await.unwrap();
        assert_eq!(value, json!(format!("Handled: request {}", i)));
    }
}

#[tokio::test]
async fn test_service_name_preserved() {
    let expected_service_name = "special-service";
    let string_handler = TowerTest::builder().service::<Request<StringRequest>, _, _, _>(move |mut h| {
        let expected = expected_service_name.to_string();
        async move {
            h.allow(1);
            let (req, resp) = h.next_request().await.expect("should receive request");
            // Verify service name is preserved
            assert_eq!(req.service_name, expected);
            resp.send_response(Response {
                extensions: req.extensions,
                responses: Box::pin(stream::iter([json!("ok")])),
            });
        }
    });

    let mut handlers = RequestDispatcher::builder();
    handlers.register_handler(string_handler);
    let mut dispatcher = handlers.into_dispatcher();

    let service_name = "special-service";
    let request = Request {
        extensions: Extensions::new(),
        service_name: service_name.to_string(),
        body: Box::new(StringRequest("test".to_string())) as Box<dyn Any + Send>,
        variables: HashMap::new(),
    };

    // The service name should be available to the handler
    let _response = dispatcher
        .ready()
        .await
        .unwrap()
        .call(request)
        .await
        .unwrap();
}

#[tokio::test]
async fn test_variables_preserved() {
    let expected_variables = {
        let mut vars = HashMap::new();
        vars.insert("key".to_string(), JsonValue::String("value".to_string()));
        vars
    };
    
    let string_handler = TowerTest::builder().service::<Request<StringRequest>, _, _, _>(move |mut h| {
        let expected = expected_variables.clone();
        async move {
            h.allow(1);
            let (req, resp) = h.next_request().await.expect("should receive request");
            // Verify variables are preserved
            assert_eq!(req.variables, expected);
            resp.send_response(Response {
                extensions: req.extensions,
                responses: Box::pin(stream::iter([json!("ok")])),
            });
        }
    });

    let mut handlers = RequestDispatcher::builder();
    handlers.register_handler(string_handler);
    let mut dispatcher = handlers.into_dispatcher();

    let mut variables = HashMap::new();
    variables.insert("key".to_string(), JsonValue::String("value".to_string()));

    let request = Request {
        extensions: Extensions::new(),
        service_name: "test".to_string(),
        body: Box::new(StringRequest("test".to_string())) as Box<dyn Any + Send>,
        variables,
    };

    // Variables should be available to the handler
    let _response = dispatcher
        .ready()
        .await
        .unwrap()
        .call(request)
        .await
        .unwrap();
}

#[tokio::test]
async fn test_stream_response() {
    let streaming_handler = TowerTest::builder().service::<Request<StringRequest>, _, _, _>(|mut h| async move {
        h.allow(1);
        let (req, resp) = h.next_request().await.expect("should receive request");
        let values = vec![
            json!("first"),
            json!("second"),
            json!("third"),
        ];
        resp.send_response(Response {
            extensions: req.extensions,
            responses: Box::pin(stream::iter(values)),
        });
    });

    let mut handlers = RequestDispatcher::builder();
    handlers.register_handler(streaming_handler);
    let mut dispatcher = handlers.into_dispatcher();

    let request = Request {
        extensions: Extensions::new(),
        service_name: "test".to_string(),
        body: Box::new(StringRequest("stream".to_string())) as Box<dyn Any + Send>,
        variables: HashMap::new(),
    };

    let response = dispatcher
        .ready()
        .await
        .unwrap()
        .call(request)
        .await
        .unwrap();
    let collected: Vec<_> = response.responses.collect().await;

    assert_eq!(collected.len(), 3);
    assert_eq!(collected[0], json!("first"));
    assert_eq!(collected[1], json!("second"));
    assert_eq!(collected[2], json!("third"));
}
