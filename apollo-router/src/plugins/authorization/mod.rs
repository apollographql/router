//! Authorization plugin

use std::collections::HashMap;
use std::collections::HashSet;
use std::ops::ControlFlow;
use std::sync::Arc;

use apollo_compiler::ApolloCompiler;
use apollo_compiler::InputDatabase;
use http::StatusCode;
use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;
use serde_json_bytes::Value;
use tokio::sync::Mutex;
use tower::BoxError;
use tower::ServiceBuilder;
use tower::ServiceExt;

use self::authenticated::AuthenticatedCheckVisitor;
use self::authenticated::AuthenticatedVisitor;
use self::authenticated::AUTHENTICATED_DIRECTIVE_NAME;
use self::policy::PolicyExtractionVisitor;
use self::policy::PolicyFilteringVisitor;
use self::policy::POLICY_DIRECTIVE_NAME;
use self::scopes::ScopeExtractionVisitor;
use self::scopes::ScopeFilteringVisitor;
use self::scopes::REQUIRES_SCOPES_DIRECTIVE_NAME;
use crate::error::QueryPlannerError;
use crate::error::SchemaError;
use crate::error::ServiceBuildError;
use crate::graphql;
use crate::json_ext::Path;
use crate::layers::ServiceBuilderExt;
use crate::plugin::Plugin;
use crate::plugin::PluginInit;
use crate::plugins::authentication::APOLLO_AUTHENTICATION_JWT_CLAIMS;
use crate::query_planner::FilteredQuery;
use crate::query_planner::QueryKey;
use crate::register_plugin;
use crate::services::execution;
use crate::services::supergraph;
use crate::spec::query::transform;
use crate::spec::query::traverse;
use crate::spec::query::QUERY_EXECUTABLE;
use crate::spec::Query;
use crate::spec::Schema;
use crate::spec::SpecError;
use crate::Configuration;
use crate::Context;

pub(crate) mod authenticated;
pub(crate) mod policy;
pub(crate) mod scopes;

const AUTHENTICATED_KEY: &str = "apollo_authorization::authenticated::required";
const REQUIRED_SCOPES_KEY: &str = "apollo_authorization::scopes::required";
const REQUIRED_POLICIES_KEY: &str = "apollo_authorization::policies::required";

#[derive(Clone, Debug, Default, Hash, PartialEq, Eq, Serialize)]
pub(crate) struct CacheKeyMetadata {
    is_authenticated: bool,
    scopes: Vec<String>,
    policies: Vec<String>,
}

/// Authorization plugin
#[derive(Clone, Debug, Default, Deserialize, JsonSchema)]
#[allow(dead_code)]
pub(crate) struct Conf {
    /// Reject unauthenticated requests
    #[serde(default)]
    require_authentication: bool,
    /// `@authenticated` and `@requiresScopes` directives
    #[serde(default)]
    preview_directives: Directives,
}

#[derive(Clone, Debug, Default, Deserialize, JsonSchema)]
#[allow(dead_code)]
pub(crate) struct Directives {
    /// enables the `@authenticated` and `@requiresScopes` directives
    #[serde(default)]
    enabled: bool,
}

pub(crate) struct AuthorizationPlugin {
    require_authentication: bool,
}

impl AuthorizationPlugin {
    pub(crate) fn enable_directives(
        configuration: &Configuration,
        schema: &Schema,
    ) -> Result<bool, ServiceBuildError> {
        let has_config = configuration
            .apollo_plugins
            .plugins
            .iter()
            .find(|(s, _)| s.as_str() == "authorization")
            .and_then(|(_, v)| v.get("preview_directives").and_then(|v| v.as_object()))
            .and_then(|v| v.get("enabled").and_then(|v| v.as_bool()));
        let has_authorization_directives = schema
            .type_system
            .definitions
            .directives
            .contains_key(AUTHENTICATED_DIRECTIVE_NAME)
            || schema
                .type_system
                .definitions
                .directives
                .contains_key(REQUIRES_SCOPES_DIRECTIVE_NAME)
            || schema
                .type_system
                .definitions
                .directives
                .contains_key(POLICY_DIRECTIVE_NAME);

        match has_config {
            Some(b) => Ok(b),
            None => {
                if has_authorization_directives {
                    Err(ServiceBuildError::Schema(SchemaError::Api("cannot start the router on a schema with authorization directives without configuring the authorization plugin".to_string())))
                } else {
                    Ok(false)
                }
            }
        }
    }

