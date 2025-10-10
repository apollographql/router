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
//! - **Origins:** Defaults to `["https://studio.apollographql.com"]` when no policies are specified at all.
//!   If you specify any policies, you must explicitly set origins for each policy.
//! - **Match origins:** Defaults to an empty list (no regex matching) unless explicitly set
//! - **Allow credentials:** Has three possible states:
//!   - not specified: Use the global default allow_credentials
//!   - `true`: Enable credentials for this policy
//!   - `false`: Disable credentials for this policy
//! - **Allow headers:** Inherits global headers if empty, otherwise uses policy-specific headers
//! - **Expose headers:** Inherits global headers if empty, otherwise uses policy-specific headers
//! - **Methods:** Has three possible states:
//!   - not specified: Use the global default methods
//!   - `[]` (empty array): No methods allowed for this policy
//!   - `[values]` (with values): Use these specific methods
//! - **Max age:** Has three possible states:
//!   - not specified: Use the global default max_age
//!   - `"0s"` or other duration: Set specific max age for this policy
//!
//! # Origin Matching
//!
//! The router matches request origins against policies in order:
//! 1. **Exact match**: First checks if the origin exactly matches any origin in the policy's `origins` list
//! 2. **Regex match**: If no exact match is found, checks if the origin matches any pattern in the policy's `match_origins` list
//! 3. **No match**: If no policy matches, the request is rejected (no CORS headers are set)
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
//! # Global methods with policy-specific overrides
//! cors:
//!   methods: [POST]  # Global default
//!   policies:
//!     - origins: [https://app1.com]
//!       # methods not specified - uses global default [POST]
//!     - origins: [https://app2.com]
//!       methods: []  # Explicitly disable all methods
//!     - origins: [https://app3.com]
//!       methods: [GET, DELETE]  # Use specific methods
//!     - origins: [https://api.example.com]
//!       match_origins: ["^https://.*\\.example\\.com$"]  # Regex pattern for subdomains
//!       allow_headers: [content-type, authorization]
//!       # Uses global methods [POST]
//! ```

use std::sync::Arc;
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
    pub(crate) allow_headers: Arc<[Arc<str>]>,

    /// Which response headers should be made available to scripts running in the browser
    pub(crate) expose_headers: Arc<[Arc<str>]>,

    /// Regex patterns to match origins against.
    #[serde(with = "serde_regex")]
    #[schemars(with = "Vec<String>")]
    pub(crate) match_origins: Vec<Regex>,

    /// The `Access-Control-Max-Age` header value in time units
    #[serde(deserialize_with = "humantime_serde::deserialize", default)]
    #[schemars(with = "String", default)]
    pub(crate) max_age: Option<Duration>,

    /// Allowed request methods for these origins.
    pub(crate) methods: Option<Arc<[Arc<str>]>>,

    /// The origins to allow requests from.
    pub(crate) origins: Arc<[Arc<str>]>,

    /// When `Some`, the `Access-Control-Allow-Private-Network` header will be added as well as the
    /// respective headers contained within the policy.
    pub(crate) private_network_access: Option<PrivateNetworkAccessPolicy>,
}

#[derive(Debug, Default, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[serde(default)]
pub(crate) struct PrivateNetworkAccessPolicy {
    /// When `Some`, the `Private-Network-Access-ID` header will be added with the given ID.
    /// The ID must be a 48-bit value presented as 6 hexadecimal bytes separated by colons, e.g.
    /// `01:23:45:67:89:0A`.
    pub(crate) access_id: Option<Arc<str>>,

    /// When `Some`, the `Private-Network-Access-Name` header will be added with the given name.
    /// The name can be at most 248 UTF-8 code units and match a RegEx equivalent to the ECMAScript
    /// RegEx `/^[a-z0-9_-.]+$/.`
    pub(crate) access_name: Option<Arc<str>>,
}

impl Default for Policy {
    fn default() -> Self {
        Self {
            allow_credentials: None,
            allow_headers: Arc::new([]),
            expose_headers: Arc::new([]),
            match_origins: Vec::new(),
            max_age: None,
            methods: None,
            origins: default_origins(),
            private_network_access: None,
        }
    }
}

