//! Helpers for out-of-crate benchmarks. Not part of the public API.

use crate::graphql::Response;
use crate::spec::Query;
use crate::spec::Schema;
use crate::spec::query::subselections::BooleanValues;

pub struct FormatResponseBench {
    query: Query,
    schema: Schema,
    response_data: serde_json_bytes::Value,
}

impl FormatResponseBench {
    /// Build a benchmark fixture from a supergraph SDL, a query string, and
    /// the JSON value that subgraphs would return as `data`.
    pub fn new(
        supergraph_sdl: &str,
        query_text: &str,
        response_data: serde_json_bytes::Value,
    ) -> Self {
        Self::with_variables(supergraph_sdl, query_text, response_data)
    }

    /// Same as [`Self::new`] but with explicit variables.
    ///
    /// Some router internals expect a Tokio runtime during initialisation.
    /// This constructor creates a temporary one when none is available.
    pub fn with_variables(
        supergraph_sdl: &str,
        query_text: &str,
        response_data: serde_json_bytes::Value,
    ) -> Self {
        let build = || {
            let schema = Schema::parse(supergraph_sdl, &Default::default())
                .expect("benchmarking: schema must be valid");
            let query = Query::parse(query_text, None, &schema, &Default::default())
                .expect("benchmarking: query must be valid");
            (schema, query)
        };

        let (schema, query) = if tokio::runtime::Handle::try_current().is_ok() {
            build()
        } else {
            let rt = tokio::runtime::Runtime::new().expect("benchmarking: tokio runtime");
            rt.block_on(async { build() })
        };

        Self {
            query,
            schema,
            response_data,
        }
    }

    /// Run one iteration of `format_response`.
    pub fn run(&self) {
        let mut response = Response::builder().data(self.response_data.clone()).build();
        self.query.format_response(
            &mut response,
            Default::default(),
            self.schema.api_schema(),
            BooleanValues { bits: 0 },
            false,
        );
    }
}

/// Wraps user-supplied type definitions in the federation boilerplate required
/// to produce a valid supergraph SDL (Federation v1 style).
pub fn with_supergraph_boilerplate(content: &str) -> String {
    format!(
        r#"
    schema
        @core(feature: "https://specs.apollo.dev/core/v0.1")
        @core(feature: "https://specs.apollo.dev/join/v0.1")
        @core(feature: "https://specs.apollo.dev/inaccessible/v0.1")
         {{
        query: Query
    }}
    directive @core(feature: String!) repeatable on SCHEMA
    directive @join__graph(name: String!, url: String!) on ENUM_VALUE
    directive @inaccessible on OBJECT | FIELD_DEFINITION | INTERFACE | UNION
    enum join__Graph {{
        TEST @join__graph(name: "test", url: "http://localhost:4001/graphql")
    }}

    {content}
    "#
    )
}
