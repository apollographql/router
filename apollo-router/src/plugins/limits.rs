//! Configurable complexity limiting

use std::ops::ControlFlow;

use apollo_compiler::hir;
use http::StatusCode;
use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;
use tower::BoxError;
use tower::ServiceBuilder;
use tower::ServiceExt;

use crate::graphql::Error;
use crate::layers::ServiceBuilderExt;
use crate::plugin::Plugin;
use crate::plugin::PluginInit;
use crate::register_plugin;
use crate::services::execution;

struct Limits {
    conf: Conf,
}

/// Requests that exceed a configured limit are rejected with a GraphQL error
#[derive(Clone, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct Conf {
    /// Limit query nesting level
    selection_set_depth: Option<u32>,
}

impl Conf {
    // Returns whether this plugin does anything
    fn any(&self) -> bool {
        let Self {
            selection_set_depth,
        } = self;
        selection_set_depth.is_some()
    }
}

#[async_trait::async_trait]
impl Plugin for Limits {
    type Config = Conf;

    async fn new(init: PluginInit<Self::Config>) -> Result<Self, BoxError> {
        Ok(Self { conf: init.config })
    }

    fn execution_service(&self, service: execution::BoxService) -> execution::BoxService {
        if !self.conf.any() {
            return service;
        }
        let conf = self.conf.clone();
        ServiceBuilder::new()
            .checkpoint(move |req: execution::Request| {
                let operation = req.operation_definition();
                if let Some(limit) = conf.selection_set_depth {
                    let depth = selection_set_depth(&req.compiler, operation.selection_set());
                    if depth > limit {
                        let error = Error::builder()
                            .message("Operation exceeds configured depth limit")
                            .extension_code("SELECTION_SET_DEPTH_LIMIT_EXCEEDED")
                            .build();
                        let res = execution::Response::builder()
                            .error(error)
                            .status_code(StatusCode::BAD_REQUEST)
                            .context(req.context)
                            .build()?;
                        return Ok(ControlFlow::Break(res));
                    }
                }
                Ok(ControlFlow::Continue(req))
            })
            .service(service)
            .boxed()
    }
}

register_plugin!("apollo", "limits", Limits);

fn selection_set_depth(db: &apollo_compiler::Snapshot, selection_set: &hir::SelectionSet) -> u32 {
    selection_set
        .selection()
        .iter()
        .map(|selection| match selection {
            hir::Selection::Field(field) => 1 + selection_set_depth(db, field.selection_set()),
            hir::Selection::InlineFragment(inline) => {
                selection_set_depth(db, inline.selection_set())
            }
            hir::Selection::FragmentSpread(spread) => spread
                .fragment(&**db)
                .map(|frag| selection_set_depth(db, frag.selection_set()))
                .unwrap_or(0),
        })
        .max()
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use tower::Service;
    use tower::ServiceExt;

    use crate::graphql;
    use crate::services::supergraph;
    use crate::TestHarness;

    #[tokio::test]
    async fn test_just_under_and_just_above_selection_set_depth_limit() {
        let harness = &mut TestHarness::builder()
            .configuration_json(serde_json::json!({
                "limits": {
                    "selection_set_depth" : 5,
                },
                "include_subgraph_errors": {
                    "all": true
                }
            }))
            .unwrap()
            .build_supergraph()
            .await
            .unwrap();

        let query = "{ me { reviews { product { reviews { body } }}}}";
        let response = call(harness, query).await;
        assert!(response.data.is_some());
        assert!(response.errors.is_empty());

        let query = "{ me { reviews { product { reviews { author { name } } }}}}";
        let response = call(harness, query).await;
        assert!(response.data.is_none());
        assert_eq!(response.errors.len(), 1);
        assert_eq!(
            response.errors[0].extensions["code"].as_str().unwrap(),
            "SELECTION_SET_DEPTH_LIMIT_EXCEEDED"
        );
    }

    #[track_caller]
    async fn call(
        test_harness: &mut supergraph::BoxCloneService,
        query: &str,
    ) -> graphql::Response {
        let request = supergraph::Request::fake_builder()
            .query(query)
            .build()
            .unwrap();
        test_harness
            .ready()
            .await
            .unwrap()
            .call(request)
            .await
            .unwrap()
            .next_response()
            .await
            .unwrap()
    }
}
