mod cost_analyzer;
mod cost_directive;
mod list_size_directive;

use std::ops::ControlFlow;

use apollo_compiler::ast::Document;
use apollo_compiler::validation::Valid;
use apollo_compiler::Schema;
use futures::StreamExt;
use http::StatusCode;
use schemars::JsonSchema;
use serde::Deserialize;
use tower::BoxError;
use tower::ServiceBuilder;
use tower::ServiceExt;

use crate::layers::ServiceBuilderExt;
use crate::plugin::Plugin;
use crate::plugin::PluginInit;
use crate::plugins::cost_analysis::cost_analyzer::CostAnalyzer;
use crate::register_plugin;
use crate::services::execution::BoxService;
use crate::services::execution::Request;
use crate::services::execution::Response;

#[derive(Debug, Clone, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, default)]
pub(crate) struct Config {
    /// The Cost Analysis plugin is disabled by default.
    enabled: bool, // TODO: Is the actually off by default?
    /// The maximum allowed cost of any query
    static_query_cost_limit: u64,
}

pub(crate) struct CostAnalysis {
    config: Config,
    supergraph_schema: Valid<Schema>,
}

#[async_trait::async_trait]
impl Plugin for CostAnalysis {
    type Config = Config;

    async fn new(init: PluginInit<Self::Config>) -> Result<Self, BoxError> {
        let supergraph_schema =
            apollo_compiler::Schema::parse_and_validate(init.supergraph_sdl.to_string(), "")
                .expect("failed to parse supergraph schema");

        Ok(CostAnalysis {
            config: init.config,
            supergraph_schema,
        })
    }

    fn execution_service(&self, service: BoxService) -> BoxService {
        if !self.config.enabled {
            service
        } else {
            let supergraph_schema = self.supergraph_schema.clone();

            ServiceBuilder::new()
                .checkpoint(move |req: Request| {
                    let query_doc = Document::parse(req.query_plan.query.string.clone(), "")
                        .expect("query string could not be parsed");
                    let mut analyzer = CostAnalyzer::new(&supergraph_schema);

                    match analyzer.estimate(&query_doc) {
                        Ok(_) => Ok(ControlFlow::Continue(req)),
                        Err(error) => {
                            let res = Response::infallible_builder()
                                // TODO .error(error)
                                .status_code(StatusCode::BAD_REQUEST)
                                .context(req.context)
                                .build();
                            Ok(ControlFlow::Break(res))
                        }
                    }
                })
                .map_response(|res: Response| {
                    // TODO: Dynamic cost analysis
                    res.map(|gql_stream| gql_stream.map(|gql_res| gql_res).boxed())
                })
                .service(service)
                .boxed()
        }
    }
}

register_plugin!("apollo", "cost_analysis", CostAnalysis);