    pub(crate) async fn query_analysis(
        query: &str,
        schema: &Schema,
        configuration: &Configuration,
        context: &Context,
    ) {
        let (compiler, file_id) = Query::make_compiler(query, schema, configuration);

        let mut visitor = AuthenticatedCheckVisitor::new(&compiler, file_id);

        // if this fails, the query is invalid and will fail at the query planning phase.
        // We do not return validation errors here for now because that would imply a huge
        // refactoring of telemetry and tests
        if traverse::document(&mut visitor, file_id).is_ok() && visitor.found {
            context.insert(AUTHENTICATED_KEY, true).unwrap();
        }

        let mut visitor = ScopeExtractionVisitor::new(&compiler, file_id);

        // if this fails, the query is invalid and will fail at the query planning phase.
        // We do not return validation errors here for now because that would imply a huge
        // refactoring of telemetry and tests
        if traverse::document(&mut visitor, file_id).is_ok() {
            let scopes: Vec<String> = visitor.extracted_scopes.into_iter().collect();

            if !scopes.is_empty() {
                context.insert(REQUIRED_SCOPES_KEY, scopes).unwrap();
            }
        }

        // TODO: @policy is out of scope for preview, this will be reactivated later
        if false {
            let mut visitor = PolicyExtractionVisitor::new(&compiler, file_id);

            // if this fails, the query is invalid and will fail at the query planning phase.
            // We do not return validation errors here for now because that would imply a huge
            // refactoring of telemetry and tests
            if traverse::document(&mut visitor, file_id).is_ok() {
                let policies: HashMap<String, Option<bool>> = visitor
                    .extracted_policies
                    .into_iter()
                    .map(|policy| (policy, None))
                    .collect();

                if !policies.is_empty() {
                    context.insert(REQUIRED_POLICIES_KEY, policies).unwrap();
                }
            }
        }
    }

    pub(crate) fn update_cache_key(context: &Context) {
        let is_authenticated = context.contains_key(APOLLO_AUTHENTICATION_JWT_CLAIMS);

        let request_scopes = context
            .get_json_value(APOLLO_AUTHENTICATION_JWT_CLAIMS)
            .and_then(|value| {
                value.as_object().and_then(|object| {
                    object.get("scope").and_then(|v| {
                        v.as_str()
                            .map(|s| s.split(' ').map(|s| s.to_string()).collect::<HashSet<_>>())
                    })
                })
            });
        let query_scopes = context.get_json_value(REQUIRED_SCOPES_KEY).and_then(|v| {
            v.as_array().map(|v| {
                v.iter()
                    .filter_map(|s| s.as_str().map(|s| s.to_string()))
                    .collect::<HashSet<_>>()
            })
        });

        let mut scopes = match (request_scopes, query_scopes) {
            (None, _) => vec![],
            (_, None) => vec![],
            (Some(req), Some(query)) => req.intersection(&query).cloned().collect(),
        };
        scopes.sort();

        let mut policies = context
            .get_json_value(REQUIRED_POLICIES_KEY)
            .and_then(|v| {
                v.as_object().map(|v| {
                    v.iter()
                        .filter_map(|(policy, result)| match result {
                            Value::Bool(true) => Some(policy.as_str().to_string()),
                            _ => None,
                        })
                        .collect::<Vec<String>>()
                })
            })
            .unwrap_or_default();
        policies.sort();

        context.private_entries.lock().insert(CacheKeyMetadata {
            is_authenticated,
            scopes,
            policies,
        });
    }

