use std::num::NonZeroUsize;
use std::ops::ControlFlow;
use std::sync::Arc;

use apollo_compiler::executable::Selection;
use serde_json_bytes::json;

use crate::cache::storage::CacheStorage;
use crate::compute_job;
use crate::compute_job::ComputeBackPressureError;
use crate::graphql;
use crate::query_planner::QueryKey;
use crate::services::layers::query_analysis::ParsedDocument;
use crate::spec;
use crate::Configuration;

const DEFAULT_INTROSPECTION_CACHE_CAPACITY: NonZeroUsize =
    unsafe { NonZeroUsize::new_unchecked(5) };

#[derive(Clone)]
pub(crate) enum IntrospectionCache {
    Disabled,
    Enabled {
        storage: Arc<CacheStorage<String, graphql::Response>>,
    },
}

impl IntrospectionCache {
    pub(crate) fn new(configuration: &Configuration) -> Self {
        if configuration.supergraph.introspection {
            let storage = Arc::new(CacheStorage::new_in_memory(
                DEFAULT_INTROSPECTION_CACHE_CAPACITY,
                "introspection",
            ));
            storage.activate();
            Self::Enabled { storage }
        } else {
            Self::Disabled
        }
    }

    pub(crate) fn activate(&self) {
        match self {
            IntrospectionCache::Disabled => {}
            IntrospectionCache::Enabled { storage } => storage.activate(),
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
                // root __typename only, probably a small query
                // Execute it without caching:
                ControlFlow::Break(Ok(Self::execute_introspection(schema, doc)))?
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
        if doc.operation.selection_set.selections.len() == 1 {
            if let Selection::Field(field) = &doc.operation.selection_set.selections[0] {
                if field.name == "__typename" && field.directives.is_empty() {
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
            }
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
        let storage = match self {
            IntrospectionCache::Enabled { storage } => storage,
            IntrospectionCache::Disabled => {
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
        let priority = compute_job::Priority::P1; // Low priority
        let response =
            compute_job::execute(priority, move || Self::execute_introspection(&schema, &doc))?
                .await
                .expect("Introspection panicked");
        storage.insert(cache_key, response.clone()).await;
        Ok(response)
    }

    fn execute_introspection(schema: &spec::Schema, doc: &ParsedDocument) -> graphql::Response {
        let api_schema = schema.api_schema();
        let operation = &doc.operation;
        let variable_values = Default::default();
        match apollo_compiler::request::coerce_variable_values(
            api_schema,
            operation,
            &variable_values,
        )
        .and_then(|variable_values| {
            apollo_compiler::introspection::partial_execute(
                api_schema,
                &schema.implementers_map,
                &doc.executable,
                operation,
                &variable_values,
            )
        }) {
            Ok(response) => response.into(),
            Err(e) => {
                let error = e.to_graphql_error(&doc.executable.sources);
                graphql::Response::builder().error(error).build()
            }
        }
    }
}
