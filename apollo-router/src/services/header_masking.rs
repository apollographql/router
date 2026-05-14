use std::collections::HashMap;
use std::collections::HashSet;
use std::sync::Arc;

use http::HeaderMap;
use http::HeaderValue;
use schemars::JsonSchema;
use serde::Deserialize;

use crate::configuration::header_masking_config::HeaderMaskingConfig;

/// Per-selector masking override. `Allow` shows the raw header value; `Mask`
/// always replaces it with `***MASKED***`. When unset, the selector defers to
/// the global request/response rules in `MaskingRulesMap`.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum RedactMode {
    /// Always show the header value, ignoring any global masking rules.
    Allow,
    /// Always mask the header value, regardless of global rules.
    Mask,
}

const MASKED_VALUE: &str = "***MASKED***";

/// Compiled header masking rules for efficient lookup
#[derive(Clone, Debug, Default)]
pub(crate) struct HeaderMaskingRules {
    /// Set of sensitive header names (lowercase) that should be masked
    sensitive_headers: HashSet<String>,
}

impl HeaderMaskingRules {
    /// Create masking rules from configuration. Returns empty rules when `enabled: false`.
    pub(crate) fn from_config(config: &HeaderMaskingConfig) -> Self {
        if !config.enabled {
            return Self::default();
        }
        let sensitive_headers = config
            .sensitive_headers
            .iter()
            .map(|h| h.to_lowercase())
            .collect();

        Self { sensitive_headers }
    }

    /// Check if a header should be masked (case-insensitive)
    pub(crate) fn should_mask(&self, header_name: &str) -> bool {
        self.sensitive_headers.contains(&header_name.to_lowercase())
    }

    /// Mask a HeaderMap and convert to HashMap for coprocessor
    #[allow(dead_code)]
    pub(crate) fn mask_header_map(
        &self,
        input: &HeaderMap<HeaderValue>,
    ) -> HashMap<String, Vec<String>> {
        let mut output = HashMap::with_capacity(input.keys_len());

        for (k, v) in input {
            let k_str = k.as_str();
            let should_mask = self.should_mask(k_str);

            match String::from_utf8(v.as_bytes().to_vec()) {
                Ok(v) => {
                    let value = if should_mask {
                        MASKED_VALUE.to_string()
                    } else {
                        v
                    };
                    output
                        .entry(k_str.to_owned())
                        .or_insert_with(Vec::new)
                        .push(value);
                }
                Err(e) => {
                    tracing::warn!(
                        "unable to convert header value to utf-8 for {}, will not be sent to coprocessor: {}",
                        k_str,
                        e
                    );
                }
            }
        }

        output
    }

    /// Mask headers in Debug format string for telemetry events
    pub(crate) fn mask_headers_debug(&self, input: &HeaderMap<HeaderValue>) -> String {
        let mut parts = Vec::with_capacity(input.len());

        for (k, v) in input {
            let k_str = k.as_str();
            let value_str = if self.should_mask(k_str) {
                MASKED_VALUE
            } else {
                v.to_str().unwrap_or("<non-utf8>")
            };

            // Use Debug formatting so embedded quotes/backslashes/control chars are
            // properly escaped — avoids invalid JSON and log-injection vectors via
            // attacker-influenceable header values (Cookie, Referer, User-Agent, ...).
            parts.push(format!("{k_str:?}: {value_str:?}"));
        }

        format!("{{{}}}", parts.join(", "))
    }
}

/// Per-direction rules: a global default plus optional per-subgraph overrides.
#[derive(Debug, Default)]
pub(crate) struct DirectionRules {
    global: Arc<HeaderMaskingRules>,
    per_subgraph: HashMap<String, Arc<HeaderMaskingRules>>,
}

impl DirectionRules {
    pub(crate) fn new(
        global: Arc<HeaderMaskingRules>,
        per_subgraph: HashMap<String, Arc<HeaderMaskingRules>>,
    ) -> Self {
        Self {
            global,
            per_subgraph,
        }
    }

    fn get(&self, subgraph_name: Option<&str>) -> &Arc<HeaderMaskingRules> {
        subgraph_name
            .and_then(|n| self.per_subgraph.get(n))
            .unwrap_or(&self.global)
    }
}

