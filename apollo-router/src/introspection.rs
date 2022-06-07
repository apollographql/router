use crate::prelude::graphql::*;
use include_dir::include_dir;
use once_cell::sync::Lazy;
use router_bridge::introspect::{self, IntrospectionError};
use std::collections::HashMap;
use tokio::sync::RwLock;

/// KNOWN_INTROSPECTION_QUERIES we will serve through NaiveIntrospection.
///
/// If you would like to add one, put it in the "well_known_introspection_queries" folder.
static KNOWN_INTROSPECTION_QUERIES: Lazy<Vec<String>> = Lazy::new(|| {
    include_dir!("$CARGO_MANIFEST_DIR/well_known_introspection_queries")
        .files()
        .map(|file| {
            file.contents_utf8()
                .unwrap_or_else(|| {
                    panic!(
                        "contents of the file at path {} isn't valid utf8",
                        file.path().display()
                    );
                })
                .to_string()
        })
        .collect()
});

/// A cache containing our well known introspection queries.
#[derive(Debug)]
pub struct Introspection {
    cache: RwLock<HashMap<String, Response>>,
}

impl Introspection {
    #[cfg(test)]
    pub fn from_cache(cache: HashMap<String, Response>) -> Self {
        Self {
            cache: RwLock::new(cache),
        }
    }

    /// Create a `NaiveIntrospection` from a `Schema`.
    ///
    /// This function will populate a cache in a blocking manner.
    /// This is why `NaiveIntrospection` instanciation happens in a spawn_blocking task on the state_machine side.
    pub fn from_schema(schema: &Schema) -> Self {
        let span = tracing::trace_span!("naive_introspection_population");
        let _guard = span.enter();

        let cache = introspect::batch_introspect(
            schema.as_str(),
            KNOWN_INTROSPECTION_QUERIES.iter().cloned().collect(),
        )
        .map_err(|deno_runtime_error| {
            tracing::warn!(
                "router-bridge returned a deno runtime error:\n{}",
                deno_runtime_error
            );
        })
        .and_then(|global_introspection_result| {
            global_introspection_result
                .map_err(|general_introspection_error| {
                    tracing::warn!(
                        "Introspection returned an error:\n{}",
                        general_introspection_error
                    );
                })
                .map(|responses| {
                    KNOWN_INTROSPECTION_QUERIES
                        .iter()
                        .zip(responses)
                        .filter_map(|(query, response)| match response.into_result() {
                            Ok(value) => {
                                let response = Response::builder().data(value).build();
                                Some((query.into(), response))
                            }
                            Err(graphql_errors) => {
                                for error in graphql_errors {
                                    tracing::warn!(
                                        "Introspection returned error:\n{}\n{}",
                                        error,
                                        query
                                    );
                                }
                                None
                            }
                        })
                        .collect()
                })
        })
        .unwrap_or_default();

        Self {
            cache: RwLock::new(cache),
        }
    }

    /// Get a cached response for a query.
    pub async fn get(&self, query: &str) -> Option<Response> {
        self.cache
            .read()
            .await
            .get(query)
            .map(std::clone::Clone::clone)
    }

    /// Execute an introspection and cache the response.
    pub async fn execute(
        &self,
        schema_sdl: &str,
        query: &str,
    ) -> Result<Response, IntrospectionError> {
        // Do the introspection query and cache it
        let mut response = introspect::batch_introspect(schema_sdl, vec![query.to_owned()])
            .map_err(|err| IntrospectionError {
                message: format!("Deno runtime error: {:?}", err).into(),
            })??;
        let introspection_result = response
            .pop()
            .ok_or_else(|| IntrospectionError {
                message: String::from("cannot find the introspection response").into(),
            })?
            .into_result()
            .map_err(|err| IntrospectionError {
                message: format!(
                    "introspection error : {}",
                    err.into_iter()
                        .map(|err| err.to_string())
                        .collect::<Vec<String>>()
                        .join(", "),
                )
                .into(),
            })?;
        let response = Response::builder().data(introspection_result).build();

        self.cache
            .write()
            .await
            .insert(query.to_string(), response.clone());

        Ok(response)
    }
}

#[cfg(test)]
mod naive_introspection_tests {
    use super::*;

    #[tokio::test]
    async fn test_plan() {
        let query_to_test = "this is a test query";
        let expected_data = Response::builder()
            .data(serde_json::Value::Number(42.into()))
            .build();

        let cache = [(query_to_test.into(), expected_data.clone())]
            .iter()
            .cloned()
            .collect();
        let naive_introspection = Introspection::from_cache(cache);

        assert_eq!(
            expected_data,
            naive_introspection.get(query_to_test).await.unwrap()
        );
    }

    #[test]
    fn test_known_introspection_queries() {
        // this only makes sure KNOWN_INTROSPECTION_QUERIES get created correctly.
        // thus preventing regressions if a wrong query is added
        // to the `well_known_introspection_queries` folder
        let _ = &*KNOWN_INTROSPECTION_QUERIES;
    }
}
