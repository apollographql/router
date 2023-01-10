//! Plugins implementing router customizations.
//!
//! These plugins are compiled into the router and configured via YAML configuration.

macro_rules! schemar_fn {
    ($name:ident, $ty:ty, $description:expr) => {
      schemar_fn!($name, $ty, None, $description)
        }
    };

    ($name:ident, $ty:ty, $default:expr, $description:expr) => {
        // The macro will expand into the contents of this block.
        fn $name(gen: &mut schemars::gen::SchemaGenerator) -> schemars::schema::Schema {
            let schema = <$ty>::json_schema(gen);
            let mut schema = schema.into_object();
            let mut metadata = schemars::schema::Metadata::default();
            metadata.description = Some($description.to_string());
            metadata.default = Some($default);
            schema.metadata = Some(Box::new(metadata));
            schemars::schema::Schema::Object(schema)
        }
    };
}

pub(crate) mod csrf;
mod expose_query_plan;
mod external;
mod forbid_mutations;
mod headers;
mod include_subgraph_errors;
pub(crate) mod override_url;
pub(crate) mod rhai;
pub(crate) mod telemetry;
pub(crate) mod traffic_shaping;
