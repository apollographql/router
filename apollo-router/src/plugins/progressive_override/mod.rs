use std::collections::HashMap;
use std::collections::HashSet;

use crate::plugin::Plugin;
use crate::plugin::PluginInit;
use crate::register_plugin;
use crate::services::*;
use apollo_compiler::schema::ExtendedType;
use apollo_compiler::Schema;
use schemars::JsonSchema;
use serde::Deserialize;
use tower::BoxError;
use tower::ServiceBuilder;
use tower::ServiceExt;

// TODO: telemetry & analytics??
// entitlement?
// tracing::info!(
//                         monotonic_counter.apollo.router.operations.authorization = 1u64,
//                         authorization.filtered = filtered,
//                         authorization.needs_authenticated = needs_authenticated,
//                         authorization.needs_requires_scopes = needs_requires_scopes,
//                     );
pub(crate) const OVERRIDE_KEY: &str = "apollo_override::override_labels";

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub(crate) struct Conf {}

pub(crate) struct ProgressiveOverridePlugin {
    label_to_percentage_map: HashMap<String, f64>,
}

fn collect_static_percentages_from_schema(schema: Schema) -> HashMap<String, f64> {
    let mut static_percentages = HashMap::new();
    for extended_type in schema.types.values() {
        if let ExtendedType::Object(object_type) = extended_type {
            for field in object_type.fields.values() {
                if let Some(label_arg) = field
                    .directives
                    .iter()
                    .find(|d| d.name == "join__field")
                    .and_then(|d| d.arguments.iter().find(|a| a.name == "overrideLabel"))
                    .and_then(|a| a.value.as_str())
                {
                    if let Some(percent_as_str) = label_arg
                        .strip_prefix("percent(")
                        .and_then(|s| s.strip_suffix(")"))
                    {
                        if let Ok(parsed_percent) = percent_as_str.parse::<f64>() {
                            static_percentages.insert(label_arg.to_owned(), parsed_percent);
                        }
                    }
                }
            }
        }
    }
    tracing::info!("static_percentages: {:?}", &static_percentages);
    static_percentages
}

#[async_trait::async_trait]
impl Plugin for ProgressiveOverridePlugin {
    type Config = Conf;

    async fn new(init: PluginInit<Self::Config>) -> Result<Self, BoxError> {
        Ok(ProgressiveOverridePlugin {
            label_to_percentage_map: collect_static_percentages_from_schema(
                Schema::parse(&*init.supergraph_sdl, "schema.graphql").unwrap(),
            ),
        })
    }

    fn supergraph_service(&self, service: supergraph::BoxService) -> supergraph::BoxService {
        // bypass plugin if we didn't find any override labels in the supergraph
        if self.label_to_percentage_map.is_empty() {
            service
        } else {
            let label_to_percentage_map = self.label_to_percentage_map.clone();
            ServiceBuilder::new()
                .map_request(move |request: supergraph::Request| {
                    let mut override_labels = HashSet::new();
                    for (label, percentage) in &label_to_percentage_map {
                        if rand::random::<f64>() * 100.0 < *percentage {
                            override_labels.insert(label.to_owned());
                        }
                    }
                    // TODO: handle the Err case here
                    tracing::info!("override_labels: {:?}", &override_labels);
                    let _ = request.context.insert(OVERRIDE_KEY, override_labels);
                    request
                })
                .service(service)
                .boxed()
        }
    }
}

register_plugin!("apollo", "progressive_override", ProgressiveOverridePlugin);

#[cfg(test)]
mod tests;
