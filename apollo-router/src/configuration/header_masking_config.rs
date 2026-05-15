use serde::Deserialize;
use serde::Serialize;

/// Configuration for header masking in logs and telemetry
#[derive(Clone, Debug, Deserialize, Serialize, schemars::JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct HeaderMaskingConfig {
    /// Enable header masking globally (default: true for fail-secure behavior)
    pub(crate) enabled: bool,

    /// Additional header names to mask (case-insensitive). By default these are
    /// merged with the built-in sensitive-header list (see
    /// `default_sensitive_headers`). Set `replace_defaults: true` to opt out of
    /// the built-ins and treat this list as authoritative.
    pub(crate) sensitive_headers: Vec<String>,

    /// When true, `sensitive_headers` replaces the built-in default list
    /// instead of extending it. Default: false (additive, fail-secure).
    pub(crate) replace_defaults: bool,
}

impl Default for HeaderMaskingConfig {
    fn default() -> Self {
        Self {
            enabled: default_enabled(),
            sensitive_headers: Vec::new(),
            replace_defaults: false,
        }
    }
}

impl HeaderMaskingConfig {
    /// Returns the effective sensitive-header list this config produces,
    /// merging `sensitive_headers` with the built-in defaults unless
    /// `replace_defaults` is set.
    pub(crate) fn effective_sensitive_headers(&self) -> Vec<String> {
        if self.replace_defaults {
            self.sensitive_headers.clone()
        } else {
            let mut headers = default_sensitive_headers();
            headers.extend(self.sensitive_headers.iter().cloned());
            headers
        }
    }
}

fn default_enabled() -> bool {
    true
}

pub(crate) fn default_sensitive_headers() -> Vec<String> {
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
        // sensitive_headers is empty in the struct (defaults are applied at
        // use time via effective_sensitive_headers, so user-provided lists are
        // additive rather than replacing).
        assert!(config.sensitive_headers.is_empty());
        assert!(!config.replace_defaults);

        // The effective list (what callers actually see) includes the
        // built-in fail-secure list.
        let effective = config.effective_sensitive_headers();
        assert!(effective.contains(&"authorization".to_string()));
        assert!(effective.contains(&"cookie".to_string()));
        assert!(effective.contains(&"x-api-key".to_string()));
    }

    #[test]
    fn effective_sensitive_headers_merges_user_list_with_defaults() {
        let config = HeaderMaskingConfig {
            enabled: true,
            sensitive_headers: vec!["x-my-secret".to_string()],
            replace_defaults: false,
        };
        let effective = config.effective_sensitive_headers();
        assert!(effective.contains(&"authorization".to_string()));
        assert!(effective.contains(&"cookie".to_string()));
        assert!(effective.contains(&"x-my-secret".to_string()));
    }

    #[test]
    fn effective_sensitive_headers_honors_replace_defaults() {
        let config = HeaderMaskingConfig {
            enabled: true,
            sensitive_headers: vec!["x-only-this".to_string()],
            replace_defaults: true,
        };
        let effective = config.effective_sensitive_headers();
        assert_eq!(effective, vec!["x-only-this".to_string()]);
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
        assert!(
            config
                .sensitive_headers
                .contains(&"custom-secret".to_string())
        );
        assert!(
            config
                .sensitive_headers
                .contains(&"x-internal-token".to_string())
        );
    }

    #[test]
    fn test_partial_config() {
        // Test that omitted fields take their struct defaults.
        let yaml = r#"
enabled: false
"#;

        let config: HeaderMaskingConfig = serde_yaml::from_str(yaml).unwrap();

        assert!(!config.enabled);
        // sensitive_headers defaults to empty; replace_defaults to false.
        assert!(config.sensitive_headers.is_empty());
        assert!(!config.replace_defaults);
    }
}
