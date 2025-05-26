//! Support 1.x context key names in 2.x.

use crate::context::OPERATION_KIND;
use crate::context::OPERATION_NAME;
use crate::plugins::authentication::APOLLO_AUTHENTICATION_JWT_CLAIMS;
use crate::plugins::authorization::AUTHENTICATION_REQUIRED_KEY;
use crate::plugins::authorization::REQUIRED_POLICIES_KEY;
use crate::plugins::authorization::REQUIRED_SCOPES_KEY;
use crate::plugins::demand_control::COST_ACTUAL_KEY;
use crate::plugins::demand_control::COST_ESTIMATED_KEY;
use crate::plugins::demand_control::COST_RESULT_KEY;
use crate::plugins::demand_control::COST_STRATEGY_KEY;
use crate::plugins::expose_query_plan::ENABLED_CONTEXT_KEY;
use crate::plugins::expose_query_plan::FORMATTED_QUERY_PLAN_CONTEXT_KEY;
use crate::plugins::expose_query_plan::QUERY_PLAN_CONTEXT_KEY;
use crate::plugins::progressive_override::LABELS_TO_OVERRIDE_KEY;
use crate::plugins::progressive_override::UNRESOLVED_LABELS_KEY;
use crate::plugins::telemetry::CLIENT_NAME;
use crate::plugins::telemetry::CLIENT_VERSION;
use crate::plugins::telemetry::STUDIO_EXCLUDE;
use crate::plugins::telemetry::SUBGRAPH_FTV1;
use crate::query_planner::APOLLO_OPERATION_ID;
use crate::services::FIRST_EVENT_CONTEXT_KEY;
use crate::services::layers::apq::PERSISTED_QUERY_CACHE_HIT;
use crate::services::layers::apq::PERSISTED_QUERY_REGISTERED;

// From crate::context
pub(crate) const DEPRECATED_OPERATION_NAME: &str = "operation_name";
pub(crate) const DEPRECATED_OPERATION_KIND: &str = "operation_kind";

// crate::plugins::authentication
pub(crate) const DEPRECATED_APOLLO_AUTHENTICATION_JWT_CLAIMS: &str =
    "apollo_authentication::JWT::claims";

// crate::plugins::authorization
pub(crate) const DEPRECATED_AUTHENTICATION_REQUIRED_KEY: &str =
    "apollo_authorization::authenticated::required";
pub(crate) const DEPRECATED_REQUIRED_SCOPES_KEY: &str = "apollo_authorization::scopes::required";
pub(crate) const DEPRECATED_REQUIRED_POLICIES_KEY: &str =
    "apollo_authorization::policies::required";

// crate::plugins::demand_control
pub(crate) const DEPRECATED_COST_ESTIMATED_KEY: &str = "cost.estimated";
pub(crate) const DEPRECATED_COST_ACTUAL_KEY: &str = "cost.actual";
pub(crate) const DEPRECATED_COST_RESULT_KEY: &str = "cost.result";
pub(crate) const DEPRECATED_COST_STRATEGY_KEY: &str = "cost.strategy";

// crate::plugins::expose_query_plan
pub(crate) const DEPRECATED_QUERY_PLAN_CONTEXT_KEY: &str = "experimental::expose_query_plan.plan";
pub(crate) const DEPRECATED_FORMATTED_QUERY_PLAN_CONTEXT_KEY: &str =
    "experimental::expose_query_plan.formatted_plan";
pub(crate) const DEPRECATED_ENABLED_CONTEXT_KEY: &str = "experimental::expose_query_plan.enabled";

// crate::plugins::progressive_override
pub(crate) const DEPRECATED_UNRESOLVED_LABELS_KEY: &str = "apollo_override::unresolved_labels";
pub(crate) const DEPRECATED_LABELS_TO_OVERRIDE_KEY: &str = "apollo_override::labels_to_override";

// crate::plugins::telemetry
pub(crate) const DEPRECATED_CLIENT_NAME: &str = "apollo_telemetry::client_name";
pub(crate) const DEPRECATED_CLIENT_VERSION: &str = "apollo_telemetry::client_version";
pub(crate) const DEPRECATED_SUBGRAPH_FTV1: &str = "apollo_telemetry::subgraph_ftv1";
pub(crate) const DEPRECATED_STUDIO_EXCLUDE: &str = "apollo_telemetry::studio::exclude";

