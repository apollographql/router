//! Cross Origin Resource Sharing (CORS configuration)

use std::future::Future;
use std::pin::Pin;
use std::str::FromStr;
use std::task::Context;
use std::task::Poll;
use std::time::Duration;

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
use regex::Regex;
use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;
use tower::Layer;
use tower::Service;

/// Configuration for a specific set of origins
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[serde(default)]
pub(crate) struct Policy {
    /// Set to true to add the `Access-Control-Allow-Credentials` header for these origins
    pub(crate) allow_credentials: Option<bool>,

    /// The headers to allow for these origins
    pub(crate) allow_headers: Vec<String>,

    /// Which response headers should be made available to scripts running in the browser
    pub(crate) expose_headers: Vec<String>,

    /// `Regex`es you want to match the origins against to determine if they're allowed.
    ///
    /// - **Omit this field (do nothing):** Defaults to an empty list (no regex matching).
    /// - `match_origins: []` (empty list): No regex matching.
    ///
    /// # Example
    ///
    /// ```yaml
    /// # No regex matching (default)
    /// policies:
    ///   - origins: [https://myapp.com]
    ///
    /// # With regex matching
    /// policies:
    ///   - match_origins: ["^https://.*\\.example\\.com$"]
    /// ```
    #[serde(with = "serde_regex")]
    #[schemars(with = "Vec<String>")]
    pub(crate) match_origins: Vec<Regex>,

    /// The `Access-Control-Max-Age` header value in time units
    #[serde(deserialize_with = "humantime_serde::deserialize", default)]
    #[schemars(with = "String", default)]
    pub(crate) max_age: Option<Duration>,

    /// Allowed request methods for these origins.
    ///
    /// **Note:** These defaults only apply when the entire `policies` field is omitted from the CORS config.
    /// When specifying individual policies, this field defaults to an empty list unless explicitly set.
    ///
    /// - **When `policies` is omitted:** Defaults to `["GET", "POST", "OPTIONS"]` (uses default Policy).
    /// - **Within a policy:** `methods: []` (empty list) means no methods allowed for this policy.
    /// - **Within a policy:** `methods: [...]` uses the specified methods.
    ///
    /// # Examples
    ///
    /// ```yaml
    /// # Use global default (Apollo Studio + default methods)
    /// cors: {}
    ///
    /// # Within a policy - no methods allowed
    /// cors:
    ///   policies:
    ///     - methods: []
    ///
    /// # Within a policy - custom methods
    /// cors:
    ///   policies:
    ///     - methods: [GET, POST]
    /// ```
    pub(crate) methods: Vec<String>,

    /// The origins to allow requests from.
    ///
    /// **Note:** These defaults only apply when the entire `policies` field is omitted from the CORS config.
    /// When specifying individual policies, this field defaults to an empty list unless explicitly set.
    ///
    /// - **When `policies` is omitted:** Defaults to `["https://studio.apollographql.com"]` (uses default Policy).
    /// - **Within a policy:** `origins: []` (empty list) means no origins allowed for this policy.
    /// - **Within a policy:** `origins: [...]` uses the specified origins.
    ///
    /// # Examples
    ///
    /// ```yaml
    /// # Use global default (Apollo Studio only)
    /// cors: {}
    ///
    /// # Within a policy - no origins allowed
    /// cors:
    ///   policies:
    ///     - origins: []
    ///
    /// # Within a policy - custom origins
    /// cors:
    ///   policies:
    ///     - origins:
    ///         - https://myapp.com
    ///         - https://studio.apollographql.com
    /// ```
    pub(crate) origins: Vec<String>,
}

impl Default for Policy {
    fn default() -> Self {
        Self {
            allow_credentials: None,
            allow_headers: Vec::new(),
            expose_headers: Vec::new(),
            match_origins: Vec::new(),
            max_age: None,
            methods: default_cors_methods(),
            origins: default_origins(),
        }
    }
}

fn default_origins() -> Vec<String> {
    vec!["https://studio.apollographql.com".into()]
}

fn default_cors_methods() -> Vec<String> {
    vec!["GET".into(), "POST".into(), "OPTIONS".into()]
}

