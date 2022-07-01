use std::collections::HashMap;

use include_dir::include_dir;
use once_cell::sync::Lazy;
use router_bridge::introspect::IntrospectionError;
use router_bridge::introspect::{self};

use crate::graphql::Response;
use crate::*;

/// KNOWN_INTROSPECTION_QUERIES we will serve through Introspection.
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
pub(crate) struct Introspection {
    cache: HashMap<String, Response>,
}

impl Introspection {
    #[cfg(test)]
    pub(crate) fn from_cache(cache: HashMap<String, Response>) -> Self {
        Self { cache }
    }

    /// Create a `Introspection` from a `Schema`.
    ///
    /// This function will populate a cache in a blocking manner.
    /// This is why `Introspection` instanciation happens in a spawn_blocking task on the state_machine side.
    pub(crate) fn from_schema(schema: &Schema) -> Self {
        let span = tracing::trace_span!("introspection_population");
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

        Self { cache }
    }

    /// Execute an introspection and cache the response.
    pub(crate) async fn execute(
        &self,
        schema_sdl: &str,
        query: &str,
    ) -> Result<Response, IntrospectionError> {
        if let Some(response) = self.cache.get(query) {
            return Ok(response.clone());
        }

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

        Ok(response)
    }
}

#[cfg(test)]
mod introspection_tests {
    use super::*;

    #[tokio::test]
    async fn test_plan() {
        let query_to_test = "this is a test query";
        let schema = " ";
        let expected_data = Response::builder().data(42).build();

        let cache = [(query_to_test.into(), expected_data.clone())]
            .iter()
            .cloned()
            .collect();
        let introspection = Introspection::from_cache(cache);

        assert_eq!(
            expected_data,
            introspection.execute(schema, query_to_test).await.unwrap()
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