/// A write-once masking rules map stored in the request context.
///
/// Inserted by the headers plugin at router-service time so all stages (router,
/// supergraph, subgraph, connector) read a consistent, immutable snapshot.
/// Request and response directions are configured independently; callers must
/// pick the matching direction via [`get_request`] or [`get_response`].
#[derive(Debug)]
pub(crate) struct MaskingRulesMap {
    request: DirectionRules,
    response: DirectionRules,
}

impl MaskingRulesMap {
    pub(crate) fn new(request: DirectionRules, response: DirectionRules) -> Self {
        Self { request, response }
    }

    /// Test helper: build a map that applies the same rules in both directions.
    /// Real config builds the two directions independently.
    #[cfg(test)]
    pub(crate) fn new_test(
        global: Arc<HeaderMaskingRules>,
        per_subgraph: HashMap<String, Arc<HeaderMaskingRules>>,
    ) -> Self {
        Self::new(
            DirectionRules::new(global.clone(), per_subgraph.clone()),
            DirectionRules::new(global, per_subgraph),
        )
    }

    /// Returns the request-side masking rules for the given subgraph (or the
    /// global request rules when `subgraph_name` is `None` or unknown).
    pub(crate) fn get_request(&self, subgraph_name: Option<&str>) -> &Arc<HeaderMaskingRules> {
        self.request.get(subgraph_name)
    }

    /// Returns the response-side masking rules for the given subgraph (or the
    /// global response rules when `subgraph_name` is `None` or unknown).
    pub(crate) fn get_response(&self, subgraph_name: Option<&str>) -> &Arc<HeaderMaskingRules> {
        self.response.get(subgraph_name)
    }
}

#[cfg(test)]
mod tests {
    use http::header::HeaderName;

    use super::*;

    fn create_test_rules() -> HeaderMaskingRules {
        let config = HeaderMaskingConfig {
            enabled: true,
            sensitive_headers: vec![
                "authorization".to_string(),
                "cookie".to_string(),
                "x-api-key".to_string(),
            ],
        };
        HeaderMaskingRules::from_config(&config)
    }

    #[test]
    fn test_should_mask_case_insensitive() {
        let rules = create_test_rules();

        // Test exact match
        assert!(rules.should_mask("authorization"));
        assert!(rules.should_mask("cookie"));
        assert!(rules.should_mask("x-api-key"));

        // Test case insensitivity
        assert!(rules.should_mask("Authorization"));
        assert!(rules.should_mask("AUTHORIZATION"));
        assert!(rules.should_mask("Cookie"));
        assert!(rules.should_mask("X-API-KEY"));
        assert!(rules.should_mask("X-Api-Key"));

        // Test non-matching headers
        assert!(!rules.should_mask("content-type"));
        assert!(!rules.should_mask("accept"));
        assert!(!rules.should_mask("x-custom-header"));
    }

    #[test]
    fn test_mask_header_map() {
        let rules = create_test_rules();
        let mut headers = HeaderMap::new();

        headers.insert(
            HeaderName::from_static("authorization"),
            HeaderValue::from_static("Bearer secret-token"), // gitleaks:allow
        );
        headers.insert(
            HeaderName::from_static("content-type"),
            HeaderValue::from_static("application/json"),
        );
        headers.insert(
            HeaderName::from_static("cookie"),
            HeaderValue::from_static("session=abc123"),
        );

        let result = rules.mask_header_map(&headers);

        // Sensitive headers should be masked
        assert_eq!(
            result.get("authorization"),
            Some(&vec![MASKED_VALUE.to_string()])
        );
        assert_eq!(result.get("cookie"), Some(&vec![MASKED_VALUE.to_string()]));

        // Non-sensitive headers should not be masked
        assert_eq!(
            result.get("content-type"),
            Some(&vec!["application/json".to_string()])
        );
    }

    #[test]
    fn test_mask_header_map_multiple_values() {
        let rules = create_test_rules();
        let mut headers = HeaderMap::new();

        // HTTP allows multiple values for the same header
        headers.append(
            HeaderName::from_static("cookie"),
            HeaderValue::from_static("session=abc123"),
        );
        headers.append(
            HeaderName::from_static("cookie"),
            HeaderValue::from_static("user=john"),
        );

        let result = rules.mask_header_map(&headers);

        // All values should be masked
        assert_eq!(
            result.get("cookie"),
            Some(&vec![MASKED_VALUE.to_string(), MASKED_VALUE.to_string()])
        );
    }

