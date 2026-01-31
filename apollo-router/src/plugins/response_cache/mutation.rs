use std::cell::RefCell;
use std::collections::HashMap;
use std::collections::HashSet;
use std::iter::Extend;
use std::sync::Arc;

use apollo_compiler::Schema;
use apollo_compiler::collections::IndexMap;
use apollo_compiler::resolvers;
use apollo_compiler::schema::ObjectType;
use apollo_compiler::validation::Valid;
use apollo_federation::connectors::StringTemplate;
use itertools::Either;
use itertools::Itertools;
use itertools::multiunzip;
use serde_json_bytes::Value;
use tower::BoxError;

use crate::error::FetchError;
use crate::plugins::response_cache::invalidation::Invalidation;
use crate::plugins::response_cache::invalidation::InvalidationRequest;
use crate::services::subgraph;

const CACHE_INVALIDATION_DIRECTIVE_NAME: &str = "federation__cacheInvalidation";

pub(crate) fn get_invalidations_from_mutation(
    request: &subgraph::Request,
    subgraph_enums: &HashMap<String, String>,
    supergraph_schema: Arc<Valid<Schema>>,
) -> Result<(Invalidations, Invalidations), anyhow::Error> {
    struct Root<'a> {
        subgraph_name: &'a str,
        subgraph_enums: &'a HashMap<String, String>,
        mutation_object_type: &'a ObjectType,
        result: RefCell<Result<(Invalidations, Invalidations), anyhow::Error>>,
    }

    impl resolvers::ObjectValue for Root<'_> {
        fn type_name(&self) -> &str {
            "Mutation"
        }

        fn resolve_field<'a>(
            &'a self,
            info: &'a resolvers::ResolveInfo<'a>,
        ) -> Result<resolvers::ResolvedValue<'a>, resolvers::FieldError> {
            let mut result = self.result.borrow_mut();
            let Ok((async_invalidation_keys, sync_invalidation_keys)) = &mut *result else {
                return Ok(resolvers::ResolvedValue::SkipForPartialExecution);
            };
            // We don't use info.field_definition() because we need the directive
            // set in supergraph schema not in the executable document
            let Some(field_def) = self.mutation_object_type.fields.get(info.field_name()) else {
                *result = Err(FetchError::MalformedRequest {
                    reason: "cannot get the field definition from supergraph schema".to_string(),
                }
                .into());
                return Ok(resolvers::ResolvedValue::SkipForPartialExecution);
            };

            let invalidations = field_def
                .directives
                .get_all("join__directive")
                .filter_map(|dir| {
                    let name = dir.argument_by_name("name", info.schema()).ok()?;
                    if name.as_str()? != CACHE_INVALIDATION_DIRECTIVE_NAME {
                        return None;
                    }
                    let is_current_subgraph =
                        dir.argument_by_name("graphs", info.schema())
                            .ok()
                            .and_then(|f| {
                                Some(f.as_list()?.iter().filter_map(|graph| graph.as_enum()).any(
                                    |g| {
                                        self.subgraph_enums.get(g.as_str()).map(|s| s.as_str())
                                            == Some(self.subgraph_name)
                                    },
                                ))
                            })
                            .unwrap_or_default();
                    if !is_current_subgraph {
                        return None;
                    }
                    let mut cache_tag = None;
                    let mut entity_type = None;
                    let mut is_async = false;
                    for (field_name, value) in dir
                        .argument_by_name("args", info.schema())
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

                        if field_name.as_str() == "async" {
                            is_async = value.to_bool().unwrap_or_default();
                        }
                    }
                    if cache_tag.is_none() && entity_type.is_none() {
                        None
                    } else {
                        Some((entity_type, cache_tag, is_async))
                    }
                });
            let mut vars = IndexMap::default();
            vars.insert("$args".to_string(), Value::Object(info.arguments().clone()));
            let (entity_type_invalidations, cache_tag_invalidations, is_asyncs): (
                Vec<_>,
                Vec<_>,
                Vec<bool>,
            ) = multiunzip(invalidations);

            match entity_type_invalidations
                .into_iter()
                .flatten()
                .zip(is_asyncs.clone())
                .map(|(entity_type, is_async)| {
                    Ok((
                        entity_type.interpolate(&vars).map(|(res, _)| res)?,
                        is_async,
                    ))
                })
                .collect::<Result<HashSet<(String, bool)>, anyhow::Error>>()
            {
                Ok(entity_types) => {
                    let (async_entity_types, sync_entity_types): (Vec<String>, Vec<String>) =
                        entity_types
                            .into_iter()
                            .partition_map(|(entity_type, is_async)| {
                                if is_async {
                                    Either::Left(entity_type)
                                } else {
                                    Either::Right(entity_type)
                                }
                            });

                    async_invalidation_keys.entity_types = async_entity_types.into_iter().collect();
                    sync_invalidation_keys.entity_types = sync_entity_types.into_iter().collect();
                }
                Err(err) => {
                    *result = Err(err);
                    return Ok(resolvers::ResolvedValue::SkipForPartialExecution);
                }
            }
            match cache_tag_invalidations
                .into_iter()
                .flatten()
                .zip(is_asyncs)
                .map(|(cache_tag, is_async)| {
                    Ok((cache_tag.interpolate(&vars).map(|(res, _)| res)?, is_async))
                })
                .collect::<Result<HashSet<(String, bool)>, anyhow::Error>>()
            {
                Ok(cache_tags) => {
                    let (async_cache_tags, sync_cache_tags): (Vec<String>, Vec<String>) =
                        cache_tags
                            .into_iter()
                            .partition_map(|(cache_tag, is_async)| {
                                if is_async {
                                    Either::Left(cache_tag)
                                } else {
                                    Either::Right(cache_tag)
                                }
                            });

                    async_invalidation_keys.cache_tags = async_cache_tags.into_iter().collect();
                    sync_invalidation_keys.cache_tags = sync_cache_tags.into_iter().collect();
                }
                Err(err) => {
                    *result = Err(err);
                    return Ok(resolvers::ResolvedValue::SkipForPartialExecution);
                }
            }

            Ok(resolvers::ResolvedValue::SkipForPartialExecution)
        }
    }
    let executable_document =
        request
            .executable_document
            .as_ref()
            .ok_or_else(|| FetchError::MalformedRequest {
                reason: "cannot get the executable document for subgraph request".to_string(),
            })?;
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
    let root = Root {
        subgraph_name: &request.subgraph_name,
        subgraph_enums,
        mutation_object_type,
        result: RefCell::new(Ok((
            Invalidations {
                subgraph_name: request.subgraph_name.to_string(),
                ..Default::default()
            },
            Invalidations {
                subgraph_name: request.subgraph_name.to_string(),
                ..Default::default()
            },
        ))),
    };
    let subgraph_request = request.subgraph_request.body();
    // FIXME: in principle we should use the subgraph schema here.
    // Maybe this is good enough as far as finding root fields is concerned?
    resolvers::Execution::new(&supergraph_schema, executable_document)
        .operation_name(subgraph_request.operation_name.as_deref())
        .unwrap()
        .raw_variable_values(&subgraph_request.variables)
        .execute_sync(&root)
        .map_err(|e| anyhow::Error::msg(e.message().to_string()))?;

    root.result.into_inner()
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct Invalidations {
    cache_tags: HashSet<String>,
    entity_types: HashSet<String>,
    subgraph_name: String,
}

impl Invalidations {
    pub(crate) fn is_empty(&self) -> bool {
        self.cache_tags.is_empty() && self.entity_types.is_empty()
    }
}

pub(crate) async fn automatic_invalidation(
    invalidation: Invalidation,
    invalidations_to_execute: Invalidations,
) -> Result<u64, BoxError> {
    // Call invalidations
    let mut invalidation_reqs: Vec<_> = invalidations_to_execute
        .cache_tags
        .into_iter()
        .map(|ct| InvalidationRequest::CacheTag {
            subgraphs: vec![invalidations_to_execute.subgraph_name.clone()]
                .into_iter()
                .collect(),
            cache_tag: ct,
        })
        .collect();
    invalidation_reqs.extend(invalidations_to_execute.entity_types.into_iter().map(|et| {
        InvalidationRequest::Type {
            subgraph: invalidations_to_execute.subgraph_name.clone(),
            r#type: et,
        }
    }));

    let count = invalidation.invalidate(invalidation_reqs).await?;

    Ok(count)
}
