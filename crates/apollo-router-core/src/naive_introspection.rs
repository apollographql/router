use crate::prelude::graphql::*;
use include_dir::include_dir;
use once_cell::sync::Lazy;
use router_bridge::introspect;
use std::collections::HashMap;

/// KNOWN_INTROSPECTION_QUERIES we will serve through NaiveIntrospection.
///
/// If you would like to add one, put it in the "well_known_introspection_queries" folder.
static KNOWN_INTROSPECTION_QUERIES: Lazy<Vec<String>> = Lazy::new(|| {
    include_dir!("./well_known_introspection_queries")
        .files()
        .iter()
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
pub struct NaiveIntrospection {
    cache: HashMap<Query, serde_json::Value>,
}

impl NaiveIntrospection {
    #[cfg(test)]
    pub fn from_cache(cache: HashMap<Query, serde_json::Value>) -> Self {
        Self { cache }
    }

    /// Create a `NaiveIntrospection` from a `Schema`.
    ///
    /// This function will populate a cache in a blocking manner.
    /// This is why `NaiveIntrospection` instanciation happens in a spawn_blocking task on the state_machine side.
    pub fn from_schema(schema: &Schema) -> Self {
        let span = tracing::info_span!("naive_introspection_population");
        let _guard = span.enter();

        let cache = introspect::batch_introspect(
            schema.as_str(),
            KNOWN_INTROSPECTION_QUERIES.iter().cloned().collect(),
        )
        .map(|responses| {
            KNOWN_INTROSPECTION_QUERIES
                .iter()
                .zip(responses)
                .filter_map(|(cache_key, response)| match response.into_result() {
                    Ok(introspection_value) => Some((cache_key.into(), introspection_value)),
                    Err(e) => {
                        let errors = e
                            .iter()
                            .map(std::string::ToString::to_string)
                            .collect::<Vec<_>>()
                            .join("\n");
                        tracing::warn!("introspection returned errors: \n {}", errors);
                        None
                    }
                })
                .collect()
        })
        .unwrap_or_default();

        Self { cache }
    }

    pub fn get(&self, request: &Request) -> Option<&serde_json::Value> {
        let span = tracing::info_span!("introspection_cache");
        let _guard = span.enter();
        self.cache.get(&request.query)
    }
}

#[cfg(test)]
mod naive_introspection_tests {
    use super::*;

    #[test]
    fn test_plan() {
        let query_to_test = "this is a test query";
        let expected_data = serde_json::Value::Number(42.into());

        let cache = [(query_to_test.into(), expected_data.clone())]
            .iter()
            .cloned()
            .collect();
        let naive_introspection = NaiveIntrospection::from_cache(cache);

        let request = Request::builder().query(query_to_test).build();

        assert_eq!(&expected_data, naive_introspection.get(&request).unwrap());
    }

    #[test]
    fn test_known_introspection_queries() {
        // this only makes sure KNOWN_INTROSPECTION_QUERIES get created correctly.
        // thus preventing regressions if a wrong query is added
        // to the `well_known_introspection_queries` folder
        let _ = &*KNOWN_INTROSPECTION_QUERIES;
    }
}
