//! Support 1.x context key names in 2.x.

use crate::context::DEPRECATED_OPERATION_KIND;
use crate::context::DEPRECATED_OPERATION_NAME;
use crate::context::OPERATION_KIND;
use crate::context::OPERATION_NAME;
use crate::plugins::authentication::APOLLO_AUTHENTICATION_JWT_CLAIMS;
use crate::plugins::authentication::DEPRECATED_APOLLO_AUTHENTICATION_JWT_CLAIMS;
use crate::plugins::authorization::AUTHENTICATION_REQUIRED_KEY;
use crate::plugins::authorization::DEPRECATED_AUTHENTICATION_REQUIRED_KEY;
use crate::plugins::authorization::DEPRECATED_REQUIRED_POLICIES_KEY;
use crate::plugins::authorization::DEPRECATED_REQUIRED_SCOPES_KEY;
use crate::plugins::authorization::REQUIRED_POLICIES_KEY;
use crate::plugins::authorization::REQUIRED_SCOPES_KEY;
use crate::plugins::demand_control::COST_ACTUAL_KEY;
use crate::plugins::demand_control::COST_ESTIMATED_KEY;
use crate::plugins::demand_control::COST_RESULT_KEY;
use crate::plugins::demand_control::COST_STRATEGY_KEY;
use crate::plugins::demand_control::DEPRECATED_COST_ACTUAL_KEY;
use crate::plugins::demand_control::DEPRECATED_COST_ESTIMATED_KEY;
use crate::plugins::demand_control::DEPRECATED_COST_RESULT_KEY;
use crate::plugins::demand_control::DEPRECATED_COST_STRATEGY_KEY;
use crate::plugins::expose_query_plan::DEPRECATED_ENABLED_CONTEXT_KEY;
use crate::plugins::expose_query_plan::DEPRECATED_FORMATTED_QUERY_PLAN_CONTEXT_KEY;
use crate::plugins::expose_query_plan::DEPRECATED_QUERY_PLAN_CONTEXT_KEY;
use crate::plugins::expose_query_plan::ENABLED_CONTEXT_KEY;
use crate::plugins::expose_query_plan::FORMATTED_QUERY_PLAN_CONTEXT_KEY;
use crate::plugins::expose_query_plan::QUERY_PLAN_CONTEXT_KEY;
use crate::plugins::progressive_override::DEPRECATED_LABELS_TO_OVERRIDE_KEY;
use crate::plugins::progressive_override::DEPRECATED_UNRESOLVED_LABELS_KEY;
use crate::plugins::progressive_override::LABELS_TO_OVERRIDE_KEY;
use crate::plugins::progressive_override::UNRESOLVED_LABELS_KEY;
use crate::plugins::telemetry::CLIENT_NAME;
use crate::plugins::telemetry::CLIENT_VERSION;
use crate::plugins::telemetry::DEPRECATED_CLIENT_NAME;
use crate::plugins::telemetry::DEPRECATED_CLIENT_VERSION;
use crate::plugins::telemetry::DEPRECATED_STUDIO_EXCLUDE;
use crate::plugins::telemetry::DEPRECATED_SUBGRAPH_FTV1;
use crate::plugins::telemetry::STUDIO_EXCLUDE;
use crate::plugins::telemetry::SUBGRAPH_FTV1;
use crate::query_planner::APOLLO_OPERATION_ID;
use crate::query_planner::DEPRECATED_APOLLO_OPERATION_ID;
use crate::services::DEPRECATED_FIRST_EVENT_CONTEXT_KEY;
use crate::services::FIRST_EVENT_CONTEXT_KEY;
use crate::services::layers::apq::DEPRECATED_PERSISTED_QUERY_CACHE_HIT;
use crate::services::layers::apq::DEPRECATED_PERSISTED_QUERY_REGISTERED;
use crate::services::layers::apq::PERSISTED_QUERY_CACHE_HIT;
use crate::services::layers::apq::PERSISTED_QUERY_REGISTERED;

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
