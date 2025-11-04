use http::HeaderMap;
use http::Method;
use http::StatusCode;
use http::Uri;
use rhai::Engine;
use rhai::EvalAltResult;

use super::super::types::OptionDance;
use super::super::types::SharedMut;
use crate::context::Context;
use crate::graphql::Request;
use crate::plugins::rhai::supergraph;

/// Register properties for supergraph request/response types.
///
/// All originating request properties (headers, body, uri) are mutable in the supergraph context.
pub(super) fn register(engine: &mut Engine) {
    engine
        .register_get(
            "context",
            |obj: &mut SharedMut<supergraph::Request>| -> Result<Context, Box<EvalAltResult>> {
                Ok(obj.with_mut(|request| request.context.clone()))
            },
        )
        .register_get(
            "context",
            |obj: &mut SharedMut<supergraph::Response>| -> Result<Context, Box<EvalAltResult>> {
                Ok(obj.with_mut(|response| response.context.clone()))
            },
        );

    engine.register_get(
        "status_code",
        |obj: &mut SharedMut<supergraph::Response>| -> Result<StatusCode, Box<EvalAltResult>> {
            Ok(obj.with_mut(|response| response.response.status()))
        },
    );

    engine
        .register_set(
            "context",
            |obj: &mut SharedMut<supergraph::Request>, context: Context| {
                obj.with_mut(|request| request.context = context);
                Ok(())
            },
        )
        .register_set(
            "context",
            |obj: &mut SharedMut<supergraph::Response>, context: Context| {
                obj.with_mut(|response| response.context = context);
                Ok(())
            },
        );

    engine
        .register_get("id", |obj: &mut SharedMut<supergraph::Request>| -> String {
            obj.with_mut(|request| request.context.id.clone())
        })
        .register_get(
            "id",
            |obj: &mut SharedMut<supergraph::Response>| -> String {
                obj.with_mut(|response| response.context.id.clone())
            },
        );

    engine.register_get_set(
        "headers",
        |obj: &mut SharedMut<supergraph::Request>| -> Result<HeaderMap, Box<EvalAltResult>> {
            Ok(obj.with_mut(|request| request.supergraph_request.headers().clone()))
        },
        |obj: &mut SharedMut<supergraph::Request>, headers: HeaderMap| {
            obj.with_mut(|request| *request.supergraph_request.headers_mut() = headers);
            Ok(())
        },
    );

    engine.register_get(
        "method",
        |obj: &mut SharedMut<supergraph::Request>| -> Result<Method, Box<EvalAltResult>> {
            Ok(obj.with_mut(|request| request.supergraph_request.method().clone()))
        },
    );

    engine.register_get_set(
        "body",
        |obj: &mut SharedMut<supergraph::Request>| -> Result<Request, Box<EvalAltResult>> {
            Ok(obj.with_mut(|request| request.supergraph_request.body().clone()))
        },
        |obj: &mut SharedMut<supergraph::Request>, body: Request| {
            obj.with_mut(|request| *request.supergraph_request.body_mut() = body);
            Ok(())
        },
    );

    engine.register_get_set(
        "uri",
        |obj: &mut SharedMut<supergraph::Request>| -> Result<Uri, Box<EvalAltResult>> {
            Ok(obj.with_mut(|request| request.supergraph_request.uri().clone()))
        },
        |obj: &mut SharedMut<supergraph::Request>, uri: Uri| {
            obj.with_mut(|request| *request.supergraph_request.uri_mut() = uri);
            Ok(())
        },
    );
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use http::StatusCode;
    use parking_lot::Mutex;

    use super::*;
    use crate::context::Context;
    use crate::plugins::rhai::supergraph;
    use crate::services::SupergraphRequest;

    fn create_engine_with_helpers() -> rhai::Engine {
        let mut engine = rhai::Engine::new();

        // Register global modules (HeaderMap indexer, Context, Request, etc.)
        crate::plugins::rhai::Rhai::register_global_modules(&mut engine);

        engine
    }

    fn create_test_supergraph_request() -> SharedMut<supergraph::Request> {
        Arc::new(Mutex::new(Some(
            SupergraphRequest::fake_builder().build().unwrap(),
        )))
    }

    fn create_test_supergraph_response() -> SharedMut<supergraph::Response> {
        Arc::new(Mutex::new(Some(
            supergraph::Response::fake_builder().build().unwrap(),
        )))
    }

    #[test]
    fn test_context_getter() {
        let engine = create_engine_with_helpers();
        let request = create_test_supergraph_request();
        let expected_id = request.lock().as_ref().unwrap().context.id.clone();

        // Context getter returns the full context object
        let script = "fn test(req) { req.context }";
        let ast = engine.compile(script).unwrap();
        let result: Context = engine
            .call_fn(&mut rhai::Scope::new(), &ast, "test", (request,))
            .unwrap();

        assert_eq!(result.id, expected_id);
    }

    #[test]
    fn test_context_setter() {
        let engine = create_engine_with_helpers();
        let request = create_test_supergraph_request();
        let new_context = Context::new();
        let expected_id = new_context.id.clone();

        let script = "fn test(req, ctx) { req.context = ctx; }";
        let ast = engine.compile(script).unwrap();
        let _: () = engine
            .call_fn(
                &mut rhai::Scope::new(),
                &ast,
                "test",
                (request.clone(), new_context),
            )
            .unwrap();

        assert_eq!(request.lock().as_ref().unwrap().context.id, expected_id);
    }

    #[test]
    fn test_id_getter() {
        let engine = create_engine_with_helpers();
        let request = create_test_supergraph_request();
        let expected_id = request.lock().as_ref().unwrap().context.id.clone();

        let script = "fn test(req) { req.id }";
        let ast = engine.compile(script).unwrap();
        let result: String = engine
            .call_fn(&mut rhai::Scope::new(), &ast, "test", (request,))
            .unwrap();

        assert_eq!(result, expected_id);
    }

    #[test]
    fn test_headers_getter() {
        let engine = create_engine_with_helpers();
        let request = create_test_supergraph_request();

        let script = r#"fn test(req) { req.headers["content-type"] }"#;
        let ast = engine.compile(script).unwrap();
        let result: String = engine
            .call_fn(&mut rhai::Scope::new(), &ast, "test", (request,))
            .unwrap();

        assert_eq!(result, "application/json");
    }

    #[test]
    fn test_headers_setter() {
        let engine = create_engine_with_helpers();
        let request = create_test_supergraph_request();

        let script = r#"
            fn test(req) {
                let headers = req.headers;
                req.headers = headers;
            }
        "#;

        let ast = engine.compile(script).unwrap();
        // Should succeed - headers are mutable in supergraph context
        let _: () = engine
            .call_fn(&mut rhai::Scope::new(), &ast, "test", (request.clone(),))
            .unwrap();
    }

    #[test]
    fn test_method_getter() {
        let engine = create_engine_with_helpers();
        let request = create_test_supergraph_request();

        let script = "fn test(req) { req.method }";
        let ast = engine.compile(script).unwrap();
        let result: http::Method = engine
            .call_fn(&mut rhai::Scope::new(), &ast, "test", (request,))
            .unwrap();

        assert_eq!(result, http::Method::POST);
    }

    #[test]
    fn test_body_getter() {
        let engine = create_engine_with_helpers();
        let request = create_test_supergraph_request();

        let script = "fn test(req) { req.body }";
        let ast = engine.compile(script).unwrap();
        let _result: crate::graphql::Request = engine
            .call_fn(&mut rhai::Scope::new(), &ast, "test", (request,))
            .unwrap();

        // Test succeeds if we can retrieve body without errors
    }

    #[test]
    fn test_body_setter() {
        let engine = create_engine_with_helpers();
        let request = create_test_supergraph_request();

        let script = "fn test(req, body) { req.body = body; }";
        let ast = engine.compile(script).unwrap();

        let new_body = crate::graphql::Request::builder()
            .query("{ modified }")
            .build();

        let _: () = engine
            .call_fn(
                &mut rhai::Scope::new(),
                &ast,
                "test",
                (request.clone(), new_body.clone()),
            )
            .unwrap();

        let binding = request.lock();
        let body = binding.as_ref().unwrap().supergraph_request.body();
        assert_eq!(body.query, new_body.query);
    }

    #[test]
    fn test_uri_getter() {
        let engine = create_engine_with_helpers();
        let request = create_test_supergraph_request();

        let script = "fn test(req) { req.uri }";
        let ast = engine.compile(script).unwrap();
        let result: http::Uri = engine
            .call_fn(&mut rhai::Scope::new(), &ast, "test", (request,))
            .unwrap();

        assert_eq!(result.path(), "/");
    }

    #[test]
    fn test_uri_setter() {
        let engine = create_engine_with_helpers();
        let request = create_test_supergraph_request();

        let script = "fn test(req, uri) { req.uri = uri; }";
        let ast = engine.compile(script).unwrap();

        let new_uri = http::Uri::from_static("http://example.com/graphql");
        let _: () = engine
            .call_fn(
                &mut rhai::Scope::new(),
                &ast,
                "test",
                (request.clone(), new_uri.clone()),
            )
            .unwrap();

        let binding = request.lock();
        let uri = binding.as_ref().unwrap().supergraph_request.uri();
        assert_eq!(uri.path(), "/graphql");
    }

    #[test]
    fn test_response_status_code() {
        let engine = create_engine_with_helpers();
        let response = create_test_supergraph_response();

        let script = "fn test(resp) { resp.status_code }";
        let ast = engine.compile(script).unwrap();
        let result: StatusCode = engine
            .call_fn(&mut rhai::Scope::new(), &ast, "test", (response,))
            .unwrap();

        assert_eq!(result, StatusCode::OK);
    }

    #[test]
    fn test_header_trim_property_chain_mutation() {
        let mut engine = create_engine_with_helpers();

        // Register trim function
        engine.register_fn("trim", |s: &str| -> String { s.trim().to_string() });

        // Build request with header that has whitespace
        let request = Arc::new(Mutex::new(Some(
            SupergraphRequest::fake_builder()
                .header("x-test", "  value-with-spaces  ")
                .build()
                .unwrap(),
        )));

        // This pattern should work: reading header, trimming it, and Rhai should
        // propagate the trimmed value back via the setter
        let script = r#"
            fn test(req) {
                // Get the header value and trim it
                // With property value propagation, this should write back the trimmed value
                req.headers["x-test"] = req.headers["x-test"].trim();

                // Verify it was trimmed
                req.headers["x-test"]
            }
        "#;

        let ast = engine.compile(script).unwrap();
        let result: String = engine
            .call_fn(&mut rhai::Scope::new(), &ast, "test", (request.clone(),))
            .unwrap();

        // Should be trimmed
        assert_eq!(result, "value-with-spaces");

        // Verify the header was actually mutated
        let guard = request.lock();
        let req = guard.as_ref().unwrap();
        let header_value = req.supergraph_request.headers().get("x-test").unwrap();
        assert_eq!(header_value.to_str().unwrap(), "value-with-spaces");
    }
}
