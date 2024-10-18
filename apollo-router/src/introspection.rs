use std::num::NonZeroUsize;
use std::ops::ControlFlow;
use std::sync::Arc;

use apollo_compiler::executable::Selection;
use serde_json_bytes::json;

use crate::cache::storage::CacheStorage;
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
    ) -> ControlFlow<graphql::Response, ()> {
        Self::maybe_lone_root_typename(schema, doc)?;
        if doc.operation.is_query() {
            if doc.has_explicit_root_fields && doc.has_schema_introspection {
                ControlFlow::Break(Self::mixed_fields_error())?;
            } else if !doc.has_explicit_root_fields {
                ControlFlow::Break(self.cached_introspection(schema, key, doc).await)?
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
    ) -> ControlFlow<graphql::Response, ()> {
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
                    ControlFlow::Break(graphql::Response::builder().data(data).build())?
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
    ) -> graphql::Response {
        let storage = match self {
            IntrospectionCache::Enabled { storage } => storage,
            IntrospectionCache::Disabled => {
                let error = graphql::Error::builder()
                    .message(String::from("introspection has been disabled"))
                    .extension_code("INTROSPECTION_DISABLED")
                    .build();
                return graphql::Response::builder().error(error).build();
            }
        };
        let query = key.filtered_query.clone();
        // TODO:  when adding support for variables in introspection queries,
        // variable values should become part of the cache key.
        // https://github.com/apollographql/router/issues/3831
        let cache_key = query;
        if let Some(response) = storage.get(&cache_key, |_| unreachable!()).await {
            return response;
        }
        let schema = schema.clone();
        let doc = doc.clone();
        let response =
            tokio::task::spawn_blocking(move || Self::execute_introspection(&schema, &doc))
                .await
                .expect("Introspection panicked");
        storage.insert(cache_key, response.clone()).await;
        response
    }

    fn execute_introspection(schema: &spec::Schema, doc: &ParsedDocument) -> graphql::Response {
        let schema = schema.api_schema();
        let operation = &doc.operation;
        let variable_values = Default::default();
        match apollo_compiler::execution::coerce_variable_values(
            schema,
            operation,
            &variable_values,
        ) {
            Ok(variable_values) => apollo_compiler::execution::execute_introspection_only_query(
                schema,
                &doc.executable,
                operation,
                &variable_values,
            )
            .into(),
            Err(e) => {
                let error = e.into_graphql_error(&doc.executable.sources);
                graphql::Response::builder().error(error).build()
            }
        }
    }
}
