use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;

/// Persisted Queries (PQ) configuration
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PersistedQueries {
    /// Activates Persisted Queries (disabled by default)
    #[serde(default = "default_pq")]
    pub enabled: bool,

    /// Enabling this field configures the router to log any freeform GraphQL request that is not in the persisted query list
    #[serde(default = "default_log_unknown")]
    pub log_unknown: bool,

    /// Restricts execution of operations that are not found in the Persisted Query List
    #[serde(default)]
    pub safelist: PersistedQueriesSafelist,
}

#[cfg(test)]
#[buildstructor::buildstructor]
impl PersistedQueries {
    #[builder]
    pub(crate) fn new(
        enabled: Option<bool>,
        log_unknown: Option<bool>,
        safelist: Option<PersistedQueriesSafelist>,
    ) -> Self {
        Self {
            enabled: enabled.unwrap_or_else(default_pq),
            safelist: safelist.unwrap_or_default(),
            log_unknown: log_unknown.unwrap_or_else(default_log_unknown),
        }
    }
}

/// Persisted Queries (PQ) Safelisting configuration
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PersistedQueriesSafelist {
    /// Enables using the persisted query list as a safelist (disabled by default)
    #[serde(default = "default_safelist")]
    pub enabled: bool,

    /// Enabling this field configures the router to reject any request that does not include the persisted query ID
    #[serde(default = "default_require_id")]
    pub require_id: bool,
}

#[cfg(test)]
#[buildstructor::buildstructor]
impl PersistedQueriesSafelist {
    #[builder]
    pub(crate) fn new(enabled: Option<bool>, require_id: Option<bool>) -> Self {
        Self {
            enabled: enabled.unwrap_or_else(default_safelist),
            require_id: require_id.unwrap_or_else(default_require_id),
        }
    }
}

impl Default for PersistedQueries {
    fn default() -> Self {
        Self {
            enabled: default_pq(),
            safelist: PersistedQueriesSafelist::default(),
            log_unknown: default_log_unknown(),
        }
    }
}

impl Default for PersistedQueriesSafelist {
    fn default() -> Self {
        Self {
            enabled: default_safelist(),
            require_id: default_require_id(),
        }
    }
}

const fn default_pq() -> bool {
    false
}

const fn default_safelist() -> bool {
    false
}

const fn default_require_id() -> bool {
    false
}

const fn default_log_unknown() -> bool {
    false
}
