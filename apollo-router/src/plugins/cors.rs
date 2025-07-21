//! Cross Origin Resource Sharing (CORS) plugin

use std::future::Future;
use std::pin::Pin;
use std::task::Context;
use std::task::Poll;

use http::Request;
use http::Response;
use http::header::ACCESS_CONTROL_ALLOW_CREDENTIALS;
use http::header::ACCESS_CONTROL_ALLOW_HEADERS;
use http::header::ACCESS_CONTROL_ALLOW_METHODS;
use http::header::ACCESS_CONTROL_ALLOW_ORIGIN;
use http::header::ACCESS_CONTROL_EXPOSE_HEADERS;
use http::header::ACCESS_CONTROL_MAX_AGE;
use http::header::ACCESS_CONTROL_REQUEST_HEADERS;
use http::header::ORIGIN;
use http::header::VARY;
use tower::Layer;
use tower::Service;

use crate::configuration::cors::Cors;
use crate::configuration::cors::Policy;

/// Our custom CORS layer that supports per-origin configuration
#[derive(Clone, Debug)]
pub(crate) struct CorsLayer {
    config: Cors,
}

impl CorsLayer {
    pub(crate) fn new(config: Cors) -> Result<Self, String> {
        // Ensure configuration is valid before creating CorsLayer
        config.ensure_usable_cors_rules()?;

        // Validate global headers
        if !config.allow_headers.is_empty() {
            parse_values::<http::HeaderName>(&config.allow_headers, "allow header name")?;
        }

        // Validate global methods
        parse_values::<http::Method>(&config.methods, "method")?;

        // Validate global expose headers
        if let Some(headers) = &config.expose_headers {
            parse_values::<http::HeaderName>(headers, "expose header name")?;
        }

        // Validate origin configurations
        if let Some(policies) = &config.policies {
            for policy in policies {
                // Validate origin URLs
                for origin in &policy.origins {
                    http::HeaderValue::from_str(origin).map_err(|_| {
                        format!(
                            "origin '{}' is not valid: failed to parse header value",
                            origin
                        )
                    })?;
                }

                // Validate origin-specific headers
                if !policy.allow_headers.is_empty() {
                    parse_values::<http::HeaderName>(&policy.allow_headers, "allow header name")?;
                }

                // Validate origin-specific methods
                if let Some(methods) = &policy.methods {
                    if !methods.is_empty() {
                        parse_values::<http::Method>(methods, "method")?;
                    }
                }

                // Validate origin-specific expose headers
                if !policy.expose_headers.is_empty() {
                    parse_values::<http::HeaderName>(&policy.expose_headers, "expose header name")?;
                }
            }
        }

        Ok(Self { config })
    }
}

impl<S> Layer<S> for CorsLayer {
    type Service = CorsService<S>;

    fn layer(&self, service: S) -> Self::Service {
        CorsService {
            inner: service,
            config: self.config.clone(),
        }
    }
}

/// Our custom CORS service that handles per-origin configuration
#[derive(Clone, Debug)]
pub(crate) struct CorsService<S> {
    inner: S,
    config: Cors,
}

impl<S, ReqBody, ResBody> Service<Request<ReqBody>> for CorsService<S>
where
    S: Service<Request<ReqBody>, Response = Response<ResBody>> + Send + 'static,
    S::Future: Send + 'static,
    S::Error: Send + 'static,
    ReqBody: Send + 'static,
    ResBody: Send + 'static + Default,
{
    type Response = S::Response;
    type Error = S::Error;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, req: Request<ReqBody>) -> Self::Future {
        let request_origin = req.headers().get(ORIGIN).cloned();
        let is_preflight = req.method() == http::Method::OPTIONS;
        let config = self.config.clone();
        let request_headers = req.headers().get(ACCESS_CONTROL_REQUEST_HEADERS).cloned();

        // Intercept OPTIONS requests and return preflight response directly
        if is_preflight {
            let mut response = Response::builder()
                .status(http::StatusCode::OK)
                .body(ResBody::default())
                .unwrap();
            // Find matching origin configuration
            let policy = Self::find_matching_policy(&config, &request_origin);
            // Add CORS headers for preflight
            Self::add_cors_headers(
                &mut response,
                &config,
                &policy,
                &request_origin,
                true,
                request_headers,
            );
            return Box::pin(async move { Ok(response) });
        }

        let fut = self.inner.call(req);
        Box::pin(async move {
            let mut response = fut.await?;
            // Find matching origin configuration
            let policy = Self::find_matching_policy(&config, &request_origin);
            // Add CORS headers for non-preflight
            Self::add_cors_headers(
                &mut response,
                &config,
                &policy,
                &request_origin,
                false,
                request_headers,
            );
            Ok(response)
        })
    }
}

