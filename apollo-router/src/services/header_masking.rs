use std::collections::HashMap;
use std::collections::HashSet;

use http::HeaderMap;
use http::HeaderValue;

use crate::configuration::header_masking_config::HeaderMaskingConfig;

const MASKED_VALUE: &str = "***MASKED***";

/// Compiled header masking rules for efficient lookup
#[derive(Clone, Debug)]
pub(crate) struct HeaderMaskingRules {
    /// Set of sensitive header names (lowercase) that should be masked
    sensitive_headers: HashSet<String>,
}

impl HeaderMaskingRules {
    /// Create masking rules from configuration
    pub(crate) fn from_config(config: &HeaderMaskingConfig) -> Self {
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

            parts.push(format!("\"{}\": \"{}\"", k_str, value_str));
        }

        format!("{{{}}}", parts.join(", "))
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
