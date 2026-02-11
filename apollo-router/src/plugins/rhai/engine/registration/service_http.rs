//! Register properties for service HTTP layer (service_http) request/response types.

use http::HeaderMap;
use http::Method;
use http::StatusCode;
use http::Uri;
use rhai::Engine;
use rhai::EvalAltResult;

use super::super::types::OptionDance;
use super::super::types::SharedMut;
use crate::plugins::rhai::rhai_http;

pub(super) fn register(engine: &mut Engine) {
    // Service HTTP Request: method, uri, headers, body, service_name
    engine.register_get_set(
        "method",
        |obj: &mut SharedMut<rhai_http::RhaiServiceHttpRequest>| -> Result<Method, Box<EvalAltResult>> {
            Ok(obj.with_mut(|req| req.method.clone()))
        },
        |obj: &mut SharedMut<rhai_http::RhaiServiceHttpRequest>, method: Method| {
            obj.with_mut(|req| req.method = method);
            Ok(())
        },
    );
    engine.register_get_set(
        "uri",
        |obj: &mut SharedMut<rhai_http::RhaiServiceHttpRequest>| -> Result<Uri, Box<EvalAltResult>> {
            Ok(obj.with_mut(|req| req.uri.clone()))
        },
        |obj: &mut SharedMut<rhai_http::RhaiServiceHttpRequest>, uri: Uri| {
            obj.with_mut(|req| req.uri = uri);
            Ok(())
        },
    );
    engine.register_get_set(
        "headers",
        |obj: &mut SharedMut<rhai_http::RhaiServiceHttpRequest>| -> Result<HeaderMap, Box<EvalAltResult>> {
            Ok(obj.with_mut(|req| req.headers.clone()))
        },
        |obj: &mut SharedMut<rhai_http::RhaiServiceHttpRequest>, headers: HeaderMap| {
            obj.with_mut(|req| req.headers = headers);
            Ok(())
        },
    );
    engine.register_get_set(
        "body",
        |obj: &mut SharedMut<rhai_http::RhaiServiceHttpRequest>| -> Result<String, Box<EvalAltResult>> {
            Ok(obj.with_mut(|req| req.body.clone()))
        },
        |obj: &mut SharedMut<rhai_http::RhaiServiceHttpRequest>, body: String| {
            obj.with_mut(|req| req.body = body);
            Ok(())
        },
    );
    engine.register_get_set(
        "service_name",
        |obj: &mut SharedMut<rhai_http::RhaiServiceHttpRequest>| -> Result<Option<String>, Box<EvalAltResult>> {
            Ok(obj.with_mut(|req| req.service_name.clone()))
        },
        |obj: &mut SharedMut<rhai_http::RhaiServiceHttpRequest>, service_name: Option<String>| {
            obj.with_mut(|req| req.service_name = service_name);
            Ok(())
        },
    );

    // Service HTTP Response: status_code, headers, body
    engine.register_get_set(
        "status_code",
        |obj: &mut SharedMut<rhai_http::RhaiServiceHttpResponse>| -> Result<StatusCode, Box<EvalAltResult>> {
            Ok(obj.with_mut(|res| res.status_code))
        },
        |obj: &mut SharedMut<rhai_http::RhaiServiceHttpResponse>, status_code: StatusCode| {
            obj.with_mut(|res| res.status_code = status_code);
            Ok(())
        },
    );
    engine.register_get_set(
        "headers",
        |obj: &mut SharedMut<rhai_http::RhaiServiceHttpResponse>| -> Result<HeaderMap, Box<EvalAltResult>> {
            Ok(obj.with_mut(|res| res.headers.clone()))
        },
        |obj: &mut SharedMut<rhai_http::RhaiServiceHttpResponse>, headers: HeaderMap| {
            obj.with_mut(|res| res.headers = headers);
            Ok(())
        },
    );
    engine.register_get_set(
        "body",
        |obj: &mut SharedMut<rhai_http::RhaiServiceHttpResponse>| -> Result<String, Box<EvalAltResult>> {
            Ok(obj.with_mut(|res| res.body.clone()))
        },
        |obj: &mut SharedMut<rhai_http::RhaiServiceHttpResponse>, body: String| {
            obj.with_mut(|res| res.body = body);
            Ok(())
        },
    );
}
