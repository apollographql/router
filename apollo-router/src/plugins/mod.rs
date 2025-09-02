//! Plugins implementing router customizations.
//!
//! These plugins are compiled into the router and configured via YAML configuration.

macro_rules! schemar_fn {
    ($name:ident, $ty:ty, $description:expr) => {
        schemar_fn!($name, $ty, None, $description);
    };

    ($name:ident, $ty:ty, $default:expr, $description:expr) => {
        fn $name(generator: &mut schemars::r#gen::SchemaGenerator) -> schemars::schema::Schema {
            let schema = <$ty>::json_schema(generator);
            let mut schema = schema.into_object();
            let mut metadata = schemars::schema::Metadata::default();
            metadata.description = Some($description.to_string());
            metadata.default = $default;
            schema.metadata = Some(Box::new(metadata));
            schemars::schema::Schema::Object(schema)
        }
    };
}

pub(crate) mod authentication;
pub(crate) mod authorization;
pub(crate) mod cache;
pub(crate) mod chaos;
pub(crate) mod connectors;
mod coprocessor;
pub(crate) mod cors;
pub(crate) mod csrf;
pub(crate) mod demand_control;
pub(crate) mod enhanced_client_awareness;
pub(crate) mod expose_query_plan;
pub(crate) mod file_uploads;
mod fleet_detector;
mod forbid_mutations;
mod headers;
pub(crate) mod healthcheck;
mod include_subgraph_errors;
pub(crate) mod license_enforcement;
pub(crate) mod limits;
pub(crate) mod mock_subgraphs;
pub(crate) mod override_url;
pub(crate) mod progressive_override;
mod record_replay;
pub(crate) mod response_cache;
pub(crate) mod rhai;
pub(crate) mod subscription;
pub(crate) mod telemetry;
#[cfg(test)]
pub(crate) mod test;
pub(crate) mod traffic_shaping;