    #[test]
    fn test_mask_headers_debug() {
        let rules = create_test_rules();
        let mut headers = HeaderMap::new();

        headers.insert(
            HeaderName::from_static("authorization"),
            HeaderValue::from_static("Bearer secret-token"), // gitleaks:allow
        );
        headers.insert(
            HeaderName::from_static("content-type"),
            HeaderValue::from_static("application/json"),
        );

        let result = rules.mask_headers_debug(&headers);

        // Should contain masked authorization
        assert!(result.contains("authorization"));
        assert!(result.contains(MASKED_VALUE));
        assert!(!result.contains("secret-token"));

        // Should contain unmasked content-type
        assert!(result.contains("content-type"));
        assert!(result.contains("application/json"));
    }

    #[test]
    fn test_mask_headers_debug_escapes_special_characters() {
        let rules = create_test_rules();
        let mut headers = HeaderMap::new();

        // A header value containing quotes and backslashes — exactly the shape
        // that broke the prior naive "{}": "{}" formatter.
        headers.insert(
            HeaderName::from_static("etag"),
            HeaderValue::from_static(r#""abc\123""#),
        );

        let result = rules.mask_headers_debug(&headers);

        // Quotes inside the value should be escaped (Debug formatting), keeping
        // the rendered string a valid JSON-ish key/value pair.
        assert!(
            result.contains(r#""etag": "\"abc\\123\"""#),
            "expected escaped value, got: {result}"
        );
    }

    #[test]
    fn test_empty_config() {
        let config = HeaderMaskingConfig {
            enabled: true,
            sensitive_headers: vec![],
        };
        let rules = HeaderMaskingRules::from_config(&config);

        // No headers should be masked with empty config
        assert!(!rules.should_mask("authorization"));
        assert!(!rules.should_mask("cookie"));
    }

    #[test]
    fn test_masking_rules_map_separates_request_and_response() {
        let request_rules = Arc::new(HeaderMaskingRules::from_config(&HeaderMaskingConfig {
            enabled: true,
            sensitive_headers: vec!["authorization".to_string()],
        }));
        let response_rules = Arc::new(HeaderMaskingRules::from_config(&HeaderMaskingConfig {
            enabled: true,
            sensitive_headers: vec!["set-cookie".to_string()],
        }));
        let per_subgraph_response: HashMap<String, Arc<HeaderMaskingRules>> = [(
            "products".to_string(),
            Arc::new(HeaderMaskingRules::from_config(&HeaderMaskingConfig {
                enabled: true,
                sensitive_headers: vec!["x-products-secret".to_string()],
            })),
        )]
        .into_iter()
        .collect();

        let map = MaskingRulesMap::new(
            DirectionRules::new(request_rules, HashMap::new()),
            DirectionRules::new(response_rules, per_subgraph_response),
        );

        // Request side masks authorization, NOT set-cookie.
        assert!(map.get_request(None).should_mask("authorization"));
        assert!(!map.get_request(None).should_mask("set-cookie"));

        // Response side masks set-cookie (global), NOT authorization.
        assert!(map.get_response(None).should_mask("set-cookie"));
        assert!(!map.get_response(None).should_mask("authorization"));

        // Per-subgraph response override applies.
        assert!(
            map.get_response(Some("products"))
                .should_mask("x-products-secret")
        );
        // Unknown subgraph falls back to global response rules.
        assert!(map.get_response(Some("nobody")).should_mask("set-cookie"));
    }

    #[test]
    fn test_mask_header_map_case_insensitive_in_headermap() {
        let rules = create_test_rules();
        let mut headers = HeaderMap::new();

        // HeaderMap normalizes to lowercase, but test with mixed case in value
        headers.insert(
            HeaderName::from_static("authorization"),
            HeaderValue::from_static("Bearer SECRET"), // gitleaks:allow
        );

        // Even though the header name is lowercase in HeaderMap, our rule should match
        let result = rules.mask_header_map(&headers);
        assert_eq!(
            result.get("authorization"),
            Some(&vec![MASKED_VALUE.to_string()])
        );
    }
}
