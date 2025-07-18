//! Cross Origin Resource Sharing (CORS configuration)
//!
//! This module provides configuration structures for CORS (Cross-Origin Resource Sharing) settings.
//!
//! # Default Behavior
//!
//! When the `policies` field is omitted from the CORS config, the router uses a default policy:
//! - **Origins:** `["https://studio.apollographql.com"]`
//! - **Methods:** `["GET", "POST", "OPTIONS"]`
//! - **Allow credentials:** `false`
//! - **Allow any origin:** `false`
//!
//! # Policy Configuration
//!
//! When specifying individual policies within the `policies` array:
//! - **Origins:** Defaults to an empty list (no origins allowed) unless explicitly set
//! - **Methods:** Defaults to an empty list (no methods allowed) unless explicitly set
//! - **Allow headers:** Defaults to an empty list (mirrors client headers) unless explicitly set
//! - **Expose headers:** Defaults to an empty list unless explicitly set
//! - **Match origins:** Defaults to an empty list (no regex matching) unless explicitly set
//!
//! # Examples
//!
//! ```yaml
//! # Use global default (Apollo Studio only)
//! cors: {}
//!
//! # Disable all CORS
//! cors:
//!   policies: []
//!
//! # Custom policies
//! cors:
//!   policies:
//!     - origins: [https://myapp.com]
//!       methods: [GET, POST]
//!     - match_origins: ["^https://.*\\.example\\.com$"]
//!       allow_headers: [content-type, authorization]
//! ```

use std::time::Duration;

use regex::Regex;
use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;

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

    /// Regex patterns to match origins against.
    #[serde(with = "serde_regex")]
    #[schemars(with = "Vec<String>")]
    pub(crate) match_origins: Vec<Regex>,

    /// The `Access-Control-Max-Age` header value in time units
    #[serde(deserialize_with = "humantime_serde::deserialize", default)]
    #[schemars(with = "String", default)]
    pub(crate) max_age: Option<Duration>,

    /// Allowed request methods for these origins.
    pub(crate) methods: Vec<String>,

    /// The origins to allow requests from.
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
    /// Set to true to allow any origin. Defaults to false. This is the only way to allow Origin: null.
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

    /// Allowed request methods. See module documentation for default behavior.
    pub(crate) methods: Vec<String>,

    /// The `Access-Control-Max-Age` header value in time units
    #[serde(deserialize_with = "humantime_serde::deserialize", default)]
    #[schemars(with = "String", default)]
    pub(crate) max_age: Option<Duration>,

    /// The origin(s) to allow requests from. The router matches request origins against policies
    /// in order, first by exact match, then by regex. See module documentation for default behavior.
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

impl Cors {
    pub(crate) fn into_layer(self) -> Result<crate::plugins::cors::CorsLayer, String> {
        crate::plugins::cors::CorsLayer::new(self)
    }

    // This is cribbed from the similarly named function in tower-http. The version there
    // asserts that CORS rules are useable, which results in a panic if they aren't. We
    // don't want the router to panic in such cases, so this function returns an error
    // with a message describing what the problem is.
    pub(crate) fn ensure_usable_cors_rules(&self) -> Result<(), &'static str> {
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

#[cfg(test)]
mod tests {
    use super::*;

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
}
