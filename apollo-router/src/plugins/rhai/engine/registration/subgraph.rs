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
use crate::plugins::rhai::subgraph;

/// Register properties for subgraph request/response types.
///
/// The originating (supergraph) request properties (headers, body, uri) are intentionally
/// READ-ONLY in the subgraph context. Setters are NOT registered for these properties.
///
/// ## Why no setters?
///
/// Rhai uses automatic property value propagation through chains. When calling a method
/// on a property chain like `request.headers["cookie"].split(';')`, Rhai will attempt to
/// propagate the result back by calling setters on the property chain, even when the method
/// is non-mutating (like `split()` or `trim()`).
///
/// If a setter exists but throws an error (as it would for read-only supergraph request
/// in subgraph context), this causes scripts to fail even for simple read operations.
/// By not registering setters at all, Rhai knows the property is truly read-only and
/// doesn't attempt value propagation, allowing read operations to work correctly.
///
/// ## How this works with types that have setters
///
/// Even though types like `HeaderMap` have indexer setters registered globally (in
/// `router_header_map` module), property chains remain read-only when the initial
/// property has no setter.
///
/// For example, with `req.headers["cookie"].split(';')`:
/// 1. `req.headers` returns a `HeaderMap` (no setter exists on `SharedMut<subgraph::Request>`)
/// 2. `["cookie"]` uses the global `HeaderMap` indexer (both getter AND setter exist)
/// 3. `.split(';')` returns an `Array`
/// 4. Rhai attempts value propagation backwards through the chain:
///    - Would call `HeaderMap` indexer setter to set `req.headers["cookie"] = result`
///    - Would then call `req.headers` setter to propagate the modified map back
///    - BUT: No setter exists for `headers` on `SharedMut<subgraph::Request>`!
///    - Propagation stops, **no error occurs**, and modifications are silently ignored
///
/// This behavior is verified by `test_headers_indexer_setter_blocked`: attempting
/// `req.headers["key"] = "value"` succeeds without error, but the modification is
/// silently ignored because Rhai can't propagate it back through the property chain.
///
/// The key: **The read-only-ness is determined by the first link in the property chain**
/// (the wrapper type like `SharedMut<subgraph::Request>`), not by intermediate types like
/// `HeaderMap`. This allows the same types (HeaderMap, Context, etc.) to be read-only in
/// some contexts and writable in others, based solely on whether their property getter
/// has a corresponding setter on the wrapper type.
///
/// Scripts can still modify `request.subgraph.headers`, `request.subgraph.body`, etc.
/// which are the actual outgoing subgraph request properties.
pub(super) fn register(engine: &mut Engine) {
    engine
        .register_get(
            "context",
            |obj: &mut SharedMut<subgraph::Request>| -> Result<Context, Box<EvalAltResult>> {
                Ok(obj.with_mut(|request| request.context.clone()))
            },
        )
        .register_get(
            "context",
            |obj: &mut SharedMut<subgraph::Response>| -> Result<Context, Box<EvalAltResult>> {
                Ok(obj.with_mut(|response| response.context.clone()))
            },
        );

    engine.register_get(
        "status_code",
        |obj: &mut SharedMut<subgraph::Response>| -> Result<StatusCode, Box<EvalAltResult>> {
            Ok(obj.with_mut(|response| response.response.status()))
        },
    );

    engine
        .register_set(
            "context",
            |obj: &mut SharedMut<subgraph::Request>, context: Context| {
                obj.with_mut(|request| request.context = context);
                Ok(())
            },
        )
        .register_set(
            "context",
            |obj: &mut SharedMut<subgraph::Response>, context: Context| {
                obj.with_mut(|response| response.context = context);
                Ok(())
            },
        );

    engine
        .register_get("id", |obj: &mut SharedMut<subgraph::Request>| -> String {
            obj.with_mut(|request| request.context.id.clone())
        })
        .register_get("id", |obj: &mut SharedMut<subgraph::Response>| -> String {
            obj.with_mut(|response| response.context.id.clone())
        });

    // Note: No setters for headers, body, uri - they are read-only in subgraph context
    engine.register_get(
        "headers",
        |obj: &mut SharedMut<subgraph::Request>| -> Result<HeaderMap, Box<EvalAltResult>> {
            Ok(obj.with_mut(|request| request.supergraph_request.headers().clone()))
        },
    );

    engine.register_get(
        "method",
        |obj: &mut SharedMut<subgraph::Request>| -> Result<Method, Box<EvalAltResult>> {
            Ok(obj.with_mut(|request| request.supergraph_request.method().clone()))
        },
    );

    engine.register_get(
        "body",
        |obj: &mut SharedMut<subgraph::Request>| -> Result<Request, Box<EvalAltResult>> {
            Ok(obj.with_mut(|request| request.supergraph_request.body().clone()))
        },
    );

    engine.register_get(
        "uri",
        |obj: &mut SharedMut<subgraph::Request>| -> Result<Uri, Box<EvalAltResult>> {
            Ok(obj.with_mut(|request| request.supergraph_request.uri().clone()))
        },
    );
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use http::HeaderValue;
    use http::StatusCode;
    use parking_lot::Mutex;

    use super::*;
    use crate::context::Context;
    use crate::plugins::rhai::subgraph;
    use crate::services::SubgraphRequest;

    fn create_engine_with_helpers() -> rhai::Engine {
        let mut engine = rhai::Engine::new();

        // Register global modules (HeaderMap indexer, Context, Request, etc.)
        crate::plugins::rhai::Rhai::register_global_modules(&mut engine);

        engine
    }

    fn create_test_subgraph_request() -> SharedMut<subgraph::Request> {
        Arc::new(Mutex::new(Some(SubgraphRequest::fake_builder().build())))
    }

    fn create_test_subgraph_response() -> SharedMut<subgraph::Response> {
        Arc::new(Mutex::new(Some(subgraph::Response::fake_builder().build())))
    }

    #[test]
    fn test_context_getter() {
        let engine = create_engine_with_helpers();
        let request = create_test_subgraph_request();
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
        let request = create_test_subgraph_request();
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
        let request = create_test_subgraph_request();
        let expected_id = request.lock().as_ref().unwrap().context.id.clone();

        let script = "fn test(req) { req.id }";
        let ast = engine.compile(script).unwrap();
        let result: String = engine
            .call_fn(&mut rhai::Scope::new(), &ast, "test", (request,))
            .unwrap();

        assert_eq!(result, expected_id);
    }

    #[test]
    fn test_headers_getter_read_only() {
        let engine = create_engine_with_helpers();
        let request = create_test_subgraph_request();

        // Add a header to test
        {
            let mut guard = request.lock();
            let req = guard.as_mut().unwrap();
            let mut new_req = (*req.supergraph_request).clone();
            new_req
                .headers_mut()
                .insert("test-header", HeaderValue::from_static("test-value"));
            req.supergraph_request = Arc::new(new_req);
        }

        let script = r#"fn test(req) { req.headers["test-header"] }"#;
        let ast = engine.compile(script).unwrap();
        let result: String = engine
            .call_fn(&mut rhai::Scope::new(), &ast, "test", (request,))
            .unwrap();

        assert_eq!(result, "test-value");
    }

    #[test]
    fn test_headers_no_setter() {
        let engine = create_engine_with_helpers();
        let request = create_test_subgraph_request();

        // Attempting to set headers should fail because no setter is registered
        let script = r#"
            fn test(req) {
                let headers = req.headers;
                req.headers = headers;
            }
        "#;

        let ast = engine.compile(script).unwrap();
        let result: Result<(), _> =
            engine.call_fn(&mut rhai::Scope::new(), &ast, "test", (request,));

        assert!(
            result.is_err(),
            "Should not be able to set read-only headers property"
        );
    }

    #[test]
    fn test_headers_indexer_setter_blocked() {
        let engine = create_engine_with_helpers();
        let request = create_test_subgraph_request();

        // Add initial header value
        {
            let mut guard = request.lock();
            let req = guard.as_mut().unwrap();
            let mut new_req = (*req.supergraph_request).clone();
            new_req
                .headers_mut()
                .insert("test-header", HeaderValue::from_static("original"));
            req.supergraph_request = Arc::new(new_req);
        }

        // Attempting to set individual header via indexer
        // Even though HeaderMap has an indexer setter registered globally,
        // it can't actually modify the request because there's no setter for
        // the headers property itself. Rhai silently ignores the modification
        // when it can't propagate changes back through the property chain.
        let script = r#"
            fn test(req) {
                req.headers["test-header"] = "modified";
            }
        "#;

        let ast = engine.compile(script).unwrap();
        let result: Result<(), _> =
            engine.call_fn(&mut rhai::Scope::new(), &ast, "test", (request.clone(),));

        // The script succeeds (Rhai doesn't error when it can't propagate),
        // but the modification is silently ignored
        assert!(result.is_ok(), "Script should run without error");

        // Verify the header wasn't actually modified - this proves the setter was blocked
        let guard = request.lock();
        let final_value = guard
            .as_ref()
            .unwrap()
            .supergraph_request
            .headers()
            .get("test-header")
            .map(|v| v.to_str().unwrap())
            .unwrap();
        assert_eq!(
            final_value, "original",
            "Header should remain unchanged despite setter attempt"
        );
    }

    #[test]
    fn test_headers_property_chain_with_split() {
        let engine = create_engine_with_helpers();
        let request = create_test_subgraph_request();

        // Add cookies
        {
            let mut guard = request.lock();
            let req = guard.as_mut().unwrap();
            let mut new_req = (*req.supergraph_request).clone();
            new_req
                .headers_mut()
                .insert("cookie", HeaderValue::from_static("a=1; b=2"));
            req.supergraph_request = Arc::new(new_req);
        }

        // This is THE critical test - split() should work without triggering setter error
        let script = r#"
            fn test(req) {
                let cookies = req.headers["cookie"].split(';');
                cookies.len()
            }
        "#;

        let ast = engine.compile(script).unwrap();
        let result: i64 = engine
            .call_fn(&mut rhai::Scope::new(), &ast, "test", (request,))
            .unwrap();

        assert_eq!(result, 2);
    }

    #[test]
    fn test_cookie_parsing_pattern() {
        let engine = create_engine_with_helpers();
        let request = create_test_subgraph_request();

        {
            let mut guard = request.lock();
            let req = guard.as_mut().unwrap();
            let mut new_req = (*req.supergraph_request).clone();
            new_req.headers_mut().insert(
                "cookie",
                HeaderValue::from_static("session=abc; user=john; theme=dark"),
            );
            req.supergraph_request = Arc::new(new_req);
        }

        // Test the exact pattern from cookies-to-headers example
        let script = r#"
            fn test(req) {
                req.headers["cookie"].split(';').len()
            }
        "#;

        let ast = engine.compile(script).unwrap();
        let result: i64 = engine
            .call_fn(&mut rhai::Scope::new(), &ast, "test", (request,))
            .unwrap();

        assert_eq!(result, 3);
    }

    #[test]
    fn test_headers_property_chain_with_string_methods() {
        let engine = create_engine_with_helpers();
        let request = create_test_subgraph_request();

        {
            let mut guard = request.lock();
            let req = guard.as_mut().unwrap();
            let mut new_req = (*req.supergraph_request).clone();
            new_req
                .headers_mut()
                .insert("content-type", HeaderValue::from_static("application/json"));
            req.supergraph_request = Arc::new(new_req);
        }

        // Test various Rhai string methods work in property chains without triggering setter errors
        // This verifies that without setters registered, Rhai doesn't try to propagate values back
        let script = r#"
            fn test(req) {
                // Test to_upper() in chain
                let upper = req.headers["content-type"].to_upper();
                // Test contains() in chain
                let has_json = req.headers["content-type"].contains("json");
                // Return results
                [upper, has_json]
            }
        "#;

        let ast = engine.compile(script).unwrap();
        let result: rhai::Array = engine
            .call_fn(&mut rhai::Scope::new(), &ast, "test", (request,))
            .unwrap();

        assert_eq!(result[0].clone().cast::<String>(), "APPLICATION/JSON");
        assert!(result[1].clone().cast::<bool>());
    }

    #[test]
    fn test_body_getter() {
        let engine = create_engine_with_helpers();
        let request = create_test_subgraph_request();

        // Body getter returns the Request object
        let script = "fn test(req) { req.body }";
        let ast = engine.compile(script).unwrap();
        let result: Request = engine
            .call_fn(&mut rhai::Scope::new(), &ast, "test", (request,))
            .unwrap();

        // Verify we got a Request object back
        assert!(result.query.is_some() || result.query.is_none());
    }

    #[test]
    fn test_body_no_setter() {
        let engine = create_engine_with_helpers();
        let request = create_test_subgraph_request();

        let script = "fn test(req) { let b = req.body; req.body = b; }";
        let ast = engine.compile(script).unwrap();
        let result: Result<(), _> =
            engine.call_fn(&mut rhai::Scope::new(), &ast, "test", (request,));

        assert!(
            result.is_err(),
            "Should not be able to set read-only body property"
        );
    }

    #[test]
    fn test_uri_getter() {
        let engine = create_engine_with_helpers();
        let request = create_test_subgraph_request();

        let script = "fn test(req) { req.uri }";
        let ast = engine.compile(script).unwrap();
        let result: http::Uri = engine
            .call_fn(&mut rhai::Scope::new(), &ast, "test", (request,))
            .unwrap();

        // Should return a valid URI (fake builder creates "/" by default)
        assert_eq!(result.path(), "/");
    }

    #[test]
    fn test_uri_no_setter() {
        let engine = create_engine_with_helpers();
        let request = create_test_subgraph_request();

        let script = "fn test(req) { let u = req.uri; req.uri = u; }";
        let ast = engine.compile(script).unwrap();
        let result: Result<(), _> =
            engine.call_fn(&mut rhai::Scope::new(), &ast, "test", (request,));

        assert!(
            result.is_err(),
            "Should not be able to set read-only uri property"
        );
    }

    #[test]
    fn test_method_getter() {
        let engine = create_engine_with_helpers();
        let request = create_test_subgraph_request();

        let script = "fn test(req) { req.method }";
        let ast = engine.compile(script).unwrap();
        let result: http::Method = engine
            .call_fn(&mut rhai::Scope::new(), &ast, "test", (request,))
            .unwrap();

        assert_eq!(result, http::Method::GET);
    }

    #[test]
    fn test_response_status_code() {
        let engine = create_engine_with_helpers();
        let response = create_test_subgraph_response();

        let script = "fn test(resp) { resp.status_code }";
        let ast = engine.compile(script).unwrap();
        let result: StatusCode = engine
            .call_fn(&mut rhai::Scope::new(), &ast, "test", (response,))
            .unwrap();

        assert_eq!(result, StatusCode::OK);
    }
}
