use std::collections::HashMap;
use std::collections::HashSet;
use std::sync::Arc;
use std::sync::OnceLock;

use http::HeaderMap;
use http::HeaderValue;
use schemars::JsonSchema;
use serde::Deserialize;

use crate::Context;
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

pub(crate) const MASKED_VALUE: &str = "***MASKED***";

/// Compiled header masking rules for efficient lookup
#[derive(Clone, Debug, Default)]
pub(crate) struct HeaderMaskingRules {
    /// Set of sensitive header names (lowercase) that should be masked
    sensitive_headers: HashSet<String>,
}

impl HeaderMaskingRules {
    /// Create masking rules from configuration. Returns empty rules when `enabled: false`.
    ///
    /// The effective sensitive-header list is the merge of the built-in
    /// defaults and the user-provided `sensitive_headers`, unless
    /// `replace_defaults: true` is set (see
    /// [`HeaderMaskingConfig::effective_sensitive_headers`]).
    pub(crate) fn from_config(config: &HeaderMaskingConfig) -> Self {
        if !config.enabled {
            return Self::default();
        }
        let sensitive_headers = config
            .effective_sensitive_headers()
            .into_iter()
            .map(|h| h.to_lowercase())
            .collect();

        Self { sensitive_headers }
    }

    /// Check if a header should be masked (case-insensitive).
    ///
    /// The set entries are stored lowercase. `http::HeaderName::as_str()` is
    /// already canonical lowercase, which is the common caller — handle that
    /// without a `String` allocation. Header names are ASCII (RFC 9110 §5.1),
    /// so the only-uppercase case falls back to the explicit lowercased
    /// lookup; all other cases reuse the input borrow.
    pub(crate) fn should_mask(&self, header_name: &str) -> bool {
        if header_name.bytes().all(|b| !b.is_ascii_uppercase()) {
            self.sensitive_headers.contains(header_name)
        } else {
            self.sensitive_headers
                .contains(&header_name.to_ascii_lowercase())
        }
    }

    /// Mask already-externalized headers (the `HashMap<String, Vec<String>>`
    /// shape used in coprocessor payloads) for safe debug logging. Sensitive
    /// header values are replaced with `***MASKED***`; non-sensitive headers
    /// pass through unchanged.
    pub(crate) fn mask_externalized_headers(
        &self,
        input: &HashMap<String, Vec<String>>,
    ) -> HashMap<String, Vec<String>> {
        input
            .iter()
            .map(|(k, v)| {
                if self.should_mask(k) {
                    (k.clone(), vec![MASKED_VALUE.to_string()])
                } else {
                    (k.clone(), v.clone())
                }
            })
            .collect()
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

    fn rules_for(&self, direction: Direction, subgraph: Option<&str>) -> &Arc<HeaderMaskingRules> {
        match direction {
            Direction::Request => self.get_request(subgraph),
            Direction::Response => self.get_response(subgraph),
        }
    }
}

/// Request vs response masking direction. Selects which rule set a caller
/// consults via [`MaskingRulesMap::get_request`] / [`MaskingRulesMap::get_response`].
#[derive(Clone, Copy)]
pub(crate) enum Direction {
    Request,
    Response,
}

/// The built-in fail-secure masking rules (the default sensitive-header list).
///
/// Used when no per-request [`MaskingRulesMap`] is installed in context, so a
/// code path that bypasses the headers plugin still masks known secrets
/// (authorization, cookie, set-cookie, ...) rather than logging them in the
/// clear. The headers plugin is mandatory, so in the normal pipeline a map is
/// always present and this fallback only guards stray/synthesized requests.
fn default_masking_rules() -> &'static HeaderMaskingRules {
    static DEFAULT: OnceLock<HeaderMaskingRules> = OnceLock::new();
    DEFAULT.get_or_init(|| HeaderMaskingRules::from_config(&HeaderMaskingConfig::default()))
}

