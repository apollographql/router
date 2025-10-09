use std::collections::HashMap;
use std::collections::HashSet;
use std::iter::Extend;
use std::sync::Arc;

use apollo_compiler::Schema;
use apollo_compiler::collections::IndexMap;
use apollo_compiler::validation::Valid;
use apollo_federation::connectors::StringTemplate;
use serde_json_bytes::Value;
use tower::BoxError;

use crate::error::FetchError;
use crate::plugins::mock_subgraphs::execution::input_coercion::coerce_argument_values;
use crate::plugins::response_cache::invalidation::Invalidation;
use crate::plugins::response_cache::invalidation::InvalidationRequest;
use crate::services::subgraph;

const CACHE_INVALIDATION_DIRECTIVE_NAME: &str = "federation__cacheInvalidation";

pub(crate) fn get_invalidations_from_mutation(
    request: &subgraph::Request,
    subgraph_enums: &HashMap<String, String>,
    supergraph_schema: Arc<Valid<Schema>>,
) -> Result<HashSet<Invalidations>, anyhow::Error> {
    let subgraph_name = &request.subgraph_name;
    let executable_document =
        request
            .executable_document
            .as_ref()
            .ok_or_else(|| FetchError::MalformedRequest {
                reason: "cannot get the executable document for subgraph request".to_string(),
            })?;
    let root_operation_fields = executable_document
        .operations
        .get(request.subgraph_request.body().operation_name.as_deref())
        .map_err(|_err| FetchError::MalformedRequest {
            reason: "cannot get the operation from executable document for subgraph request"
                .to_string(),
        })?
        .root_fields(executable_document);
    let root_mutation_type = supergraph_schema
        .root_operation(apollo_compiler::ast::OperationType::Mutation)
        .ok_or_else(|| FetchError::MalformedRequest {
            reason: "cannot get the root operation from supergraph schema".to_string(),
        })?;
    let mutation_object_type = supergraph_schema
        .get_object(root_mutation_type.as_str())
        .ok_or_else(|| FetchError::MalformedRequest {
            reason: "cannot get the root query type from supergraph schema".to_string(),
        })?;

    let invalidations = root_operation_fields
        .map(|field| {
            // We don't use field.definition because we need the directive set in supergraph schema not in the executable document
            let field_def = mutation_object_type
                .fields
                .get(&field.name)
                .ok_or_else(|| FetchError::MalformedRequest {
                    reason: "cannot get the field definition from supergraph schema".to_string(),
                })?;

            let invalidations = field_def
                .directives
                .get_all("join__directive")
                .filter_map(|dir| {
                    let name = dir.argument_by_name("name", &supergraph_schema).ok()?;
                    if name.as_str()? != CACHE_INVALIDATION_DIRECTIVE_NAME {
                        return None;
                    }
                    let is_current_subgraph =
                        dir.argument_by_name("graphs", &supergraph_schema)
                            .ok()
                            .and_then(|f| {
                                Some(f.as_list()?.iter().filter_map(|graph| graph.as_enum()).any(
                                    |g| {
                                        subgraph_enums.get(g.as_str()).map(|s| s.as_str())
                                            == Some(subgraph_name)
                                    },
                                ))
                            })
                            .unwrap_or_default();
                    if !is_current_subgraph {
                        return None;
                    }
                    let mut cache_tag = None;
                    let mut entity_type = None;
                    for (field_name, value) in dir
                        .argument_by_name("args", &supergraph_schema)
                        .ok()?
                        .as_object()?
                    {
                        if field_name.as_str() == "type" {
                            entity_type = value
                                .as_str()
                                .and_then(|v| v.parse::<StringTemplate>().ok())
                        } else if field_name.as_str() == "cacheTag" {
                            cache_tag = value
                                .as_str()
                                .and_then(|v| v.parse::<StringTemplate>().ok())
                        }
                    }
                    if cache_tag.is_none() && entity_type.is_none() {
                        None
                    } else {
                        Some((entity_type, cache_tag))
                    }
                });
            let mut errors = Vec::new();
            // Query::validate_variables runs before this
            let variable_values =
                Valid::assume_valid_ref(&request.subgraph_request.body().variables);
            let args = coerce_argument_values(
                &supergraph_schema,
                executable_document,
                variable_values,
                &mut errors,
                Default::default(),
                field_def,
                field,
            )
            .map_err(|_| FetchError::MalformedRequest {
                reason: format!("cannot argument values for root fields {:?}", field.name),
            })?;

            if !errors.is_empty() {
                return Err(FetchError::MalformedRequest {
                    reason: format!(
                        "cannot coerce argument values for root fields {:?}, errors: {errors:?}",
                        field.name,
                    ),
                }
                .into());
            }

            let mut vars = IndexMap::default();
            vars.insert("$args".to_string(), Value::Object(args));
            let (entity_type_invalidations, cache_tag_invalidations): (Vec<_>, Vec<_>) =
                invalidations.unzip();
            let entity_types = entity_type_invalidations
                .into_iter()
                .flatten()
                .map(|entity_type| Ok(entity_type.interpolate(&vars).map(|(res, _)| res)?))
                .collect::<Result<Vec<String>, anyhow::Error>>()?;
            let cache_tags = cache_tag_invalidations
                .into_iter()
                .flatten()
                .map(|cache_tag| Ok(cache_tag.interpolate(&vars).map(|(res, _)| res)?))
                .collect::<Result<Vec<String>, anyhow::Error>>()?;

            Ok(Invalidations {
                cache_tags,
                entity_types,
                subgraph_name: subgraph_name.clone(),
            })
        })
        .collect::<Result<HashSet<Invalidations>, anyhow::Error>>()?;

    Ok(invalidations)
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct Invalidations {
    cache_tags: Vec<String>,
    entity_types: Vec<String>,
    subgraph_name: String,
}

pub(crate) async fn automatic_invalidation(
    invalidation: Invalidation,
    invalidations_to_execute: HashSet<Invalidations>,
) -> Result<u64, BoxError> {
    // Call invalidations
    let invalidation_reqs = invalidations_to_execute.into_iter().flat_map(|inv| {
        let mut invalidation_reqs: Vec<_> = inv
            .cache_tags
            .into_iter()
            .map(|ct| InvalidationRequest::CacheTag {
                subgraphs: vec![inv.subgraph_name.clone()].into_iter().collect(),
                cache_tag: ct,
            })
            .collect();
        invalidation_reqs.extend(inv.entity_types.into_iter().map(|et| {
            InvalidationRequest::Type {
                subgraph: inv.subgraph_name.clone(),
                r#type: et,
            }
        }));

        invalidation_reqs
    });

    let count = invalidation.invalidate(invalidation_reqs.collect()).await?;

    Ok(count)
}
