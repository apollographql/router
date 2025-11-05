use http::HeaderMap;
use http::Method;
use http::Uri;
use rhai::Engine;
use rhai::EvalAltResult;

use super::super::types::OptionDance;
use super::super::types::SharedMut;
use crate::context::Context;
use crate::plugins::rhai::router;

/// Register properties for router request/response types.
///
/// All originating request properties (headers, body, uri) are mutable in the router context.
pub(super) fn register(engine: &mut Engine) {
    engine
        .register_get(
            "context",
            |obj: &mut SharedMut<router::FirstRequest>| -> Result<Context, Box<EvalAltResult>> {
                Ok(obj.with_mut(|request| request.context.clone()))
            },
        )
        .register_get(
            "context",
            |obj: &mut SharedMut<router::ChunkedRequest>| -> Result<Context, Box<EvalAltResult>> {
                Ok(obj.with_mut(|request| request.context.clone()))
            },
        )
        .register_get(
            "context",
            |obj: &mut SharedMut<router::FirstResponse>| -> Result<Context, Box<EvalAltResult>> {
                Ok(obj.with_mut(|response| response.context.clone()))
            },
        )
        .register_get(
            "context",
            |obj: &mut SharedMut<router::DeferredResponse>| -> Result<Context, Box<EvalAltResult>> {
                Ok(obj.with_mut(|response| response.context.clone()))
            },
        );

    engine
        .register_set(
            "context",
            |obj: &mut SharedMut<router::FirstRequest>, context: Context| {
                obj.with_mut(|request| request.context = context);
                Ok(())
            },
        )
        .register_set(
            "context",
            |obj: &mut SharedMut<router::ChunkedRequest>, context: Context| {
                obj.with_mut(|request| request.context = context);
                Ok(())
            },
        )
        .register_set(
            "context",
            |obj: &mut SharedMut<router::FirstResponse>, context: Context| {
                obj.with_mut(|response| response.context = context);
                Ok(())
            },
        )
        .register_set(
            "context",
            |obj: &mut SharedMut<router::DeferredResponse>, context: Context| {
                obj.with_mut(|response| response.context = context);
                Ok(())
            },
        );

    engine
        .register_get(
            "id",
            |obj: &mut SharedMut<router::FirstRequest>| -> String {
                obj.with_mut(|request| request.context.id.clone())
            },
        )
        .register_get(
            "id",
            |obj: &mut SharedMut<router::ChunkedRequest>| -> String {
                obj.with_mut(|request| request.context.id.clone())
            },
        )
        .register_get(
            "id",
            |obj: &mut SharedMut<router::FirstResponse>| -> String {
                obj.with_mut(|response| response.context.id.clone())
            },
        )
        .register_get(
            "id",
            |obj: &mut SharedMut<router::DeferredResponse>| -> String {
                obj.with_mut(|response| response.context.id.clone())
            },
        );

    engine.register_get_set(
        "headers",
        |obj: &mut SharedMut<router::FirstRequest>| -> Result<HeaderMap, Box<EvalAltResult>> {
            Ok(obj.with_mut(|request| request.request.headers().clone()))
        },
        |obj: &mut SharedMut<router::FirstRequest>, headers: HeaderMap| {
            obj.with_mut(|request| *request.request.headers_mut() = headers);
            Ok(())
        },
    );

    engine.register_get_set(
        "headers",
        |obj: &mut SharedMut<router::FirstResponse>| -> Result<HeaderMap, Box<EvalAltResult>> {
            Ok(obj.with_mut(|response| response.response.headers().clone()))
        },
        |obj: &mut SharedMut<router::FirstResponse>, headers: HeaderMap| {
            obj.with_mut(|response| *response.response.headers_mut() = headers);
            Ok(())
        },
    );

    engine.register_get(
        "uri",
        |obj: &mut SharedMut<router::FirstRequest>| -> Result<Uri, Box<EvalAltResult>> {
            Ok(obj.with_mut(|request| request.request.uri().clone()))
        },
    );

    engine.register_get(
        "uri",
        |obj: &mut SharedMut<router::Request>| -> Result<Uri, Box<EvalAltResult>> {
            Ok(obj.with_mut(|request| request.router_request.uri().clone()))
        },
    );

    engine.register_set(
        "uri",
        |obj: &mut SharedMut<router::FirstRequest>, uri: Uri| {
            obj.with_mut(|request| *request.request.uri_mut() = uri);
            Ok(())
        },
    );

    engine.register_set("uri", |obj: &mut SharedMut<router::Request>, uri: Uri| {
        obj.with_mut(|request| *request.router_request.uri_mut() = uri);
        Ok(())
    });

    engine.register_get(
        "method",
        |obj: &mut SharedMut<router::FirstRequest>| -> Result<Method, Box<EvalAltResult>> {
            Ok(obj.with_mut(|request| request.request.method().clone()))
        },
    );
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use http::HeaderMap;
    use parking_lot::Mutex;

    use super::*;
    use crate::context::Context;
    use crate::plugins::rhai::engine::registration;
    use crate::plugins::rhai::router;
    use crate::services::RouterRequest;
    use crate::services::RouterResponse;

    fn create_engine_with_helpers() -> rhai::Engine {
        let mut engine = rhai::Engine::new();

        // Register global modules (HeaderMap indexer, Context, Request, etc.)
        crate::plugins::rhai::Rhai::register_global_modules(&mut engine);
        // Add common getter/setters for different types
        registration::register(&mut engine);

        engine
    }

    fn create_test_first_request() -> SharedMut<router::FirstRequest> {
        let router_request = RouterRequest::fake_builder().build().unwrap();
        let context = router_request.context.clone();
        let http_request = router_request.router_request.map(|_| ());

        Arc::new(Mutex::new(Some(router::FirstRequest {
            context,
            request: http_request,
        })))
    }

    fn create_test_first_response() -> SharedMut<router::FirstResponse> {
        let router_response = RouterResponse::fake_builder().build().unwrap();
        let context = router_response.context.clone();
        let http_response = router_response.response.map(|_| ());

        Arc::new(Mutex::new(Some(router::FirstResponse {
            context,
            response: http_response,
        })))
    }

    #[test]
    fn test_first_request_context_getter() {
        let engine = create_engine_with_helpers();
        let request = create_test_first_request();
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
    fn test_first_request_context_setter() {
        let engine = create_engine_with_helpers();
        let request = create_test_first_request();
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
    fn test_first_request_id_getter() {
        let engine = create_engine_with_helpers();
        let request = create_test_first_request();
        let expected_id = request.lock().as_ref().unwrap().context.id.clone();

        let script = "fn test(req) { req.id }";
        let ast = engine.compile(script).unwrap();
        let result: String = engine
            .call_fn(&mut rhai::Scope::new(), &ast, "test", (request,))
            .unwrap();

        assert_eq!(result, expected_id);
    }

    #[test]
    fn test_first_request_headers_getter() {
        let engine = create_engine_with_helpers();
        let request = create_test_first_request();

        let script = "fn test(req) { req.headers }";
        let ast = engine.compile(script).unwrap();
        let _result: HeaderMap = engine
            .call_fn(&mut rhai::Scope::new(), &ast, "test", (request,))
            .unwrap();

        // Test succeeds if we can retrieve headers without errors
    }

    #[test]
    fn test_first_request_headers_setter() {
        let engine = create_engine_with_helpers();
        let request = create_test_first_request();

        let script = r#"
            fn test(req) {
                let headers = req.headers;
                req.headers = headers;
            }
        "#;

        let ast = engine.compile(script).unwrap();
        // Should succeed - headers are mutable in router context
        let _: () = engine
            .call_fn(&mut rhai::Scope::new(), &ast, "test", (request.clone(),))
            .unwrap();
    }

    #[test]
    fn test_first_request_method_getter() {
        let engine = create_engine_with_helpers();
        let request = create_test_first_request();

        let script = "fn test(req) { req.method }";
        let ast = engine.compile(script).unwrap();
        let result: http::Method = engine
            .call_fn(&mut rhai::Scope::new(), &ast, "test", (request,))
            .unwrap();

        assert_eq!(result, http::Method::GET);
    }

    #[test]
    fn test_first_request_uri_getter() {
        let engine = create_engine_with_helpers();
        let request = create_test_first_request();

        let script = "fn test(req) { req.uri }";
        let ast = engine.compile(script).unwrap();
        let result: http::Uri = engine
            .call_fn(&mut rhai::Scope::new(), &ast, "test", (request,))
            .unwrap();

        assert_eq!(result.path(), "/");
    }

    #[test]
    fn test_first_request_uri_setter() {
        let engine = create_engine_with_helpers();
        let request = create_test_first_request();

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
        let uri = binding.as_ref().unwrap().request.uri();
        assert_eq!(uri.path(), "/graphql");
    }

    #[test]
    fn test_first_response_context_getter() {
        let engine = create_engine_with_helpers();
        let response = create_test_first_response();
        let expected_id = response.lock().as_ref().unwrap().context.id.clone();

        // Context getter returns the full context object
        let script = "fn test(resp) { resp.context }";
        let ast = engine.compile(script).unwrap();
        let result: Context = engine
            .call_fn(&mut rhai::Scope::new(), &ast, "test", (response,))
            .unwrap();

        assert_eq!(result.id, expected_id);
    }

    #[test]
    fn test_first_response_context_setter() {
        let engine = create_engine_with_helpers();
        let response = create_test_first_response();
        let new_context = Context::new();
        let expected_id = new_context.id.clone();

        let script = "fn test(resp, ctx) { resp.context = ctx; }";
        let ast = engine.compile(script).unwrap();
        let _: () = engine
            .call_fn(
                &mut rhai::Scope::new(),
                &ast,
                "test",
                (response.clone(), new_context),
            )
            .unwrap();

        assert_eq!(response.lock().as_ref().unwrap().context.id, expected_id);
    }

    #[test]
    fn test_first_response_id_getter() {
        let engine = create_engine_with_helpers();
        let response = create_test_first_response();
        let expected_id = response.lock().as_ref().unwrap().context.id.clone();

        let script = "fn test(resp) { resp.id }";
        let ast = engine.compile(script).unwrap();
        let result: String = engine
            .call_fn(&mut rhai::Scope::new(), &ast, "test", (response,))
            .unwrap();

        assert_eq!(result, expected_id);
    }

    #[test]
    fn test_first_response_headers_getter() {
        let engine = create_engine_with_helpers();
        let response = create_test_first_response();

        let script = "fn test(resp) { resp.headers }";
        let ast = engine.compile(script).unwrap();
        let _result: HeaderMap = engine
            .call_fn(&mut rhai::Scope::new(), &ast, "test", (response,))
            .unwrap();

        // Test succeeds if we can retrieve headers without errors
    }

    #[test]
    fn test_first_response_headers_setter() {
        let engine = create_engine_with_helpers();
        let response = create_test_first_response();

        let script = r#"
            fn test(resp) {
                let headers = resp.headers;
                resp.headers = headers;
            }
        "#;

        let ast = engine.compile(script).unwrap();
        // Should succeed - headers are mutable in router context
        let _: () = engine
            .call_fn(&mut rhai::Scope::new(), &ast, "test", (response.clone(),))
            .unwrap();
    }
}