    pub(crate) fn filter_query(
        key: &QueryKey,
        schema: &Schema,
    ) -> Result<Option<FilteredQuery>, QueryPlannerError> {
        // we create a compiler to filter the query. The filtered query will then be used
        // to generate selections for response formatting, to execute introspection and
        // generating a query plan
        let mut compiler = ApolloCompiler::new();
        compiler.set_type_system_hir(schema.type_system.clone());
        let _id = compiler.add_executable(&key.filtered_query, "query");

        let is_authenticated = key.metadata.is_authenticated;
        let scopes = &key.metadata.scopes;
        let policies = &key.metadata.policies;

        let mut is_filtered = false;
        let mut unauthorized_paths: Vec<Path> = vec![];

        let filter_res = Self::authenticated_filter_query(&compiler, is_authenticated)?;

        let compiler = match filter_res {
            None => compiler,
            Some((query, paths)) => {
                unauthorized_paths.extend(paths);

                if query.is_empty() {
                    return Err(QueryPlannerError::Unauthorized(unauthorized_paths));
                }

                is_filtered = true;

                let mut compiler = ApolloCompiler::new();
                compiler.set_type_system_hir(schema.type_system.clone());
                let _id = compiler.add_executable(&query, "query");
                compiler
            }
        };

        let filter_res = Self::scopes_filter_query(&compiler, scopes)?;

        let compiler = match filter_res {
            None => compiler,
            Some((query, paths)) => {
                unauthorized_paths.extend(paths);

                if query.is_empty() {
                    return Err(QueryPlannerError::Unauthorized(unauthorized_paths));
                }

                is_filtered = true;

                let mut compiler = ApolloCompiler::new();
                compiler.set_type_system_hir(schema.type_system.clone());
                let _id = compiler.add_executable(&query, "query");
                compiler
            }
        };

        let filter_res = Self::policies_filter_query(&compiler, policies)?;

        let compiler = match filter_res {
            None => compiler,
            Some((query, paths)) => {
                unauthorized_paths.extend(paths);

                if query.is_empty() {
                    return Err(QueryPlannerError::Unauthorized(unauthorized_paths));
                }

                is_filtered = true;

                let mut compiler = ApolloCompiler::new();
                compiler.set_type_system_hir(schema.type_system.clone());
                let _id = compiler.add_executable(&query, "query");
                compiler
            }
        };

        if is_filtered {
            let file_id = compiler
                .db
                .source_file(QUERY_EXECUTABLE.into())
                .ok_or_else(|| {
                    QueryPlannerError::SpecError(SpecError::ValidationError(
                        "missing input file for query".to_string(),
                    ))
                })?;
            let filtered_query = compiler.db.source_code(file_id).to_string();

            Ok(Some((
                filtered_query,
                unauthorized_paths,
                Arc::new(Mutex::new(compiler)),
            )))
        } else {
            Ok(None)
        }
    }

    fn authenticated_filter_query(
        compiler: &ApolloCompiler,
        is_authenticated: bool,
    ) -> Result<Option<(String, Vec<Path>)>, QueryPlannerError> {
        let id = compiler
            .db
            .executable_definition_files()
            .pop()
            .expect("the query was added to the compiler earlier");

        let mut visitor = AuthenticatedVisitor::new(compiler, id);
        let modified_query = transform::document(&mut visitor, id)
            .map_err(|e| SpecError::ParsingError(e.to_string()))?
            .to_string();

        if visitor.query_requires_authentication {
            if is_authenticated {
                tracing::debug!("the query contains @authenticated, the request is authenticated, keeping the query");
                Ok(None)
            } else {
                tracing::debug!("the query contains @authenticated, modified query:\n{modified_query}\nunauthorized paths: {:?}", visitor
                .unauthorized_paths
                .iter()
                .map(|path| path.to_string())
                .collect::<Vec<_>>());

                Ok(Some((modified_query, visitor.unauthorized_paths)))
            }
        } else {
            tracing::debug!("the query does not contain @authenticated");
            Ok(None)
        }
    }