// Currently, this is only used for testing.
#[cfg(test)]
#[buildstructor::buildstructor]
impl Policy {
    #[builder]
    pub(crate) fn new(
        allow_credentials: Option<bool>,
        allow_headers: Vec<String>,
        expose_headers: Vec<String>,
        match_origins: Vec<Regex>,
        max_age: Option<Duration>,
        methods: Vec<String>,
        origins: Vec<String>,
    ) -> Self {
        Self {
            allow_credentials,
            allow_headers,
            expose_headers,
            match_origins,
            max_age,
            methods,
            origins,
        }
    }
}

/// Cross origin request configuration.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[serde(default)]
pub(crate) struct Cors {
    /// Set to true to allow any origin.
    ///
    /// Defaults to false
    /// Having this set to true is the only way to allow Origin: null.
    pub(crate) allow_any_origin: bool,

    /// Set to true to add the `Access-Control-Allow-Credentials` header.
    pub(crate) allow_credentials: bool,

    /// The headers to allow.
    ///
    /// If this value is not set, the router will mirror client's `Access-Control-Request-Headers`.
    ///
    /// Note that if you set headers here,
    /// you also want to have a look at your `CSRF` plugins configuration,
    /// and make sure you either:
    /// - accept `x-apollo-operation-name` AND / OR `apollo-require-preflight`
    /// - defined `csrf` required headers in your yml configuration, as shown in the
    ///   `examples/cors-and-csrf/custom-headers.router.yaml` files.
    pub(crate) allow_headers: Vec<String>,

    /// Which response headers should be made available to scripts running in the browser,
    /// in response to a cross-origin request.
    pub(crate) expose_headers: Option<Vec<String>>,

    /// Allowed request methods. Defaults to GET, POST, OPTIONS.
    ///
    /// - **Omit this field (do nothing):** Defaults to `["GET", "POST", "OPTIONS"]`.
    /// - `methods: []` (empty list): No methods are allowed.
    ///
    /// This field uses [`#[serde(default = "default_cors_methods")]`](https://serde.rs/attributes.html#default).
    ///
    /// # Example
    ///
    /// ```yaml
    /// # Use default methods
    /// cors: {}
    ///
    /// # Custom methods
    /// cors:
    ///   methods: [GET, POST]
    /// ```
    pub(crate) methods: Vec<String>,

    /// The `Access-Control-Max-Age` header value in time units
    #[serde(deserialize_with = "humantime_serde::deserialize", default)]
    #[schemars(with = "String", default)]
    pub(crate) max_age: Option<Duration>,

    /// The origin(s) to allow requests from.
    ///
    /// When a request is received, the router will match the request origin against the policies
    /// in order, first by exact match, then by regex. The first policy that matches the request
    /// origin will be used to determine the CORS headers to return.
    ///
    /// - **Omit this field (do nothing):** Use the default Apollo Studio policy (`https://studio.apollographql.com`)
    /// - `policies: []` (empty list): Disable all origins (no CORS headers will be set)
    /// - `policies: [...]`: Use custom policies
    ///
    /// This field uses [`#[serde(default)]`](https://serde.rs/attributes.html#default) and
    /// [`Option<T>` handling](https://serde.rs/attr-default.html), so omitting the field in YAML
    /// will use the default, while specifying an empty list disables CORS.
    ///
    /// # Examples
    ///
    /// ```yaml
    /// # Use default (Apollo Studio only)
    /// cors: {}
    ///
    /// # Disable all CORS
    /// cors:
    ///   policies: []
    ///
    /// # Custom policies
    /// cors:
    ///   policies:
    ///     - origins: [https://myapp.com]
    /// ```
    ///
    /// See also: [Serde attributes documentation](https://serde.rs/attributes.html#default)
    pub(crate) policies: Option<Vec<Policy>>,
}

impl Default for Cors {
    fn default() -> Self {
        Self::builder().build()
    }
}

