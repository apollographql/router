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


const OVERRIDE_KEY: &str = "apollo_override::overridden_labels";

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub(crate) struct Conf {
    enabled: bool,
}

pub(crate) struct ProgressiveOverridePlugin {
    label_to_percentages_map: HashMap<String, f64>,
}

fn collect_static_percentages_from_schema(schema: Schema) -> HashMap<String, f64> {
    let mut static_percentages = HashMap::new();
    for extended_type in schema.types.values() {
        if let ExtendedType::Object(object_type) = extended_type {
            for field in object_type.fields.values() {
                if let Some(label_arg) = field
                    .directives
                    .iter()
                    .find(|d| d.name == "override")
                    .and_then(|d| d.arguments.iter().find(|a| a.name == "label"))
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
    static_percentages
}

#[async_trait::async_trait]
impl Plugin for ProgressiveOverridePlugin {
    type Config = Conf;

    async fn new(init: PluginInit<Self::Config>) -> Result<Self, BoxError> {
        Ok(ProgressiveOverridePlugin {
            label_to_percentages_map: collect_static_percentages_from_schema(Schema::parse(
                &*init.supergraph_sdl,
                "schema.graphql",
            )),
        })
    }

    fn router_service(&self, service: router::BoxService) -> router::BoxService {
        service
    }

    fn supergraph_service(&self, service: supergraph::BoxService) -> supergraph::BoxService {
        service
    }

    fn execution_service(&self, service: execution::BoxService) -> execution::BoxService {
        let label_to_percentages_map = self.label_to_percentages_map.clone();
        ServiceBuilder::new()
            .map_request(move |request: execution::Request| {
                let mut overridden_labels = HashSet::new();
                label_to_percentages_map.iter().for_each(|(label, percentage)| {
                    if rand::random::<f64>() < *percentage {
                        overridden_labels.insert(label.to_owned());
                    }
                });
                // TODO: handle the Err case here
                let _ = request.context.insert(OVERRIDE_KEY, overridden_labels);
                request
            })
            .service(service)
            .boxed()
    }
}

register_plugin!("apollo", "progressive_override", ProgressiveOverridePlugin);