impl<S> CorsService<S> {
    /// Find the matching policy for a given origin
    fn find_matching_policy<'a>(
        config: &'a Cors,
        origin: &'a Option<http::HeaderValue>,
    ) -> Option<&'a Policy> {
        let origin_str = origin.as_ref()?.to_str().ok()?;

        if let Some(policies) = &config.policies {
            for policy in policies.iter() {
                for url in &policy.origins {
                    if url == origin_str {
                        return Some(policy);
                    }
                }

                if !policy.match_origins.is_empty() {
                    for regex in &policy.match_origins {
                        if regex.is_match(origin_str) {
                            return Some(policy);
                        }
                    }
                }
            }
        }

        None
    }

    /// Add CORS headers to the response
    fn add_cors_headers<ResBody>(
        response: &mut Response<ResBody>,
        config: &Cors,
        policy: &Option<&Policy>,
        request_origin: &Option<http::HeaderValue>,
        is_preflight: bool,
        request_headers: Option<http::HeaderValue>,
    ) {
        let allow_credentials = policy
            .and_then(|p| p.allow_credentials)
            .unwrap_or(config.allow_credentials);

        let allow_headers = policy
            .and_then(|p| {
                if p.allow_headers.is_empty() {
                    None
                } else {
                    Some(&p.allow_headers)
                }
            })
            .unwrap_or(&config.allow_headers);

        // Distinguish between None, Some([]), and Some([item, ...]) for expose_headers
        let expose_headers = if let Some(policy) = policy {
            if policy.expose_headers.is_empty() {
                config.expose_headers.as_ref()
            } else {
                Some(&policy.expose_headers)
            }
        } else {
            config.expose_headers.as_ref()
        };

        // Distinguish between None, Some([]), and Some([item, ...]) for methods
        let methods = if let Some(policy) = policy {
            match &policy.methods {
                None => &config.methods,
                Some(methods) => methods,
            }
        } else {
            &config.methods
        };

        let max_age = policy.and_then(|p| p.max_age).or(config.max_age);

        // Set Access-Control-Allow-Origin
        if let Some(origin) = request_origin {
            if config.allow_any_origin {
                response.headers_mut().insert(
                    ACCESS_CONTROL_ALLOW_ORIGIN,
                    http::HeaderValue::from_static("*"),
                );
            } else if policy.is_some() {
                // Only set the header if we found a matching origin configuration
                response
                    .headers_mut()
                    .insert(ACCESS_CONTROL_ALLOW_ORIGIN, origin.clone());
            }
            // If no matching origin config found, don't set the header (origin will be rejected)
        }

        // Set Access-Control-Allow-Credentials
        if allow_credentials {
            response.headers_mut().insert(
                ACCESS_CONTROL_ALLOW_CREDENTIALS,
                http::HeaderValue::from_static("true"),
            );
        }

        // Set Access-Control-Allow-Headers (only for preflight requests)
        if is_preflight {
            if !allow_headers.is_empty() {
                // Join the headers with commas for a single header value
                let header_value = allow_headers.join(", ");
                response.headers_mut().insert(
                    ACCESS_CONTROL_ALLOW_HEADERS,
                    http::HeaderValue::from_str(&header_value)
                        .unwrap_or_else(|_| http::HeaderValue::from_static("")),
                );
            } else {
                // If no headers are configured, mirror the client's Access-Control-Request-Headers
                if let Some(request_headers) = request_headers {
                    if let Ok(headers_str) = request_headers.to_str() {
                        response.headers_mut().insert(
                            ACCESS_CONTROL_ALLOW_HEADERS,
                            http::HeaderValue::from_str(headers_str)
                                .unwrap_or_else(|_| http::HeaderValue::from_static("")),
                        );
                    }
                }
            }
        }

        // Set Access-Control-Expose-Headers (only for non-preflight requests)
        if !is_preflight {
            if let Some(headers) = expose_headers {
                // Join the headers with commas for a single header value
                let header_value = headers.join(", ");
                response.headers_mut().insert(
                    ACCESS_CONTROL_EXPOSE_HEADERS,
                    http::HeaderValue::from_str(&header_value)
                        .unwrap_or_else(|_| http::HeaderValue::from_static("")),
                );
            }
        }

        // Set Access-Control-Allow-Methods (for preflight requests)
        if is_preflight {
            // Join the methods with commas for a single header value
            let method_value = methods.join(", ");
            response.headers_mut().insert(
                ACCESS_CONTROL_ALLOW_METHODS,
                http::HeaderValue::from_str(&method_value)
                    .unwrap_or_else(|_| http::HeaderValue::from_static("")),
            );
        }

        // Set Access-Control-Max-Age (only for preflight requests)
        if is_preflight {
            if let Some(max_age) = max_age {
                let max_age_secs = max_age.as_secs();
                response.headers_mut().insert(
                    ACCESS_CONTROL_MAX_AGE,
                    http::HeaderValue::from_str(&max_age_secs.to_string())
                        .unwrap_or_else(|_| http::HeaderValue::from_static("")),
                );
            }
        }

        // Set Vary header
        response
            .headers_mut()
            .insert(VARY, http::HeaderValue::from_static("Origin"));
    }
}