#[buildstructor::buildstructor]
impl Cors {
    #[builder]
    pub(crate) fn new(
        allow_any_origin: Option<bool>,
        allow_credentials: Option<bool>,
        allow_headers: Option<Vec<String>>,
        expose_headers: Option<Vec<String>>,
        max_age: Option<Duration>,
        methods: Option<Vec<String>>,
        policies: Option<Vec<Policy>>,
    ) -> Self {
        Self {
            allow_any_origin: allow_any_origin.unwrap_or_default(),
            allow_credentials: allow_credentials.unwrap_or_default(),
            allow_headers: allow_headers.unwrap_or_default(),
            expose_headers,
            max_age,
            methods: methods.unwrap_or_else(default_cors_methods),
            policies: policies.or_else(|| Some(vec![Policy::default()])),
        }
    }
}

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
                if !policy.methods.is_empty() {
                    parse_values::<http::Method>(&policy.methods, "method")?;
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

        // Check for exact origin matches first
        if let Some(policies) = &config.policies {
            for policy in policies.iter() {
                for url in &policy.origins {
                    if url == origin_str {
                        return Some(policy);
                    }
                }

                // Check regex matches
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

        let methods = policy
            .and_then(|p| {
                if p.methods.is_empty() {
                    None
                } else {
                    Some(&p.methods)
                }
            })
            .unwrap_or(&config.methods);

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
                // Precompute the header value to avoid multiple allocations and lookups
                let header_values: Vec<http::HeaderValue> = allow_headers
                    .iter()
                    .map(|header| {
                        http::HeaderValue::from_str(header)
                            .unwrap_or_else(|_| http::HeaderValue::from_static(""))
                    })
                    .collect();

                for header_value in header_values {
                    response
                        .headers_mut()
                        .append(ACCESS_CONTROL_ALLOW_HEADERS, header_value);
                }
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
                // Precompute the header values to avoid multiple allocations and lookups
                let header_values: Vec<http::HeaderValue> = headers
                    .iter()
                    .map(|header| {
                        http::HeaderValue::from_str(header)
                            .unwrap_or_else(|_| http::HeaderValue::from_static(""))
                    })
                    .collect();

                for header_value in header_values {
                    response
                        .headers_mut()
                        .append(ACCESS_CONTROL_EXPOSE_HEADERS, header_value);
                }
            }
        }

        // Set Access-Control-Allow-Methods (for preflight requests)
        if is_preflight {
            // Precompute the method values to avoid multiple allocations and lookups
            let method_values: Vec<http::HeaderValue> = methods
                .iter()
                .map(|method| {
                    http::HeaderValue::from_str(method)
                        .unwrap_or_else(|_| http::HeaderValue::from_static(""))
                })
                .collect();

            for method_value in method_values {
                response
                    .headers_mut()
                    .append(ACCESS_CONTROL_ALLOW_METHODS, method_value);
            }
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

impl Cors {
    pub(crate) fn into_layer(self) -> Result<CorsLayer, String> {
        CorsLayer::new(self)
    }

    // This is cribbed from the similarly named function in tower-http. The version there
    // asserts that CORS rules are useable, which results in a panic if they aren't. We
    // don't want the router to panic in such cases, so this function returns an error
    // with a message describing what the problem is.
    fn ensure_usable_cors_rules(&self) -> Result<(), &'static str> {
        // Check for wildcard origins in any Policy
        if let Some(policies) = &self.policies {
            for policy in policies {
                if policy.origins.iter().any(|x| x == "*") {
                    return Err(
                        "Invalid CORS configuration: use `allow_any_origin: true` to set `Access-Control-Allow-Origin: *`",
                    );
                }
            }
        }

        if self.allow_credentials {
            // Check global fields for wildcards
            if self.allow_headers.iter().any(|x| x == "*") {
                return Err(
                    "Invalid CORS configuration: Cannot combine `Access-Control-Allow-Credentials: true` \
                        with `Access-Control-Allow-Headers: *`",
                );
            }

            if self.methods.iter().any(|x| x == "*") {
                return Err(
                    "Invalid CORS configuration: Cannot combine `Access-Control-Allow-Credentials: true` \
                    with `Access-Control-Allow-Methods: *`",
                );
            }

            if self.allow_any_origin {
                return Err(
                    "Invalid CORS configuration: Cannot combine `Access-Control-Allow-Credentials: true` \
                    with `allow_any_origin: true`",
                );
            }

            if let Some(headers) = &self.expose_headers {
                if headers.iter().any(|x| x == "*") {
                    return Err(
                        "Invalid CORS configuration: Cannot combine `Access-Control-Allow-Credentials: true` \
                        with `Access-Control-Expose-Headers: *`",
                    );
                }
            }

            // Check per-policy fields for wildcards when credentials are enabled
            if let Some(policies) = &self.policies {
                for policy in policies {
                    if policy.allow_headers.iter().any(|x| x == "*") {
                        return Err(
                            "Invalid CORS configuration: Cannot combine `Access-Control-Allow-Credentials: true` \
                            with `Access-Control-Allow-Headers: *` in policy",
                        );
                    }

                    if policy.methods.iter().any(|x| x == "*") {
                        return Err(
                            "Invalid CORS configuration: Cannot combine `Access-Control-Allow-Credentials: true` \
                            with `Access-Control-Allow-Methods: *` in policy",
                        );
                    }

                    if policy.expose_headers.iter().any(|x| x == "*") {
                        return Err(
                            "Invalid CORS configuration: Cannot combine `Access-Control-Allow-Credentials: true` \
                            with `Access-Control-Expose-Headers: *` in policy",
                        );
                    }
                }
            }
        }
        Ok(())
    }
}

fn parse_values<T>(values_to_parse: &[String], error_description: &str) -> Result<Vec<T>, String>
where
    T: FromStr,
    <T as FromStr>::Err: std::fmt::Display,
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
        let layer = cors.into_layer();
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
        let layer = cors.into_layer();
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
        let layer = cors.into_layer();
        assert!(layer.is_err());

        assert_eq!(
            layer.unwrap_err(),
            String::from("origin 'bad\norigin' is not valid: failed to parse header value")
        );
    }

    #[test]
    fn test_bad_match_origins_cors_configuration() {
        let yaml = r#"
allow_any_origin: false
allow_credentials: false
allow_headers: []
expose_headers: []
methods: ["GET", "POST", "OPTIONS"]
policies:
  - origins: ["https://studio.apollographql.com"]
    allow_credentials: false
    allow_headers: []
    expose_headers: []
    match_origins: ["["]
    methods: ["GET", "POST", "OPTIONS"]
"#;
        let cors: Result<Cors, _> = serde_yaml::from_str(yaml);
        assert!(cors.is_err());
        let err = format!("{}", cors.unwrap_err());
        assert!(err.contains("regex parse error"));
        assert!(err.contains("unclosed character class"));
    }

    #[test]
    fn test_good_cors_configuration() {
        let cors = Cors::builder()
            .allow_headers(vec![String::from("good-name")])
            .build();
        let layer = cors.into_layer();
        assert!(layer.is_ok());
    }

    // Test that multiple Policy entries have correct precedence (exact match > regex)
    // This ensures the matching logic is deterministic and follows the documented behavior
    #[test]
    fn test_multiple_origin_config_precedence() {
        let cors = Cors::builder()
            .policies(vec![
                // This should match by regex but be lower priority
                Policy::builder()
                    .origins(vec![])
                    .match_origins(vec![
                        regex::Regex::new(r"https://.*\.example\.com").unwrap(),
                    ])
                    .allow_headers(vec!["regex-header".into()])
                    .build(),
                // This should match by exact match and be higher priority
                Policy::builder()
                    .origins(vec!["https://api.example.com".into()])
                    .allow_headers(vec!["exact-header".into()])
                    .build(),
            ])
            .build();
        let layer = cors.into_layer();
        assert!(layer.is_ok());
    }

    // Test regex matching edge cases to ensure regexes are not too permissive or restrictive
    // This prevents security issues where unintended origins might be allowed
    #[test]
    fn test_regex_matching_edge_cases() {
        let cors = Cors::builder()
            .policies(vec![
                Policy::builder()
                    .origins(vec![])
                    .match_origins(vec![
                        regex::Regex::new(r"https://[a-z]+\.example\.com").unwrap(),
                    ])
                    .build(),
            ])
            .build();
        let layer = cors.into_layer();
        assert!(layer.is_ok());
    }

    // Test that wildcard origins in Policy are rejected
    // This ensures users must use allow_any_origin: true for wildcard behavior
    #[test]
    fn test_wildcard_origin_in_origin_config_rejected() {
        let cors = Cors::builder()
            .policies(vec![Policy::builder().origins(vec!["*".into()]).build()])
            .build();
        let layer = cors.into_layer();
        assert!(layer.is_err());
        assert!(layer.unwrap_err().contains("use `allow_any_origin: true`"));
    }

    // Test that allow_any_origin with credentials is rejected
    // This is forbidden by the CORS spec and prevents security issues
    #[test]
    fn test_allow_any_origin_with_credentials_rejected() {
        let cors = Cors::builder()
            .allow_any_origin(true)
            .allow_credentials(true)
            .build();
        let layer = cors.into_layer();
        assert!(layer.is_err());
        assert!(
            layer
                .unwrap_err()
                .contains("Cannot combine `Access-Control-Allow-Credentials: true`")
        );
    }

    // Test that wildcard headers with credentials are rejected
    // This prevents security issues where credentials could be sent with any header
    #[test]
    fn test_wildcard_headers_with_credentials_rejected() {
        let cors = Cors::builder()
            .allow_credentials(true)
            .allow_headers(vec!["*".into()])
            .build();
        let layer = cors.into_layer();
        assert!(layer.is_err());
        assert!(
            layer
                .unwrap_err()
                .contains("Cannot combine `Access-Control-Allow-Credentials: true`")
        );
    }

    // Test that wildcard methods with credentials are rejected
    // This prevents security issues where credentials could be sent with any method
    #[test]
    fn test_wildcard_methods_with_credentials_rejected() {
        let cors = Cors::builder()
            .allow_credentials(true)
            .methods(vec!["*".into()])
            .build();
        let layer = cors.into_layer();
        assert!(layer.is_err());
        assert!(
            layer
                .unwrap_err()
                .contains("Cannot combine `Access-Control-Allow-Credentials: true`")
        );
    }

    // Test that wildcard expose headers with credentials are rejected
    // This prevents security issues where any header could be exposed with credentials
    #[test]
    fn test_wildcard_expose_headers_with_credentials_rejected() {
        let cors = Cors::builder()
            .allow_credentials(true)
            .expose_headers(vec!["*".into()])
            .build();
        let layer = cors.into_layer();
        assert!(layer.is_err());
        assert!(
            layer
                .unwrap_err()
                .contains("Cannot combine `Access-Control-Allow-Credentials: true`")
        );
    }

    // Test that per-policy wildcard headers with credentials are rejected
    // This prevents security issues where credentials could be sent with any header in a policy
    #[test]
    fn test_per_policy_wildcard_headers_with_credentials_rejected() {
        let cors = Cors::builder()
            .allow_credentials(true)
            .policies(vec![
                Policy::builder()
                    .origins(vec!["https://example.com".into()])
                    .allow_headers(vec!["*".into()])
                    .build(),
            ])
            .build();
        let layer = cors.into_layer();
        assert!(layer.is_err());
        let error_msg = layer.unwrap_err();
        assert!(error_msg.contains("Cannot combine `Access-Control-Allow-Credentials: true`"));
        assert!(error_msg.contains("in policy"));
    }

    // Test that per-policy wildcard methods with credentials are rejected
    // This prevents security issues where credentials could be sent with any method in a policy
    #[test]
    fn test_per_policy_wildcard_methods_with_credentials_rejected() {
        let cors = Cors::builder()
            .allow_credentials(true)
            .policies(vec![
                Policy::builder()
                    .origins(vec!["https://example.com".into()])
                    .methods(vec!["*".into()])
                    .build(),
            ])
            .build();
        let layer = cors.into_layer();
        assert!(layer.is_err());
        let error_msg = layer.unwrap_err();
        assert!(error_msg.contains("Cannot combine `Access-Control-Allow-Credentials: true`"));
        assert!(error_msg.contains("in policy"));
    }

    // Test that per-policy wildcard expose headers with credentials are rejected
    // This prevents security issues where any header could be exposed with credentials in a policy
    #[test]
    fn test_per_policy_wildcard_expose_headers_with_credentials_rejected() {
        let cors = Cors::builder()
            .allow_credentials(true)
            .policies(vec![
                Policy::builder()
                    .origins(vec!["https://example.com".into()])
                    .expose_headers(vec!["*".into()])
                    .build(),
            ])
            .build();
        let layer = cors.into_layer();
        assert!(layer.is_err());
        let error_msg = layer.unwrap_err();
        assert!(error_msg.contains("Cannot combine `Access-Control-Allow-Credentials: true`"));
        assert!(error_msg.contains("in policy"));
    }

    // Test that per-policy wildcard validation works with multiple policies
    // This ensures that validation checks all policies, not just the first one
    #[test]
    fn test_per_policy_wildcard_validation_with_multiple_policies() {
        let cors = Cors::builder()
            .allow_credentials(true)
            .policies(vec![
                Policy::builder()
                    .origins(vec!["https://example.com".into()])
                    .allow_headers(vec!["content-type".into()])
                    .build(),
                Policy::builder()
                    .origins(vec!["https://another.com".into()])
                    .allow_headers(vec!["*".into()])
                    .build(),
            ])
            .build();
        let layer = cors.into_layer();
        assert!(layer.is_err());
        let error_msg = layer.unwrap_err();
        assert!(error_msg.contains("Cannot combine `Access-Control-Allow-Credentials: true`"));
        assert!(error_msg.contains("in policy"));
    }

    // Test that per-policy wildcard validation is skipped when credentials are disabled
    // This ensures that wildcards are allowed when credentials are not enabled
    #[test]
    fn test_per_policy_wildcard_allowed_when_credentials_disabled() {
        let cors = Cors::builder()
            .allow_credentials(false)
            .policies(vec![
                Policy::builder()
                    .origins(vec!["https://example.com".into()])
                    .allow_headers(vec!["*".into()])
                    .methods(vec!["*".into()])
                    .expose_headers(vec!["*".into()])
                    .build(),
            ])
            .build();
        let layer = cors.into_layer();
        assert!(layer.is_ok());
    }

    // Test that Origin: null is only allowed with allow_any_origin: true
    // This ensures compliance with the CORS spec which only allows null origin in this case
    #[test]
    fn test_origin_null_only_allowed_with_allow_any_origin() {
        let cors = Cors::builder().allow_any_origin(true).build();
        let layer = cors.into_layer();
        assert!(layer.is_ok());

        let cors_without_allow_any = Cors::builder().allow_any_origin(false).build();
        let layer = cors_without_allow_any.into_layer();
        assert!(layer.is_ok()); // This should be valid config, but null origin requests should be rejected
    }

    // Test that max_age is properly validated and handled
    // This ensures preflight caching works correctly and prevents invalid configurations
    #[test]
    fn test_max_age_validation() {
        // Valid max_age
        let cors = Cors::builder().max_age(Duration::from_secs(3600)).build();
        let layer = cors.into_layer();
        assert!(layer.is_ok());

        // Zero max_age should be valid
        let cors_zero = Cors::builder().max_age(Duration::from_secs(0)).build();
        let layer_zero = cors_zero.into_layer();
        assert!(layer_zero.is_ok());
    }

    // Test that expose_headers are properly validated
    // This ensures that only valid header names can be exposed to the browser
    #[test]
    fn test_expose_headers_validation() {
        // Valid expose headers
        let cors = Cors::builder()
            .expose_headers(vec!["content-type".into(), "x-custom-header".into()])
            .build();
        let layer = cors.into_layer();
        assert!(layer.is_ok());

        // Invalid expose header
        let cors_invalid = Cors::builder()
            .expose_headers(vec!["invalid\nheader".into()])
            .build();
        let layer_invalid = cors_invalid.into_layer();
        assert!(layer_invalid.is_err());
        assert!(layer_invalid.unwrap_err().contains("expose header name"));
    }

    // Test that origin-specific expose_headers are properly validated
    // This ensures per-origin configurations are validated correctly
    #[test]
    fn test_origin_specific_expose_headers_validation() {
        let cors = Cors::builder()
            .policies(vec![
                Policy::builder()
                    .origins(vec!["https://example.com".into()])
                    .expose_headers(vec!["invalid\nheader".into()])
                    .build(),
            ])
            .build();
        let layer = cors.into_layer();
        assert!(layer.is_err());
        assert!(layer.unwrap_err().contains("expose header name"));
    }

    // Test that origin-specific methods are properly validated
    // This ensures per-origin method configurations are validated correctly
    #[test]
    fn test_origin_specific_methods_validation() {
        let cors = Cors::builder()
            .policies(vec![
                Policy::builder()
                    .origins(vec!["https://example.com".into()])
                    .methods(vec!["INVALID\nMETHOD".into()])
                    .build(),
            ])
            .build();
        let layer = cors.into_layer();
        assert!(layer.is_err());
        assert!(layer.unwrap_err().contains("method"));
    }

    // Test that origin-specific allow_headers are properly validated
    // This ensures per-origin header configurations are validated correctly
    #[test]
    fn test_origin_specific_allow_headers_validation() {
        let cors = Cors::builder()
            .policies(vec![
                Policy::builder()
                    .origins(vec!["https://example.com".into()])
                    .allow_headers(vec!["invalid\nheader".into()])
                    .build(),
            ])
            .build();
        let layer = cors.into_layer();
        assert!(layer.is_err());
        assert!(layer.unwrap_err().contains("allow header name"));
    }

    // Test that empty origins list is valid
    // This ensures the configuration can be used for deny-all scenarios
    #[test]
    fn test_empty_origins_list_valid() {
        let cors = Cors::builder().policies(vec![]).build();
        let layer = cors.into_layer();
        assert!(layer.is_ok());
    }

    // Test that empty methods list falls back to defaults
    // This ensures backward compatibility when methods are not specified
    #[test]
    fn test_empty_methods_falls_back_to_defaults() {
        let cors = Cors::builder().methods(vec![]).build();
        let layer = cors.into_layer();
        assert!(layer.is_ok());
    }

    // Test that empty allow_headers list is valid
    // This ensures the mirroring behavior works when no headers are configured
    #[test]
    fn test_empty_allow_headers_valid() {
        let cors = Cors::builder().allow_headers(vec![]).build();
        let layer = cors.into_layer();
        assert!(layer.is_ok());
    }

    // Test that complex regex patterns are handled correctly
    // This ensures advanced regex matching works for complex origin patterns
    #[test]
    fn test_complex_regex_patterns() {
        let cors = Cors::builder()
            .policies(vec![
                Policy::builder()
                    .origins(vec![])
                    .match_origins(vec![
                        regex::Regex::new(r"https://(?:www\.)?example\.com").unwrap(),
                        regex::Regex::new(r"https://api-[0-9]+\.example\.com").unwrap(),
                    ])
                    .build(),
            ])
            .build();
        let layer = cors.into_layer();
        assert!(layer.is_ok());
    }

    // Test that multiple regex patterns in a single Policy work
    // This ensures that multiple regex patterns can be used for the same origin configuration
    #[test]
    fn test_multiple_regex_patterns_in_single_origin_config() {
        let cors = Cors::builder()
            .policies(vec![
                Policy::builder()
                    .origins(vec![])
                    .match_origins(vec![
                        regex::Regex::new(r"https://api\.example\.com").unwrap(),
                        regex::Regex::new(r"https://staging\.example\.com").unwrap(),
                    ])
                    .build(),
            ])
            .build();
        let layer = cors.into_layer();
        assert!(layer.is_ok());
    }

    // Test that case-sensitive origin matching works correctly
    // This ensures that origin matching follows the CORS spec which requires case-sensitive matching
    #[test]
    fn test_case_sensitive_origin_matching() {
        let cors = Cors::builder()
            .policies(vec![
                Policy::builder()
                    .origins(vec!["https://Example.com".into()])
                    .build(),
            ])
            .build();
        let layer = cors.into_layer();
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
        let layer = cors.into_layer().unwrap();
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
        let layer = cors.into_layer().unwrap();
        let mut service = layer.layer(DummyService);
        let req = Request::get("/")
            .header(ORIGIN, "https://studio.apollographql.com")
            .body(())
            .unwrap();
        let resp = futures::executor::block_on(service.call(req)).unwrap();
        let headers = resp.headers();
        let expose = headers
            .get_all(ACCESS_CONTROL_EXPOSE_HEADERS)
            .iter()
            .collect::<Vec<_>>();
        assert!(
            expose
                .iter()
                .any(|h| *h == http::HeaderValue::from_static("x-foo"))
        );
        assert!(
            expose
                .iter()
                .any(|h| *h == http::HeaderValue::from_static("x-bar"))
        );
    }

    #[test]
    fn test_expose_headers_non_preflight_not_set() {
        let cors = Cors::builder().build();
        let layer = cors.into_layer().unwrap();
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
        let layer = cors.into_layer().unwrap();
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
        let layer = cors.into_layer().unwrap();
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
}
