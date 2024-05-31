use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;

/// Persisted Queries (PQ) configuration
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields, default)]
pub struct PersistedQueries {
    /// Activates Persisted Queries (disabled by default)
    pub enabled: bool,

    /// Enabling this field configures the router to log any freeform GraphQL request that is not in the persisted query list
    pub log_unknown: bool,

    /// Restricts execution of operations that are not found in the Persisted Query List
    pub safelist: PersistedQueriesSafelist,

    /// Experimental feature to prewarm the query plan cache with persisted queries
    pub experimental_prewarm_query_plan_cache: bool,
}

#[cfg(test)]
#[buildstructor::buildstructor]
impl PersistedQueries {
    #[builder]
    pub(crate) fn new(
        enabled: Option<bool>,
        log_unknown: Option<bool>,
        safelist: Option<PersistedQueriesSafelist>,
        experimental_prewarm_query_plan_cache: Option<bool>,
    ) -> Self {
        Self {
            enabled: enabled.unwrap_or_else(default_pq),
            safelist: safelist.unwrap_or_default(),
            log_unknown: log_unknown.unwrap_or_else(default_log_unknown),
            experimental_prewarm_query_plan_cache: experimental_prewarm_query_plan_cache
                .unwrap_or_else(default_prewarm_query_plan_cache),
        }
    }
}

/// Persisted Queries (PQ) Safelisting configuration
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields, default)]
pub struct PersistedQueriesSafelist {
    /// Enables using the persisted query list as a safelist (disabled by default)
    pub enabled: bool,

    /// Enabling this field configures the router to reject any request that does not include the persisted query ID
    pub require_id: bool,

    /// Enables using a local copy of the persisted query list to safelist operations
    pub local_safelist: Option<String>,
}

#[cfg(test)]
#[buildstructor::buildstructor]
impl PersistedQueriesSafelist {
    #[builder]
    pub(crate) fn new(
        enabled: Option<bool>,
        require_id: Option<bool>,
        local_safelist: Option<String>,
    ) -> Self {
        Self {
            enabled: enabled.unwrap_or_else(default_safelist),
            require_id: require_id.unwrap_or_else(default_require_id),
            local_safelist: local_safelist,
        }
    }
}

impl Default for PersistedQueries {
    fn default() -> Self {
        Self {
            enabled: default_pq(),
            safelist: PersistedQueriesSafelist::default(),
            log_unknown: default_log_unknown(),
            experimental_prewarm_query_plan_cache: default_prewarm_query_plan_cache(),
        }
    }
}

impl Default for PersistedQueriesSafelist {
    fn default() -> Self {
        Self {
            enabled: default_safelist(),
            require_id: default_require_id(),
            local_safelist: None,
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

const fn default_prewarm_query_plan_cache() -> bool {
    false
}
