use std::collections::HashMap;
use std::collections::HashSet;
use std::sync::Arc;

use apollo_compiler::schema::ExtendedType;
use apollo_compiler::Schema;
use dashmap::DashMap;
use schemars::JsonSchema;
use serde::Deserialize;
use tower::BoxError;
use tower::ServiceBuilder;
use tower::ServiceExt;

use self::layers::query_analysis::ParsedDocument;
use self::visitor::OverrideLabelVisitor;
use crate::plugin::Plugin;
use crate::plugin::PluginInit;
use crate::register_plugin;
use crate::services::*;
use crate::spec;
use crate::spec::query::traverse;

pub(crate) mod visitor;
pub(crate) const UNRESOLVED_LABELS: &str = "apollo_override::unresolved_labels";
pub(crate) const LABELS_TO_OVERRIDE: &str = "apollo_override::labels_to_override";

pub(crate) const JOIN_FIELD_DIRECTIVE_NAME: &str = "join__field";
pub(crate) const JOIN_SPEC_BASE_URL: &str = "https://specs.apollo.dev/join";
pub(crate) const JOIN_SPEC_VERSION: &str = "0.4";
pub(crate) const OVERRIDE_LABEL_ARG_NAME: &str = "overrideLabel";

/// Configuration for the progressive override plugin
#[derive(Debug, Default, Deserialize, JsonSchema)]
pub(crate) struct Config {}

pub(crate) struct ProgressiveOverridePlugin {
    enabled: bool,
    schema: Schema,
    labels_from_schema: LabelsFromSchema,
    // We have to visit each operation to find out which labels from the schema
    // are relevant for any given operation. This allows us to minimize the
    // number of labels we ultimately send to the query planner. Since these
    // labels are a component of the query plan cache key, it's important we
    // don't "overprovide" any labels, since doing so can explode the number of
    // cache entries per operation.
    labels_per_operation_cache: Arc<DashMap<ParsedDocument, Vec<Arc<String>>>>,
}

type LabelsFromSchema = (HashMap<Arc<String>, Arc<f64>>, HashSet<Arc<String>>);

fn collect_labels_from_schema(schema: &Schema) -> LabelsFromSchema {
    let Some(directive_name) = spec::Schema::directive_name(
        schema,
        JOIN_SPEC_BASE_URL,
        JOIN_SPEC_VERSION,
        JOIN_FIELD_DIRECTIVE_NAME,
    ) else {
        tracing::error!(
            "No join directive >=v0.4 found in the schema. No labels will be overridden."
        );
        return (HashMap::new(), HashSet::new());
    };

    let all_override_labels = schema
        .types
        .values()
        .filter_map(|extended_type| {
            if let ExtendedType::Object(object_type) = extended_type {
                Some(object_type)
            } else {
                None
            }
        })
        .flat_map(|object_type| &object_type.fields)
        .filter_map(|(_, field)| {
            let join_field_directives = field
                .directives
                .iter()
                .filter(|d| d.name.as_str() == directive_name)
                .collect::<Vec<_>>();
            if !join_field_directives.is_empty() {
                Some(join_field_directives)
            } else {
                None
            }
        })
        .flatten()
        .filter_map(|join_directive| {
            if let Some(override_label_arg) =
                join_directive.argument_by_name(OVERRIDE_LABEL_ARG_NAME)
            {
                override_label_arg
                    .as_str()
                    .map(|str| Arc::new(str.to_string()))
            } else {
                None
            }
        })
        .collect::<HashSet<_>>();

    let (percentages, other_labels): (HashSet<_>, HashSet<_>) = all_override_labels
        .into_iter()
        .partition(|label| label.starts_with("percent("));

    let static_percentages = percentages
        .into_iter()
        .filter_map(|unparsed_label| {
            unparsed_label
                .strip_prefix("percent(")
                .and_then(|unparsed_label| unparsed_label.strip_suffix(')'))
                .and_then(|percent_as_string| percent_as_string.parse::<f64>().ok())
                .map(|parsed_float| (Arc::new(unparsed_label.to_string()), Arc::new(parsed_float)))
        })
        .collect::<HashMap<_, _>>();

    tracing::debug!("static_percentages: {:?}", &static_percentages);
    (static_percentages, other_labels)
}