fn parse_values<T>(values_to_parse: &[String], error_description: &str) -> Result<Vec<T>, String>
where
    T: std::str::FromStr,
    <T as std::str::FromStr>::Err: std::fmt::Display,
{
    let mut errors = Vec::new();
    let mut values = Vec::new();
    for val in values_to_parse {
        match val
            .parse::<T>()
            .map_err(|err| format!("{error_description} '{val}' is not valid: {err}"))
        {
            Ok(val) => values.push(val),
            Err(err) => errors.push(err),
        }
    }

    if errors.is_empty() {
        Ok(values)
    } else {
        Err(errors.join(", "))
    }
}

#[cfg(test)]
mod tests {
    use std::future::Future;
    use std::pin::Pin;
    use std::task::Context;
    use std::task::Poll;

    use http::Request;
    use http::Response;
    use http::StatusCode;
    use http::header::ACCESS_CONTROL_ALLOW_ORIGIN;
    use http::header::ACCESS_CONTROL_EXPOSE_HEADERS;
    use http::header::ORIGIN;
    use tower::Service;

    use super::*;
    use crate::configuration::cors::Cors;
    use crate::configuration::cors::Policy;

    struct DummyService;
    impl Service<Request<()>> for DummyService {
        type Response = Response<&'static str>;
        type Error = ();
        type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

        fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
            Poll::Ready(Ok(()))
        }