/// Whether `header_name` should be masked per the masking rules installed in
/// `context` for the given direction/subgraph. Falls back to the built-in
/// [`default_masking_rules`] (fail-secure) when no rules are present.
fn should_mask_header(
    context: &Context,
    direction: Direction,
    subgraph: Option<&str>,
    header_name: &str,
) -> bool {
    context.extensions().with_lock(|lock| match lock.get::<Arc<MaskingRulesMap>>() {
        Some(m) => m.rules_for(direction, subgraph).should_mask(header_name),
        None => default_masking_rules().should_mask(header_name),
    })
}

/// Resolve the value a telemetry header selector should emit, applying — in
/// priority order — the per-selector `redact` override and then the
/// global/per-subgraph masking rules from `context`. `value` is the raw header
/// value (`None` if the header is absent).
///
/// Defined once so every selector surface (router, supergraph, subgraph,
/// connector, http-client) shares identical redaction precedence rather than
/// re-implementing it per call site.
pub(crate) fn redact_header_value(
    context: &Context,
    direction: Direction,
    subgraph: Option<&str>,
    header_name: &str,
    value: Option<String>,
    redact: Option<&RedactMode>,
) -> Option<String> {
    match (redact, &value) {
        // An explicit per-selector override always wins.
        (Some(RedactMode::Allow), _) => value,
        (Some(RedactMode::Mask), Some(_)) => Some(MASKED_VALUE.to_string()),
        // Otherwise defer to the configured masking rules.
        (None, Some(_)) if should_mask_header(context, direction, subgraph, header_name) => {
            Some(MASKED_VALUE.to_string())
        }
        // Nothing to mask: absent value, or the rules say "show".
        _ => value,
    }
}

/// Render `headers` for debug logging, masking sensitive values per the rules
/// in `context` for the given direction/subgraph. Falls back to the built-in
/// [`default_masking_rules`] (fail-secure) when no masking rules are installed,
/// so a stray request can't log secrets in the clear.
///
/// Defined once so a new header-logging site can't forget to consult the rules.
pub(crate) fn masked_headers_for_log(
    context: &Context,
    direction: Direction,
    subgraph: Option<&str>,
    headers: &HeaderMap<HeaderValue>,
) -> String {
    context.extensions().with_lock(|lock| match lock.get::<Arc<MaskingRulesMap>>() {
        Some(m) => m.rules_for(direction, subgraph).mask_headers_debug(headers),
        None => default_masking_rules().mask_headers_debug(headers),
    })
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
            replace_defaults: false,
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
    fn test_empty_config_with_replace_defaults_masks_nothing() {
        let config = HeaderMaskingConfig {
            enabled: true,
            sensitive_headers: vec![],
            // Explicitly opt out of built-in defaults to make the empty list
            // authoritative.
            replace_defaults: true,
        };
        let rules = HeaderMaskingRules::from_config(&config);

        assert!(!rules.should_mask("authorization"));
        assert!(!rules.should_mask("cookie"));
    }

    #[test]
    fn empty_user_list_with_default_replace_defaults_still_masks_built_in_headers() {
        let config = HeaderMaskingConfig::default();
        let rules = HeaderMaskingRules::from_config(&config);

        // The fail-secure default: even without user config, the built-in
        // sensitive-header list is in effect.
        assert!(rules.should_mask("authorization"));
        assert!(rules.should_mask("cookie"));
    }

    #[test]
    fn test_masking_rules_map_separates_request_and_response() {
        // Use replace_defaults: true so each rule set contains exactly the
        // listed headers, isolating the request/response separation under test.
        let request_rules = Arc::new(HeaderMaskingRules::from_config(&HeaderMaskingConfig {
            enabled: true,
            sensitive_headers: vec!["authorization".to_string()],
            replace_defaults: true,
        }));
        let response_rules = Arc::new(HeaderMaskingRules::from_config(&HeaderMaskingConfig {
            enabled: true,
            sensitive_headers: vec!["set-cookie".to_string()],
            replace_defaults: true,
        }));
        let per_subgraph_response: HashMap<String, Arc<HeaderMaskingRules>> = [(
            "products".to_string(),
            Arc::new(HeaderMaskingRules::from_config(&HeaderMaskingConfig {
                enabled: true,
                sensitive_headers: vec!["x-products-secret".to_string()],
                replace_defaults: true,
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
}