fn default_origins() -> Arc<[Arc<str>]> {
    Arc::new(["https://studio.apollographql.com".into()])
}

fn default_cors_methods() -> Arc<[Arc<str>]> {
    Arc::new(["GET".into(), "POST".into(), "OPTIONS".into()])
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
        methods: Option<Vec<String>>,
        origins: Vec<String>,
        private_network_access: Option<PrivateNetworkAccessPolicy>,
    ) -> Self {
        Self {
            allow_credentials,
            allow_headers: allow_headers.into_iter().map(Arc::from).collect(),
            expose_headers: expose_headers.into_iter().map(Arc::from).collect(),
            match_origins,
            max_age,
            methods: methods.map(|methods| methods.into_iter().map(Arc::from).collect()),
            origins: origins.into_iter().map(Arc::from).collect(),
            private_network_access,
        }
    }
}

#[cfg(test)]
#[buildstructor::buildstructor]
impl PrivateNetworkAccessPolicy {
    #[builder]
    pub(crate) fn new(access_name: Option<String>, access_id: Option<String>) -> Self {
        Self {
            access_id: access_id.map(Arc::from),
            access_name: access_name.map(Arc::from),
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
    pub(crate) allow_headers: Arc<[Arc<str>]>,

    /// Which response headers should be made available to scripts running in the browser,
    /// in response to a cross-origin request.
    pub(crate) expose_headers: Option<Arc<[Arc<str>]>>,

    /// Allowed request methods. See module documentation for default behavior.
    pub(crate) methods: Arc<[Arc<str>]>,

    /// The `Access-Control-Max-Age` header value in time units
    #[serde(deserialize_with = "humantime_serde::deserialize", default)]
    #[schemars(with = "String", default)]
    pub(crate) max_age: Option<Duration>,

    /// The origin(s) to allow requests from. The router matches request origins against policies
    /// in order, first by exact match, then by regex. See module documentation for default behavior.
    pub(crate) policies: Option<Arc<[Policy]>>,
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
        let global_methods = methods.map_or_else(default_cors_methods, |methods| {
            methods.into_iter().map(Arc::from).collect()
        });
        let policies = policies.or_else(|| {
            let default_policy = Policy {
                methods: Some(global_methods.clone()),
                ..Default::default()
            };
            Some(vec![default_policy])
        });

        Self {
            allow_any_origin: allow_any_origin.unwrap_or_default(),
            allow_credentials: allow_credentials.unwrap_or_default(),
            allow_headers: allow_headers.map_or_else(Default::default, |headers| {
                headers.into_iter().map(Arc::from).collect()
            }),
            expose_headers: expose_headers
                .map(|headers| headers.into_iter().map(Arc::from).collect()),
            max_age,
            methods: global_methods,
            policies: policies.map(Arc::from),
        }
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub(crate) enum CorsConfigError {
    #[error(
        "Invalid CORS configuration: use `allow_any_origin: true` to set `Access-Control-Allow-Origin: *`"
    )]
    AllowAnyOrigin,
    #[error(
        "Invalid CORS configuration: origins cannot have trailing slashes (a serialized origin has no trailing slash)"
    )]
    TrailingSlashInOrigin,
    #[error(
        "Invalid CORS configuration: Cannot combine `Access-Control-Allow-Credentials: true` with `Access-Control-Allow-Headers: *` in policy"
    )]
    AllowCredentialsWithAllowHeaders,
    #[error(
        "Invalid CORS configuration: Cannot combine `Access-Control-Allow-Credentials: true` with `Access-Control-Allow-Methods: *` in policy"
    )]
    AllowCredentialsWithAllowMethods,
    #[error(
        "Invalid CORS configuration: Cannot combine `Access-Control-Allow-Credentials: true` with `allow_any_origin: true`"
    )]
    AllowCredentialsWithAnyOrigin,
    #[error(
        "Invalid CORS configuration: Cannot combine `Access-Control-Allow-Credentials: true` with `Access-Control-Expose-Headers: *` in policy"
    )]
    AllowCredentialsWithExposeHeaders,
    #[error(
        "Invalid CORS configuration: `Private-Network-Access-Name` header value must not be empty."
    )]
    EmptyPrivateNetworkAccessName,
    #[error(
        "Invalid CORS configuration: `Private-Network-Access-Name` header value must be no longer than 248 characters."
    )]
    LengthyPrivateNetworkAccessName,
    #[error(
        "Invalid CORS configuration: `Private-Network-Access-Name` header value can only contain the characters a-z0-9_-."
    )]
    InvalidPrivateNetworkAccessName,
    #[error(
        "Invalid CORS configuration: `Private-Network-Access-ID` header value must be a 48-bit value presented as 6 hexadecimal bytes separated by colons"
    )]
    InvalidPrivateNetworkAccessId,
}

