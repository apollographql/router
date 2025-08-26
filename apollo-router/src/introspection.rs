use std::num::NonZeroUsize;
use std::ops::ControlFlow;
use std::sync::Arc;

use apollo_compiler::executable::Selection;
use serde_json_bytes::json;

use crate::Configuration;
use crate::cache::storage::CacheStorage;
use crate::compute_job;
use crate::compute_job::ComputeBackPressureError;
use crate::compute_job::ComputeJobType;
use crate::graphql;
use crate::query_planner::QueryKey;
use crate::services::layers::query_analysis::ParsedDocument;
use crate::spec;

const DEFAULT_INTROSPECTION_CACHE_CAPACITY: NonZeroUsize = NonZeroUsize::new(5).unwrap();

#[derive(Clone)]
pub(crate) struct IntrospectionCache(Mode);

#[derive(Clone)]
enum Mode {
    Disabled,
    Enabled {
        storage: Arc<CacheStorage<String, graphql::Response>>,
        max_depth: MaxDepth,
    },
}

#[derive(Copy, Clone)]
enum MaxDepth {
    Check,
    Ignore,
}

impl IntrospectionCache {
    pub(crate) fn new(configuration: &Configuration) -> Self {
        if configuration.supergraph.introspection {
            let storage = Arc::new(CacheStorage::new_in_memory(
                DEFAULT_INTROSPECTION_CACHE_CAPACITY,
                "introspection",
            ));
            storage.activate();
            Self(Mode::Enabled {
                storage,
                max_depth: if configuration.limits.introspection_max_depth {
                    MaxDepth::Check
                } else {
                    MaxDepth::Ignore
                },
            })
        } else {
            Self(Mode::Disabled)
        }
    }

    pub(crate) fn activate(&self) {
        match &self.0 {
            Mode::Disabled => {}
            Mode::Enabled { storage, .. } => storage.activate(),
        }
    }

    /// If `request` is a query with only introspection fields,
    /// execute it and return a (cached) response
    pub(crate) async fn maybe_execute(
        &self,
        schema: &Arc<spec::Schema>,
        key: &QueryKey,
        doc: &ParsedDocument,
    ) -> ControlFlow<Result<graphql::Response, ComputeBackPressureError>, ()> {
        Self::maybe_lone_root_typename(schema, doc)?;
        if doc.operation.is_query() {
            if doc.has_schema_introspection {
                if doc.has_explicit_root_fields {
                    ControlFlow::Break(Ok(Self::mixed_fields_error()))?;
                } else {
                    ControlFlow::Break(self.cached_introspection(schema, key, doc).await)?
                }
            } else if !doc.has_explicit_root_fields {
                // Root __typename only

                // No list field so depth is already known to be zero:
                let max_depth = MaxDepth::Ignore;

                // Probably a small query, execute it without caching:
                ControlFlow::Break(Ok(Self::execute_introspection(max_depth, schema, doc)))?
            }
        }
        ControlFlow::Continue(())
    }

    /// A `{ __typename }` query is often used as a ping or health check.
    /// Handle it without touching the cache.
    ///
    /// This fast path only applies if no fragment or directive is used,
    /// so that we don’t have to deal with `@skip` or `@include` here.
    fn maybe_lone_root_typename(
        schema: &Arc<spec::Schema>,
        doc: &ParsedDocument,
    ) -> ControlFlow<Result<graphql::Response, ComputeBackPressureError>, ()> {
        if doc.operation.selection_set.selections.len() == 1
            && let Selection::Field(field) = &doc.operation.selection_set.selections[0]
            && field.name == "__typename"
            && field.directives.is_empty()
        {
            // `{ alias: __typename }` is much less common so handling it here is not essential
            // but easier than a conditional to reject it
            let key = field.response_key().as_str();
            let object_type_name = schema
                .api_schema()
                .root_operation(doc.operation.operation_type)
                .expect("validation should have caught undefined root operation")
                .as_str();
            let data = json!({key: object_type_name});
            ControlFlow::Break(Ok(graphql::Response::builder().data(data).build()))?
        }
        ControlFlow::Continue(())
    }

    fn mixed_fields_error() -> graphql::Response {
        let error = graphql::Error::builder()
            .message(
                "\
                Mixed queries with both schema introspection and concrete fields \
                are not supported yet: https://github.com/apollographql/router/issues/2789\
            ",
            )
            .extension_code("MIXED_INTROSPECTION")
            .build();
        graphql::Response::builder().error(error).build()
    }

    async fn cached_introspection(
        &self,
        schema: &Arc<spec::Schema>,
        key: &QueryKey,
        doc: &ParsedDocument,
    ) -> Result<graphql::Response, ComputeBackPressureError> {
        let (storage, max_depth) = match &self.0 {
            Mode::Enabled { storage, max_depth } => (storage, *max_depth),
            Mode::Disabled => {
                let error = graphql::Error::builder()
                    .message(String::from("introspection has been disabled"))
                    .extension_code("INTROSPECTION_DISABLED")
                    .build();
                return Ok(graphql::Response::builder().error(error).build());
            }
        };
        let query = key.filtered_query.clone();
        // TODO:  when adding support for variables in introspection queries,
        // variable values should become part of the cache key.
        // https://github.com/apollographql/router/issues/3831
        let cache_key = query;
        if let Some(response) = storage.get(&cache_key, |_| unreachable!()).await {
            return Ok(response);
        }
        let schema = schema.clone();
        let doc = doc.clone();
        let response = compute_job::execute(ComputeJobType::Introspection, move |_| {
            Self::execute_introspection(max_depth, &schema, &doc)
        })?
        // `expect()` propagates any panic that potentially happens in the closure, but:
        //
        // * We try to avoid such panics in the first place and consider them bugs
        // * The panic handler in `apollo-router/src/executable.rs` exits the process
        //   so this error case should never be reached.
        .await;
        storage.insert(cache_key, response.clone()).await;
        Ok(response)
    }

    fn execute_introspection(
        max_depth: MaxDepth,
        schema: &spec::Schema,
        doc: &ParsedDocument,
    ) -> graphql::Response {
        let api_schema = schema.api_schema();
        let operation = &doc.operation;
        let variable_values = Default::default();
        let max_depth_result = match max_depth {
            MaxDepth::Check => {
                apollo_compiler::introspection::check_max_depth(&doc.executable, operation)
            }
            MaxDepth::Ignore => Ok(()),
        };
        let result = max_depth_result
            .and_then(|()| {
                apollo_compiler::request::coerce_variable_values(
                    api_schema,
                    operation,
                    &variable_values,
                )
            })
            .and_then(|variable_values| {
                apollo_compiler::introspection::partial_execute(
                    api_schema,
                    &schema.implementers_map,
                    &doc.executable,
                    operation,
                    &variable_values,
                )
            });
        match result {
            Ok(response) => response.into(),
            Err(e) => {
                let error = e.to_graphql_error(&doc.executable.sources);
                graphql::Response::builder().error(error).build()
            }
        }
    }
}