// crate::query_planner::caching_query_planner
pub(crate) const DEPRECATED_APOLLO_OPERATION_ID: &str = "apollo_operation_id";

// crate::services::supergraph::service
pub(crate) const DEPRECATED_FIRST_EVENT_CONTEXT_KEY: &str =
    "apollo_router::supergraph::first_event";

// crate::services::layers::apq
pub(crate) const DEPRECATED_PERSISTED_QUERY_CACHE_HIT: &str = "persisted_query_hit";
pub(crate) const DEPRECATED_PERSISTED_QUERY_REGISTERED: &str = "persisted_query_register";

/// Generate the function pair with a macro to be sure that they handle all the same keys.
macro_rules! make_deprecated_key_conversions {
    ( $( $new:ident => $deprecated:ident, )* ) => {
        /// Convert context key to the deprecated context key (mainly useful for coprocessor/rhai)
        /// If the context key is not part of a deprecated one it just returns the original one because it doesn't have to be renamed
        pub(crate) fn context_key_to_deprecated(key: String) -> String {
            match key.as_str() {
                $( $new => $deprecated.to_string(), )*
                _ => key,
            }
        }

        /// Convert context key from deprecated to new one (mainly useful for coprocessor/rhai)
        /// If the context key is not part of a deprecated one it just returns the original one because it doesn't have to be renamed
        pub(crate) fn context_key_from_deprecated(key: String) -> String {
            match key.as_str() {
                $( $deprecated => $new.to_string(), )*
                _ => key,
            }
        }
    };
}

make_deprecated_key_conversions!(
    OPERATION_NAME => DEPRECATED_OPERATION_NAME,
    OPERATION_KIND => DEPRECATED_OPERATION_KIND,
    APOLLO_AUTHENTICATION_JWT_CLAIMS => DEPRECATED_APOLLO_AUTHENTICATION_JWT_CLAIMS,
    AUTHENTICATION_REQUIRED_KEY => DEPRECATED_AUTHENTICATION_REQUIRED_KEY,
    REQUIRED_SCOPES_KEY => DEPRECATED_REQUIRED_SCOPES_KEY,
    REQUIRED_POLICIES_KEY => DEPRECATED_REQUIRED_POLICIES_KEY,
    APOLLO_OPERATION_ID => DEPRECATED_APOLLO_OPERATION_ID,
    UNRESOLVED_LABELS_KEY => DEPRECATED_UNRESOLVED_LABELS_KEY,
    LABELS_TO_OVERRIDE_KEY => DEPRECATED_LABELS_TO_OVERRIDE_KEY,
    FIRST_EVENT_CONTEXT_KEY => DEPRECATED_FIRST_EVENT_CONTEXT_KEY,
    CLIENT_NAME => DEPRECATED_CLIENT_NAME,
    CLIENT_VERSION => DEPRECATED_CLIENT_VERSION,
    STUDIO_EXCLUDE => DEPRECATED_STUDIO_EXCLUDE,
    SUBGRAPH_FTV1 => DEPRECATED_SUBGRAPH_FTV1,
    COST_ESTIMATED_KEY => DEPRECATED_COST_ESTIMATED_KEY,
    COST_ACTUAL_KEY => DEPRECATED_COST_ACTUAL_KEY,
    COST_RESULT_KEY => DEPRECATED_COST_RESULT_KEY,
    COST_STRATEGY_KEY => DEPRECATED_COST_STRATEGY_KEY,
    ENABLED_CONTEXT_KEY => DEPRECATED_ENABLED_CONTEXT_KEY,
    FORMATTED_QUERY_PLAN_CONTEXT_KEY => DEPRECATED_FORMATTED_QUERY_PLAN_CONTEXT_KEY,
    QUERY_PLAN_CONTEXT_KEY => DEPRECATED_QUERY_PLAN_CONTEXT_KEY,
    PERSISTED_QUERY_CACHE_HIT => DEPRECATED_PERSISTED_QUERY_CACHE_HIT,
    PERSISTED_QUERY_REGISTERED => DEPRECATED_PERSISTED_QUERY_REGISTERED,
);