    fn scopes_filter_query(
        compiler: &ApolloCompiler,
        scopes: &[String],
    ) -> Result<Option<(String, Vec<Path>)>, QueryPlannerError> {
        let id = compiler
            .db
            .executable_definition_files()
            .pop()
            .expect("the query was added to the compiler earlier");

        let mut visitor =
            ScopeFilteringVisitor::new(compiler, id, scopes.iter().cloned().collect());

        let modified_query = transform::document(&mut visitor, id)
            .map_err(|e| SpecError::ParsingError(e.to_string()))?
            .to_string();

        if visitor.query_requires_scopes {
            tracing::debug!("the query required scopes, the requests present scopes: {scopes:?}, modified query:\n{modified_query}\nunauthorized paths: {:?}",
                visitor
                    .unauthorized_paths
                    .iter()
                    .map(|path| path.to_string())
                    .collect::<Vec<_>>()
            );
            Ok(Some((modified_query, visitor.unauthorized_paths)))
        } else {
            tracing::debug!("the query does not require scopes");
            Ok(None)
        }
    }

    fn policies_filter_query(
        compiler: &ApolloCompiler,
        policies: &[String],
    ) -> Result<Option<(String, Vec<Path>)>, QueryPlannerError> {
        let id = compiler
            .db
            .executable_definition_files()
            .pop()
            .expect("the query was added to the compiler earlier");

        let mut visitor =
            PolicyFilteringVisitor::new(compiler, id, policies.iter().cloned().collect());

        let modified_query = transform::document(&mut visitor, id)
            .map_err(|e| SpecError::ParsingError(e.to_string()))?
            .to_string();

        if visitor.query_requires_policies {
            tracing::debug!("the query required policies, the requests present policies: {policies:?}, modified query:\n{modified_query}\nunauthorized paths: {:?}",
                visitor
                    .unauthorized_paths
                    .iter()
                    .map(|path| path.to_string())
                    .collect::<Vec<_>>()
            );
            Ok(Some((modified_query, visitor.unauthorized_paths)))
        } else {
            tracing::debug!("the query does not require policies");
            Ok(None)
        }
    }
}

#[async_trait::async_trait]
impl Plugin for AuthorizationPlugin {
    type Config = Conf;

    async fn new(init: PluginInit<Self::Config>) -> Result<Self, BoxError> {
        Ok(AuthorizationPlugin {
            require_authentication: init.config.require_authentication,
        })
    }

    fn supergraph_service(&self, service: supergraph::BoxService) -> supergraph::BoxService {
        if self.require_authentication {
            ServiceBuilder::new()
                .checkpoint(move |request: supergraph::Request| {
                    if request
                        .context
                        .contains_key(APOLLO_AUTHENTICATION_JWT_CLAIMS)
                    {
                        Ok(ControlFlow::Continue(request))
                    } else {
                        // This is a metric and will not appear in the logs
                        tracing::info!(
                            monotonic_counter.apollo_require_authentication_failure_count = 1u64,
                        );
                        tracing::error!("rejecting unauthenticated request");
                        let response = supergraph::Response::error_builder()
                            .error(
                                graphql::Error::builder()
                                    .message("unauthenticated".to_string())
                                    .extension_code("AUTH_ERROR")
                                    .build(),
                            )
                            .status_code(StatusCode::UNAUTHORIZED)
                            .context(request.context)
                            .build()?;
                        Ok(ControlFlow::Break(response))
                    }
                })
                .service(service)
                .boxed()
        } else {
            service
        }
    }

    fn execution_service(&self, service: execution::BoxService) -> execution::BoxService {
        ServiceBuilder::new()
            .map_request(|request: execution::Request| {
                let filtered = !request.query_plan.query.unauthorized_paths.is_empty();
                let needs_authenticated = request.context.contains_key(AUTHENTICATED_KEY);
                let needs_requires_scopes = request.context.contains_key(REQUIRED_SCOPES_KEY);

                if needs_authenticated || needs_requires_scopes {
                    tracing::info!(
                        monotonic_counter.apollo.router.operations.authorization = 1u64,
                        authorization.filtered = filtered,
                        authorization.needs_authenticated = needs_authenticated,
                        authorization.needs_requires_scopes = needs_requires_scopes,
                    );
                }

                request
            })
            .service(service)
            .boxed()
    }
}

// This macro allows us to use it in our plugin registry!
// register_plugin takes a group name, and a plugin name.
//
// In order to keep the plugin names consistent,
// we use using the `Reverse domain name notation`
register_plugin!("apollo", "authorization", AuthorizationPlugin);

#[cfg(test)]
mod tests;
