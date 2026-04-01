use serde::Deserialize;
use serde::Serialize;

/// Configuration for header masking in logs and telemetry
#[derive(Clone, Debug, Deserialize, Serialize, schemars::JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct HeaderMaskingConfig {
    /// Enable header masking globally (default: true for fail-secure behavior)
    #[serde(default = "default_enabled")]
    pub(crate) enabled: bool,

    /// List of header names to mask (case-insensitive)
    /// Default includes common sensitive headers
    #[serde(default = "default_sensitive_headers")]
    pub(crate) sensitive_headers: Vec<String>,
}

impl Default for HeaderMaskingConfig {
    fn default() -> Self {
        Self {
            enabled: default_enabled(),
            sensitive_headers: default_sensitive_headers(),
        }
    }
}

fn default_enabled() -> bool {
    true
}

fn default_sensitive_headers() -> Vec<String> {
    vec![
        // Authentication and authorization
        "authorization".to_string(),
        "proxy-authorization".to_string(),
        "proxy-authenticate".to_string(),

        // Session management
        "cookie".to_string(),
        "set-cookie".to_string(),

        // API keys
        "x-api-key".to_string(),
        "api-key".to_string(),

        // Auth tokens
        "x-auth-token".to_string(),
        "x-session-id".to_string(),
        "x-session-token".to_string(),

        // CSRF protection
        "x-csrf-token".to_string(),
        "x-xsrf-token".to_string(),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let config = HeaderMaskingConfig::default();

        assert!(config.enabled);
        assert!(!config.sensitive_headers.is_empty());

        // Verify common sensitive headers are included
        assert!(config.sensitive_headers.contains(&"authorization".to_string()));
        assert!(config.sensitive_headers.contains(&"cookie".to_string()));
        assert!(config.sensitive_headers.contains(&"x-api-key".to_string()));
    }

    #[test]
    fn test_custom_config() {
        let yaml = r#"
enabled: false
sensitive_headers:
  - custom-secret
  - x-internal-token
"#;

        let config: HeaderMaskingConfig = serde_yaml::from_str(yaml).unwrap();

        assert!(!config.enabled);
        assert_eq!(config.sensitive_headers.len(), 2);
        assert!(config.sensitive_headers.contains(&"custom-secret".to_string()));
        assert!(config.sensitive_headers.contains(&"x-internal-token".to_string()));
    }

    #[test]
    fn test_partial_config() {
        // Test that defaults are applied when fields are omitted
        let yaml = r#"
enabled: false
"#;

        let config: HeaderMaskingConfig = serde_yaml::from_str(yaml).unwrap();

        assert!(!config.enabled);
        // Should still have default sensitive headers
        assert!(!config.sensitive_headers.is_empty());
    }
}