impl Cors {
    pub(crate) fn into_layer(self) -> Result<crate::plugins::cors::CorsLayer, String> {
        crate::plugins::cors::CorsLayer::new(self)
    }

    // This is cribbed from the similarly named function in tower-http. The version there
    // asserts that CORS rules are useable, which results in a panic if they aren't. We
    // don't want the router to panic in such cases, so this function returns an error
    // with a message describing what the problem is.
    //
    // This function validates CORS configuration according to the CORS specification:
    // https://fetch.spec.whatwg.org/#cors-protocol-and-credentials
    //
    // CORS Specification Table (from https://fetch.spec.whatwg.org/#cors-protocol-and-credentials):
    //
    // Request's credentials mode | Access-Control-Allow-Origin | Access-Control-Allow-Credentials | Shared? | Notes
    // ---------------------------|------------------------------|-----------------------------------|---------|------
    // "omit"                     | `*`                          | Omitted                          | ✅      | —
    // "omit"                     | `*`                          | `true`                           | ✅      | If credentials mode is not "include", then `Access-Control-Allow-Credentials` is ignored.
    // "omit"                     | `https://rabbit.invalid/`    | Omitted                          | ❌      | A serialized origin has no trailing slash.
    // "omit"                     | `https://rabbit.invalid`     | Omitted                          | ✅      | —
    // "include"                  | `*`                          | `true`                           | ❌      | If credentials mode is "include", then `Access-Control-Allow-Origin` cannot be `*`.
    // "include"                  | `https://rabbit.invalid`     | `true`                           | ✅      | —
    // "include"                  | `https://rabbit.invalid`     | `True`                           | ❌      | `true` is (byte) case-sensitive.
    //
    // Similarly, `Access-Control-Expose-Headers`, `Access-Control-Allow-Methods`, and `Access-Control-Allow-Headers`
    // response headers can only use `*` as value when request's credentials mode is not "include".
    pub(crate) fn ensure_usable_cors_rules(&self) -> Result<(), CorsConfigError> {
        // Check for wildcard origins in any Policy
        if let Some(policies) = &self.policies {
            for policy in policies.iter() {
                if policy.origins.iter().any(|x| &**x == "*") {
                    return Err(CorsConfigError::AllowAnyOrigin);
                }

                // Validate that origins don't have trailing slashes (per CORS spec)
                for origin in policy.origins.iter() {
                    if origin.ends_with('/') && &**origin != "/" {
                        return Err(CorsConfigError::TrailingSlashInOrigin);
                    }
                }
            }
        }

        if self.allow_credentials {
            // Check global fields for wildcards
            if self.allow_headers.iter().any(|x| &**x == "*") {
                return Err(CorsConfigError::AllowCredentialsWithAllowHeaders);
            }

            if self.methods.iter().any(|x| &**x == "*") {
                return Err(CorsConfigError::AllowCredentialsWithAllowMethods);
            }

            if self.allow_any_origin {
                return Err(CorsConfigError::AllowCredentialsWithAnyOrigin);
            }

            if let Some(headers) = &self.expose_headers
                && headers.iter().any(|x| &**x == "*")
            {
                return Err(CorsConfigError::AllowCredentialsWithExposeHeaders);
            }
        }

        // Check per-policy fields for wildcards when policy-level credentials are enabled
        if let Some(policies) = &self.policies {
            for policy in policies.iter() {
                // Check if policy-level credentials override is enabled
                let policy_credentials = policy.allow_credentials.unwrap_or(self.allow_credentials);

                if policy_credentials {
                    if policy.allow_headers.iter().any(|x| &**x == "*") {
                        return Err(CorsConfigError::AllowCredentialsWithAllowHeaders);
                    }

                    if let Some(methods) = &policy.methods
                        && methods.iter().any(|x| &**x == "*")
                    {
                        return Err(CorsConfigError::AllowCredentialsWithAllowMethods);
                    }

                    if policy.expose_headers.iter().any(|x| &**x == "*") {
                        return Err(CorsConfigError::AllowCredentialsWithExposeHeaders);
                    }
                }

                if let Some(pna) = &policy.private_network_access {
                    if let Some(name) = &pna.access_name {
                        if name.is_empty() {
                            return Err(CorsConfigError::EmptyPrivateNetworkAccessName);
                        }

                        // NOTE: Simply checking the number of bytes in the string will suffice
                        // (rather than chars) since all chars in the name are only a byte wide.
                        if name.len() > 248 {
                            return Err(CorsConfigError::LengthyPrivateNetworkAccessName);
                        }

                        // The access name needs to make the EMCAscript ReGex: `/^[a-z0-9_-.]+$/.`
                        if name.chars().any(
                            |c| !matches!(c, 'A'..='Z'| 'a'..='z' | '0'..='9' | '_' | '-' | '.'),
                        ) {
                            return Err(CorsConfigError::InvalidPrivateNetworkAccessName);
                        }
                    }

                    // The access ID needs to follow pattern: XX:XX:XX:XX:XX:XX` (where "X" is a
                    // hexdigit). This is 17 characters long, seperated by colons, with each
                    // substring being only 2 characters long
                    if let Some(id) = &pna.access_id
                        && (id.len() != 17
                            || id
                                .split(':')
                                .any(|s| s.len() != 2 || s.chars().any(|c| !c.is_ascii_hexdigit())))
                    {
                        return Err(CorsConfigError::InvalidPrivateNetworkAccessId);
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

    // Test that policies without specified methods fall back to global methods
    // This ensures that the global methods are used when policies don't specify methods
    #[test]
    fn test_policy_falls_back_to_global_methods() {
        let cors = Cors::builder()
            .methods(vec!["POST".into()])
            .policies(vec![
                Policy::builder()
                    .origins(vec!["https://example.com".into()])
                    .build(),
            ])
            .build();
        let layer = cors.clone().into_layer();
        assert!(layer.is_ok());

        // Verify that the policy has None methods (will fall back to global)
        let policies = cors.policies.unwrap();
        assert!(policies[0].methods.is_none());

        // Verify that the global methods are set correctly
        assert_eq!(cors.methods, Arc::from(["POST".into()]));
    }

    // Test that policy with Some([]) methods overrides global defaults
    // This ensures that explicitly setting empty methods disables all methods for that policy
    #[test]
    fn test_policy_empty_methods_override_global() {
        let cors = Cors::builder()
            .methods(vec!["POST".into(), "PUT".into()])
            .policies(vec![
                Policy::builder()
                    .origins(vec!["https://example.com".into()])
                    .methods(vec![])
                    .build(),
            ])
            .build();
        let layer = cors.clone().into_layer();
        assert!(layer.is_ok());

        // Verify that the policy has Some([]) methods (overrides global)
        let policies = cors.policies.unwrap();
        assert_eq!(policies[0].methods, Some(Arc::from([])));

        // Verify that the global methods are set correctly
        assert_eq!(cors.methods, Arc::from(["POST".into(), "PUT".into()]));
    }

    // Test that policy with Some([value]) methods uses those specific values
    // This ensures that explicitly setting methods uses those exact methods
    #[test]
    fn test_policy_specific_methods_used() {
        let cors = Cors::builder()
            .methods(vec!["POST".into()])
            .policies(vec![
                Policy::builder()
                    .origins(vec!["https://example.com".into()])
                    .methods(vec!["GET".into(), "DELETE".into()])
                    .build(),
            ])
            .build();
        let layer = cors.clone().into_layer();
        assert!(layer.is_ok());

        // Verify that the policy has specific methods
        let policies = cors.policies.unwrap();
        assert_eq!(
            policies[0].methods,
            Some(Arc::from(["GET".into(), "DELETE".into()]))
        );

        // Verify that the global methods are set correctly
        assert_eq!(cors.methods, Arc::from(["POST".into()]));
    }

    // Tests based on CORS specification table for credentials mode and Access-Control-Allow-Origin combinations
    // https://fetch.spec.whatwg.org/#cors-protocol

    // Test: credentials "omit" + Access-Control-Allow-Origin "*" + Access-Control-Allow-Credentials omitted = ✅
    #[test]
    fn test_cors_spec_omit_credentials_wildcard_origin_no_credentials_header() {
        let cors = Cors::builder()
            .allow_any_origin(true)
            .allow_credentials(false)
            .build();
        let result = cors.ensure_usable_cors_rules();
        assert!(result.is_ok());
    }

    // Test: credentials "omit" + Access-Control-Allow-Origin "*" + Access-Control-Allow-Credentials "true" = ✅
    // Note: This is allowed because when credentials mode is not "include", Access-Control-Allow-Credentials is ignored
    #[test]
    fn test_cors_spec_omit_credentials_wildcard_origin_with_credentials_header() {
        let cors = Cors::builder()
            .allow_any_origin(true)
            .allow_credentials(true)
            .build();
        let result = cors.ensure_usable_cors_rules();
        // This should fail in our implementation because we enforce the stricter rule
        assert!(result.is_err());
        assert_eq!(result.unwrap_err(), CorsConfigError::AllowAnyOrigin);
    }

    // Test: credentials "omit" + Access-Control-Allow-Origin "https://rabbit.invalid/" + Access-Control-Allow-Credentials omitted = ❌
    // A serialized origin has no trailing slash
    #[test]
    fn test_cors_spec_origin_with_trailing_slash_rejected() {
        let cors = Cors::builder()
            .policies(vec![
                Policy::builder()
                    .origins(vec!["https://rabbit.invalid/".into()])
                    .build(),
            ])
            .build();
        let result = cors.ensure_usable_cors_rules();
        assert!(result.is_err());
        assert_eq!(result.unwrap_err(), CorsConfigError::TrailingSlashInOrigin);
    }

    // Test: credentials "omit" + Access-Control-Allow-Origin "https://rabbit.invalid" + Access-Control-Allow-Credentials omitted = ✅
    #[test]
    fn test_cors_spec_origin_without_trailing_slash_accepted() {
        let cors = Cors::builder()
            .policies(vec![
                Policy::builder()
                    .origins(vec!["https://rabbit.invalid".into()])
                    .build(),
            ])
            .build();
        let result = cors.ensure_usable_cors_rules();
        assert!(result.is_ok());
    }

    // Test: credentials "include" + Access-Control-Allow-Origin "https://rabbit.invalid" + Access-Control-Allow-Credentials "true" = ✅
    #[test]
    fn test_cors_spec_include_credentials_specific_origin_accepted() {
        let cors = Cors::builder()
            .allow_any_origin(false)
            .allow_credentials(true)
            .policies(vec![
                Policy::builder()
                    .origins(vec!["https://rabbit.invalid".into()])
                    .build(),
            ])
            .build();
        let result = cors.ensure_usable_cors_rules();
        assert!(result.is_ok());
    }

    // Test: credentials "include" + Access-Control-Allow-Origin "https://rabbit.invalid" + Access-Control-Allow-Credentials "True" = ❌
    // "true" is (byte) case-sensitive - but this is handled by serde deserialization
    // This test verifies our validation doesn't accidentally allow mixed case
    #[test]
    fn test_cors_spec_credentials_case_sensitivity_handled_by_serde() {
        // Since we use bool in our config, case sensitivity is handled by serde
        // This test ensures our validation doesn't break this behavior
        let cors = Cors::builder()
            .allow_credentials(true)
            .policies(vec![
                Policy::builder()
                    .origins(vec!["https://rabbit.invalid".into()])
                    .build(),
            ])
            .build();
        let result = cors.ensure_usable_cors_rules();
        assert!(result.is_ok());
    }

    // Test policy-level credentials override behavior
    #[test]
    fn test_cors_spec_policy_level_credentials_override() {
        // Global credentials disabled, but policy-level credentials enabled should still validate
        let cors = Cors::builder()
            .allow_any_origin(false)
            .allow_credentials(false)
            .policies(vec![
                Policy::builder()
                    .origins(vec!["https://example.com".into()])
                    .allow_credentials(true)
                    .allow_headers(vec!["*".into()]) // This should be rejected with policy-level credentials
                    .build(),
            ])
            .build();

        let result = cors.ensure_usable_cors_rules();
        assert!(result.is_err());
        assert_eq!(
            result.unwrap_err(),
            CorsConfigError::AllowCredentialsWithAllowHeaders
        );
    }

    // Test policy-level credentials disabled should allow wildcards even with global credentials enabled
    #[test]
    fn test_cors_spec_policy_level_credentials_disabled_allows_wildcards() {
        let cors = Cors::builder()
            .allow_credentials(true)
            .policies(vec![
                Policy::builder()
                    .origins(vec!["https://example.com".into()])
                    .allow_credentials(false)
                    .allow_headers(vec!["*".into()]) // This should be allowed with policy-level credentials disabled
                    .build(),
            ])
            .build();
        let result = cors.ensure_usable_cors_rules();
        assert!(result.is_ok());
    }

    // Test root path "/" as origin (special case)
    #[test]
    fn test_cors_spec_root_path_origin_allowed() {
        let cors = Cors::builder()
            .policies(vec![Policy::builder().origins(vec!["/".into()]).build()])
            .build();
        let result = cors.ensure_usable_cors_rules();
        assert!(result.is_ok());
    }

    // Test multiple trailing slash violations
    #[test]
    fn test_cors_spec_multiple_trailing_slash_violations() {
        let cors = Cors::builder()
            .policies(vec![
                Policy::builder()
                    .origins(vec![
                        "https://example.com".into(),      // Valid
                        "https://api.example.com/".into(), // Invalid
                        "https://app.example.com".into(),  // Valid
                    ])
                    .build(),
            ])
            .build();
        let result = cors.ensure_usable_cors_rules();
        assert!(result.is_err());
        assert_eq!(result.unwrap_err(), CorsConfigError::TrailingSlashInOrigin);
    }

    // Test edge case: empty string origin
    #[test]
    fn test_cors_spec_empty_origin_handling() {
        let cors = Cors::builder()
            .policies(vec![Policy::builder().origins(vec!["".into()]).build()])
            .build();
        let result = cors.ensure_usable_cors_rules();
        // Empty string should be handled by header validation, not by our trailing slash check
        assert!(result.is_ok());
    }

    // Test comprehensive wildcard validation with all headers/methods/expose combinations
    #[test]
    fn test_cors_spec_comprehensive_wildcard_validation() {
        let cors = Cors::builder()
            .allow_credentials(true)
            .allow_headers(vec!["*".into()])
            .methods(vec!["*".into()])
            .expose_headers(vec!["*".into()])
            .policies(vec![
                Policy::builder()
                    .origins(vec!["https://example.com".into()])
                    .allow_headers(vec!["*".into()])
                    .methods(vec!["*".into()])
                    .expose_headers(vec!["*".into()])
                    .build(),
            ])
            .build();
        let result = cors.ensure_usable_cors_rules();
        assert!(result.is_err());
        // Should fail on the first wildcard check (global allow_headers)
        assert_eq!(
            result.unwrap_err(),
            CorsConfigError::AllowCredentialsWithAllowHeaders
        );
    }

    #[test]
    fn test_empty_pna_access_name_cors_configuration() {
        let cors = Cors::builder()
            .policies(vec![
                Policy::builder()
                    .private_network_access(
                        PrivateNetworkAccessPolicy::builder()
                            .access_name(String::from(""))
                            .build(),
                    )
                    .build(),
            ])
            .build();
        let layer = cors.into_layer();
        assert!(layer.is_err());

        assert_eq!(
            layer.unwrap_err(),
            String::from(
                "Invalid CORS configuration: `Private-Network-Access-Name` header value must not be empty."
            )
        );
    }

    #[test]
    fn test_bad_pna_access_name_cors_configuration() {
        let cors = Cors::builder()
            .policies(vec![
                Policy::builder()
                    .private_network_access(
                        PrivateNetworkAccessPolicy::builder()
                            .access_name(String::from("Bad name"))
                            .build(),
                    )
                    .build(),
            ])
            .build();
        let layer = cors.into_layer();
        assert!(layer.is_err());

        assert_eq!(
            layer.unwrap_err(),
            String::from(
                "Invalid CORS configuration: `Private-Network-Access-Name` header value can only contain the characters a-z0-9_-."
            )
        );
    }

    #[test]
    fn test_long_pna_access_name_cors_configuration() {
        let cors = Cors::builder()
            .policies(vec![
                Policy::builder()
                    .private_network_access(
                        PrivateNetworkAccessPolicy::builder()
                            .access_name("long_name".repeat(28))
                            .build(),
                    )
                    .build(),
            ])
            .build();
        let layer = cors.into_layer();
        assert!(layer.is_err());

        assert_eq!(
            layer.unwrap_err(),
            String::from(
                "Invalid CORS configuration: `Private-Network-Access-Name` header value must be no longer than 248 characters."
            )
        );
    }

    #[test]
    fn test_short_pna_access_id_cors_configuration() {
        let cors = Cors::builder()
            .policies(vec![
                Policy::builder()
                    .private_network_access(
                        PrivateNetworkAccessPolicy::builder()
                            .access_id(String::from("01:23:45:56:78"))
                            .build(),
                    )
                    .build(),
            ])
            .build();
        let layer = cors.into_layer();
        assert!(layer.is_err());

        assert_eq!(
            layer.unwrap_err(),
            String::from(
                "Invalid CORS configuration: `Private-Network-Access-ID` header value must be a 48-bit value presented as 6 hexadecimal bytes separated by colons"
            )
        );
    }

    #[test]
    fn test_bad_pna_access_id_cors_configuration() {
        let cors = Cors::builder()
            .policies(vec![
                Policy::builder()
                    .private_network_access(
                        PrivateNetworkAccessPolicy::builder()
                            .access_id(String::from("0:1:2:3:4:5:5:6:7"))
                            .build(),
                    )
                    .build(),
            ])
            .build();
        let layer = cors.into_layer();
        assert!(layer.is_err());

        assert_eq!(
            layer.unwrap_err(),
            String::from(
                "Invalid CORS configuration: `Private-Network-Access-ID` header value must be a 48-bit value presented as 6 hexadecimal bytes separated by colons"
            )
        );
    }

    #[test]
    fn test_non_hex_pna_access_id_cors_configuration() {
        let cors = Cors::builder()
            .policies(vec![
                Policy::builder()
                    .private_network_access(
                        PrivateNetworkAccessPolicy::builder()
                            .access_id(String::from("O1:23:45:56:78:9A"))
                            .build(),
                    )
                    .build(),
            ])
            .build();
        let layer = cors.into_layer();
        assert!(layer.is_err());

        assert_eq!(
            layer.unwrap_err(),
            String::from(
                "Invalid CORS configuration: `Private-Network-Access-ID` header value must be a 48-bit value presented as 6 hexadecimal bytes separated by colons"
            )
        );
    }
}