        fn call(&mut self, _req: Request<()>) -> Self::Future {
            Box::pin(async {
                Ok(Response::builder()
                    .status(StatusCode::OK)
                    .body("ok")
                    .unwrap())
            })
        }
    }

    #[test]
    fn test_bad_allow_headers_cors_configuration() {
        let cors = Cors::builder()
            .allow_headers(vec![String::from("bad\nname")])
            .build();
        let layer = CorsLayer::new(cors);
        assert!(layer.is_err());

        assert_eq!(
            layer.unwrap_err(),
            String::from("allow header name 'bad\nname' is not valid: invalid HTTP header name")
        );
    }

    #[test]
    fn test_bad_allow_methods_cors_configuration() {
        let cors = Cors::builder()
            .methods(vec![String::from("bad\nmethod")])
            .build();
        let layer = CorsLayer::new(cors);
        assert!(layer.is_err());

        assert_eq!(
            layer.unwrap_err(),
            String::from("method 'bad\nmethod' is not valid: invalid HTTP method")
        );
    }

    #[test]
    fn test_bad_origins_cors_configuration() {
        let cors = Cors::builder()
            .policies(vec![
                Policy::builder()
                    .origins(vec![String::from("bad\norigin")])
                    .build(),
            ])
            .build();
        let layer = CorsLayer::new(cors);
        assert!(layer.is_err());

        assert_eq!(
            layer.unwrap_err(),
            String::from("origin 'bad\norigin' is not valid: failed to parse header value")
        );
    }

    #[test]
    fn test_good_cors_configuration() {
        let cors = Cors::builder()
            .allow_headers(vec![String::from("good-name")])
            .build();
        let layer = CorsLayer::new(cors);
        assert!(layer.is_ok());
    }

    #[test]
    fn test_non_preflight_cors_headers() {
        let cors = Cors::builder()
            .policies(vec![
                Policy::builder()
                    .origins(vec!["https://trusted.com".into()])
                    .expose_headers(vec!["x-custom-header".into()])
                    .build(),
            ])
            .build();
        let layer = CorsLayer::new(cors).unwrap();
        let mut service = layer.layer(DummyService);
        let req = Request::get("/")
            .header(ORIGIN, "https://trusted.com")
            .body(())
            .unwrap();
        let fut = service.call(req);
        let resp = futures::executor::block_on(fut).unwrap();
        let headers = resp.headers();
        assert_eq!(
            headers.get(ACCESS_CONTROL_ALLOW_ORIGIN).unwrap(),
            "https://trusted.com"
        );
        assert_eq!(
            headers.get(ACCESS_CONTROL_EXPOSE_HEADERS).unwrap(),
            "x-custom-header"
        );
    }

    #[test]
    fn test_expose_headers_non_preflight_set() {
        let cors = Cors::builder()
            .expose_headers(vec!["x-foo".into(), "x-bar".into()])
            .build();
        let layer = CorsLayer::new(cors).unwrap();
        let mut service = layer.layer(DummyService);
        let req = Request::get("/")
            .header(ORIGIN, "https://studio.apollographql.com")
            .body(())
            .unwrap();
        let resp = futures::executor::block_on(service.call(req)).unwrap();
        let headers = resp.headers();
        assert_eq!(
            headers.get(ACCESS_CONTROL_EXPOSE_HEADERS).unwrap(),
            "x-foo, x-bar"
        );
    }

    #[test]
    fn test_expose_headers_non_preflight_not_set() {
        let cors = Cors::builder().build();
        let layer = CorsLayer::new(cors).unwrap();
        let mut service = layer.layer(DummyService);
        let req = Request::get("/")
            .header(ORIGIN, "https://studio.apollographql.com")
            .body(())
            .unwrap();
        let resp = futures::executor::block_on(service.call(req)).unwrap();
        let headers = resp.headers();
        assert!(headers.get(ACCESS_CONTROL_EXPOSE_HEADERS).is_none());
    }

    #[test]
    fn test_mirror_request_headers_preflight() {
        let cors = Cors::builder().allow_headers(vec![]).build();
        let layer = CorsLayer::new(cors).unwrap();
        let mut service = layer.layer(DummyService);
        let req = Request::builder()
            .method("OPTIONS")
            .uri("/")
            .header(ORIGIN, "https://studio.apollographql.com")
            .header(ACCESS_CONTROL_REQUEST_HEADERS, "x-foo, x-bar")
            .body(())
            .unwrap();
        let resp = futures::executor::block_on(service.call(req)).unwrap();
        let headers = resp.headers();
        let allow_headers = headers.get(ACCESS_CONTROL_ALLOW_HEADERS).unwrap();
        assert_eq!(allow_headers, "x-foo, x-bar");
    }

    #[test]
    fn test_no_mirror_request_headers_non_preflight() {
        let cors = Cors::builder().allow_headers(vec![]).build();
        let layer = CorsLayer::new(cors).unwrap();
        let mut service = layer.layer(DummyService);
        let req = Request::get("/")
            .header(ORIGIN, "https://studio.apollographql.com")
            .header(ACCESS_CONTROL_REQUEST_HEADERS, "x-foo, x-bar")
            .body(())
            .unwrap();
        let resp = futures::executor::block_on(service.call(req)).unwrap();
        let headers = resp.headers();
        // Should not set ACCESS_CONTROL_ALLOW_HEADERS for non-preflight
        assert!(headers.get(ACCESS_CONTROL_ALLOW_HEADERS).is_none());
    }

    #[test]
    fn test_cors_headers_comma_separated_format() {
        // Test that Access-Control-Allow-Headers uses comma-separated format
        let cors = Cors::builder()
            .allow_headers(vec![
                "content-type".into(),
                "authorization".into(),
                "x-custom".into(),
            ])
            .build();
        let layer = CorsLayer::new(cors).unwrap();
        let mut service = layer.layer(DummyService);
        let req = Request::builder()
            .method("OPTIONS")
            .uri("/")
            .header(ORIGIN, "https://studio.apollographql.com")
            .body(())
            .unwrap();
        let resp = futures::executor::block_on(service.call(req)).unwrap();
        let headers = resp.headers();

        // Should have a single header with comma-separated values
        let allow_headers = headers.get(ACCESS_CONTROL_ALLOW_HEADERS).unwrap();
        assert_eq!(allow_headers, "content-type, authorization, x-custom");

        // Should not have multiple separate headers
        let all_headers = headers.get_all(ACCESS_CONTROL_ALLOW_HEADERS);
        assert_eq!(all_headers.iter().count(), 1);
    }

    #[test]
    fn test_cors_methods_comma_separated_format() {
        // Test that Access-Control-Allow-Methods uses comma-separated format
        let cors = Cors {
            allow_any_origin: false,
            allow_credentials: false,
            allow_headers: vec![],
            expose_headers: None,
            methods: vec!["GET".into(), "POST".into(), "PUT".into()],
            max_age: None,
            policies: None,
        };
        let layer = CorsLayer::new(cors).unwrap();
        let mut service = layer.layer(DummyService);
        let req = Request::builder()
            .method("OPTIONS")
            .uri("/")
            .header(ORIGIN, "https://studio.apollographql.com")
            .body(())
            .unwrap();
        let resp = futures::executor::block_on(service.call(req)).unwrap();
        let headers = resp.headers();

        // Should have a single header with comma-separated values
        let allow_methods = headers.get(ACCESS_CONTROL_ALLOW_METHODS).unwrap();
        assert_eq!(allow_methods, "GET, POST, PUT");

        // Should not have multiple separate headers
        let all_methods = headers.get_all(ACCESS_CONTROL_ALLOW_METHODS);
        assert_eq!(all_methods.iter().count(), 1);
    }

    #[test]
    fn test_policy_methods_fallback_to_global() {
        // Test that when a policy doesn't specify methods, it falls back to global methods
        let cors = Cors::builder()
            .methods(vec!["POST".into()])
            .policies(vec![
                Policy::builder()
                    .origins(vec!["https://example.com".into()])
                    .build(),
            ])
            .build();
        let layer = CorsLayer::new(cors).unwrap();
        let mut service = layer.layer(DummyService);

        // Test preflight request from the policy origin
        let req = Request::builder()
            .method("OPTIONS")
            .uri("/")
            .header(ORIGIN, "https://example.com")
            .body(())
            .unwrap();
        let resp = futures::executor::block_on(service.call(req)).unwrap();
        let headers = resp.headers();

        // Should use the global methods (POST) instead of default methods
        let allow_methods = headers.get(ACCESS_CONTROL_ALLOW_METHODS).unwrap();
        assert_eq!(allow_methods, "POST");
    }

    #[test]
    fn test_policy_empty_methods_runtime() {
        // Test that a policy with empty methods ([]) overrides global methods
        let cors = Cors::builder()
            .methods(vec!["POST".into(), "PUT".into()])
            .policies(vec![
                Policy::builder()
                    .origins(vec!["https://example.com".into()])
                    .methods(vec![])
                    .build(),
            ])
            .build();
        let layer = CorsLayer::new(cors).unwrap();
        let mut service = layer.layer(DummyService);

        // Test preflight request from the policy origin
        let req = Request::builder()
            .method("OPTIONS")
            .uri("/")
            .header(ORIGIN, "https://example.com")
            .body(())
            .unwrap();
        let resp = futures::executor::block_on(service.call(req)).unwrap();
        let headers = resp.headers();

        // Should use empty methods (no methods allowed)
        let allow_methods = headers.get(ACCESS_CONTROL_ALLOW_METHODS).unwrap();
        assert_eq!(allow_methods, "");
    }

    #[test]
    fn test_policy_specific_methods_runtime() {
        // Test that a policy with specific methods uses those methods
        let cors = Cors::builder()
            .methods(vec!["POST".into()])
            .policies(vec![
                Policy::builder()
                    .origins(vec!["https://example.com".into()])
                    .methods(vec!["GET".into(), "DELETE".into()])
                    .build(),
            ])
            .build();
        let layer = CorsLayer::new(cors).unwrap();
        let mut service = layer.layer(DummyService);

        // Test preflight request from the policy origin
        let req = Request::builder()
            .method("OPTIONS")
            .uri("/")
            .header(ORIGIN, "https://example.com")
            .body(())
            .unwrap();
        let resp = futures::executor::block_on(service.call(req)).unwrap();
        let headers = resp.headers();

        // Should use the specific methods (GET, DELETE)
        let allow_methods = headers.get(ACCESS_CONTROL_ALLOW_METHODS).unwrap();
        assert_eq!(allow_methods, "GET, DELETE");
    }
}
