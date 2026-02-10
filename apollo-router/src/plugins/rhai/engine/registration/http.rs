use http::HeaderMap;
use http::Method;
use http::StatusCode;
use http::Uri;
use rhai::Engine;
use rhai::EvalAltResult;

use super::super::types::OptionDance;
use super::super::types::SharedMut;
use crate::plugins::rhai::rhai_http;

/// Register properties for HTTP layer request/response types.
///
/// Request: method, uri, headers, body (string).
/// Response: status_code, headers, body (string).
pub(super) fn register(engine: &mut Engine) {
    // Request: method
    engine.register_get_set(
        "method",
        |obj: &mut SharedMut<rhai_http::RhaiHttpRequest>| -> Result<Method, Box<EvalAltResult>> {
            Ok(obj.with_mut(|req| req.method.clone()))
        },
        |obj: &mut SharedMut<rhai_http::RhaiHttpRequest>, method: Method| {
            obj.with_mut(|req| req.method = method);
            Ok(())
        },
    );

    // Request: uri
    engine.register_get_set(
        "uri",
        |obj: &mut SharedMut<rhai_http::RhaiHttpRequest>| -> Result<Uri, Box<EvalAltResult>> {
            Ok(obj.with_mut(|req| req.uri.clone()))
        },
        |obj: &mut SharedMut<rhai_http::RhaiHttpRequest>, uri: Uri| {
            obj.with_mut(|req| req.uri = uri);
            Ok(())
        },
    );

    // Request: headers
    engine.register_get_set(
        "headers",
        |obj: &mut SharedMut<rhai_http::RhaiHttpRequest>| -> Result<HeaderMap, Box<EvalAltResult>> {
            Ok(obj.with_mut(|req| req.headers.clone()))
        },
        |obj: &mut SharedMut<rhai_http::RhaiHttpRequest>, headers: HeaderMap| {
            obj.with_mut(|req| req.headers = headers);
            Ok(())
        },
    );

    // Request: body (string)
    engine.register_get_set(
        "body",
        |obj: &mut SharedMut<rhai_http::RhaiHttpRequest>| -> Result<String, Box<EvalAltResult>> {
            Ok(obj.with_mut(|req| req.body.clone()))
        },
        |obj: &mut SharedMut<rhai_http::RhaiHttpRequest>, body: String| {
            obj.with_mut(|req| req.body = body);
            Ok(())
        },
    );

    // Response: status_code
    engine.register_get_set(
        "status_code",
        |obj: &mut SharedMut<rhai_http::RhaiHttpResponse>| -> Result<StatusCode, Box<EvalAltResult>> {
            Ok(obj.with_mut(|res| res.status_code))
        },
        |obj: &mut SharedMut<rhai_http::RhaiHttpResponse>, status_code: StatusCode| {
            obj.with_mut(|res| res.status_code = status_code);
            Ok(())
        },
    );

    // Response: headers
    engine.register_get_set(
        "headers",
        |obj: &mut SharedMut<rhai_http::RhaiHttpResponse>| -> Result<HeaderMap, Box<EvalAltResult>> {
            Ok(obj.with_mut(|res| res.headers.clone()))
        },
        |obj: &mut SharedMut<rhai_http::RhaiHttpResponse>, headers: HeaderMap| {
            obj.with_mut(|res| res.headers = headers);
            Ok(())
        },
    );

    // Response: body (string)
    engine.register_get_set(
        "body",
        |obj: &mut SharedMut<rhai_http::RhaiHttpResponse>| -> Result<String, Box<EvalAltResult>> {
            Ok(obj.with_mut(|res| res.body.clone()))
        },
        |obj: &mut SharedMut<rhai_http::RhaiHttpResponse>, body: String| {
            obj.with_mut(|res| res.body = body);
            Ok(())
        },
    );
}