#[async_trait::async_trait]
impl Plugin for ProgressiveOverridePlugin {
    type Config = Config;

    async fn new(init: PluginInit<Self::Config>) -> Result<Self, BoxError> {
        let schema = Schema::parse(&*init.supergraph_sdl, "schema.graphql").expect(
            "i guess unwrap is safe here because otherwise plugin init shouldn't be called?",
        );
        let labels_from_schema = collect_labels_from_schema(&schema);
        let enabled = !labels_from_schema.0.is_empty() || !labels_from_schema.1.is_empty();
        Ok(ProgressiveOverridePlugin {
            enabled,
            schema,
            labels_from_schema,
            // we have to visit each operation to find out which labels from the schema are relevant.
            labels_per_operation_cache: Arc::new(
                // TODO: size config?
                DashMap::with_capacity(1000),
            ),
        })
    }

    // Add all arbitrary labels (non-percentage-based labels) from the schema to
    // the context so coprocessors can resolve their values
    fn router_service(&self, service: router::BoxService) -> router::BoxService {
        if !self.enabled {
            service
        } else {
            let (_, arbitrary_labels) = self.labels_from_schema.clone();
            ServiceBuilder::new()
                .map_request(move |request: router::Request| {
                    let _ = request
                        .context
                        .insert(UNRESOLVED_LABELS, arbitrary_labels.clone());
                    request
                })
                .service(service)
                .boxed()
        }
    }

    // Here we'll do a few things:
    // 1. "Roll the dice" for all of our percentage-based labels and collect the
    //    subset that will be enabled for this request
    // 2. Collect any externally-resolved labels from the context
    // 3. Filter the set of labels to only those that are relevant to the
    //    operation
    // 4. Add the filtered, sorted set of labels to the context for use by the
    //    query planner
    fn supergraph_service(&self, service: supergraph::BoxService) -> supergraph::BoxService {
        if !self.enabled {
            service
        } else {
            let (percentage_labels, _) = self.labels_from_schema.clone();
            let labels_per_operation_cache = self.labels_per_operation_cache.clone();

            let schema = self.schema.clone();
            ServiceBuilder::new()
            .map_request(move |request: supergraph::Request| {
                // evaluate each percentage-based label in the schema
                let percentage_override_labels =
                    percentage_labels.iter().filter(|(_, percentage)| rand::random::<f64>() * 100.0 < ***percentage).map(|(label, _) | label.clone());

                // collect any externally-resolved labels from the context
                let externally_overridden_labels = request
                    .context
                    .get::<_, Vec<Arc<String>>>(LABELS_TO_OVERRIDE)
                    .unwrap_or_default()
                    .unwrap_or_default();

                if let Some(parsed_doc) = request.context.extensions().lock().get::<ParsedDocument>() {
                    // we have to visit the operation to find out which subset
                    // of labels are relevant unless we've already cached that
                    // work
                    let relevant_labels = labels_per_operation_cache
                        .entry(Arc::clone(parsed_doc))
                        .or_insert_with(|| {
                            OverrideLabelVisitor::new(&schema)
                                .map(|mut visitor| {
                                    let _ = traverse::document(&mut visitor, &parsed_doc.ast);
                                    visitor.override_labels.into_iter().collect::<Vec<_>>()
                                })
                                .unwrap_or_default()
                        })
                        .clone();

                    // the intersection of all provided labels (percentage and
                    // external) and the labels relevant to this operation is
                    // the set of labels we'll send to the query planner
                    let mut overridden_labels_for_operation = percentage_override_labels
                        .chain(externally_overridden_labels)
                        .filter(|l| relevant_labels.contains(l))
                        .collect::<Vec<_>>();
                    overridden_labels_for_operation.sort();
                    // note: this only dedupes as expected since the vec is
                    // sorted immediately before
                    overridden_labels_for_operation.dedup();

                    tracing::debug!("ProgressiveOverridePlugin: overridden labels: {:?}", &overridden_labels_for_operation);

                    let _ = request
                        .context
                        .insert(LABELS_TO_OVERRIDE, overridden_labels_for_operation);

                } else {
                    tracing::error!("No parsed document found in the context. All override labels will be ignored.");
                }

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
