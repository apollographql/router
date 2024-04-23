//! Authorization plugin

use std::collections::HashMap;
use std::collections::HashSet;
use std::ops::ControlFlow;

use apollo_compiler::ast;
use apollo_compiler::ExecutableDocument;
use http::StatusCode;
use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;
use serde_json_bytes::Value;
use tower::BoxError;
use tower::ServiceBuilder;
use tower::ServiceExt;

use self::authenticated::AuthenticatedCheckVisitor;
use self::authenticated::AuthenticatedVisitor;
use self::authenticated::AUTHENTICATED_SPEC_BASE_URL;
use self::authenticated::AUTHENTICATED_SPEC_VERSION_RANGE;
use self::policy::PolicyExtractionVisitor;
use self::policy::PolicyFilteringVisitor;
use self::policy::POLICY_SPEC_BASE_URL;
use self::policy::POLICY_SPEC_VERSION_RANGE;
use self::scopes::ScopeExtractionVisitor;
use self::scopes::ScopeFilteringVisitor;
use self::scopes::REQUIRES_SCOPES_SPEC_BASE_URL;
use self::scopes::REQUIRES_SCOPES_SPEC_VERSION_RANGE;
use crate::error::QueryPlannerError;
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
use crate::services::layers::query_analysis::ParsedDocumentInner;
use crate::services::supergraph;
use crate::spec::query::transform;
use crate::spec::query::traverse;
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

#[derive(Clone, Debug, Default, Hash, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct CacheKeyMetadata {
    pub(crate) is_authenticated: bool,
    pub(crate) scopes: Vec<String>,
    pub(crate) policies: Vec<String>,
}

/// Authorization plugin
#[derive(Clone, Debug, serde_derive_default::Default, Deserialize, JsonSchema)]
#[allow(dead_code)]
pub(crate) struct Conf {
    /// Reject unauthenticated requests
    #[serde(default)]
    require_authentication: bool,
    /// `@authenticated`, `@requiresScopes` and `@policy` directives
    #[serde(default)]
    directives: Directives,
}

#[derive(Clone, Debug, serde_derive_default::Default, Deserialize, JsonSchema)]
#[allow(dead_code)]
pub(crate) struct Directives {
    /// enables the `@authenticated` and `@requiresScopes` directives
    #[serde(default = "default_enable_directives")]
    enabled: bool,
    /// generates the authorization error messages without modying the query
    #[serde(default)]
    dry_run: bool,
    /// refuse a query entirely if any part would be filtered
    #[serde(default)]
    reject_unauthorized: bool,
    /// authorization errors behaviour
    #[serde(default)]
    errors: ErrorConfig,
}

#[derive(
    Clone, Debug, serde_derive_default::Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema,
)]
#[allow(dead_code)]
pub(crate) struct ErrorConfig {
    /// log authorization errors
    #[serde(default = "enable_log_errors")]
    pub(crate) log: bool,
    /// location of authorization errors in the GraphQL response
    #[serde(default)]
    pub(crate) response: ErrorLocation,
}

fn enable_log_errors() -> bool {
    true
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ErrorLocation {
    /// store authorization errors in the response errors
    #[default]
    Errors,
    /// store authorization errors in the response extensions
    Extensions,
    /// do not add the authorization errors to the GraphQL response
    Disabled,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct UnauthorizedPaths {
    pub(crate) paths: Vec<Path>,
    pub(crate) errors: ErrorConfig,
}

fn default_enable_directives() -> bool {
    true
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
            .and_then(|(_, v)| v.get("directives").and_then(|v| v.as_object()))
            .and_then(|v| v.get("enabled").and_then(|v| v.as_bool()));

        let has_authorization_directives = schema.has_spec(
            AUTHENTICATED_SPEC_BASE_URL,
            AUTHENTICATED_SPEC_VERSION_RANGE,
        ) || schema.has_spec(
            REQUIRES_SCOPES_SPEC_BASE_URL,
            REQUIRES_SCOPES_SPEC_VERSION_RANGE,
        ) || schema
            .has_spec(POLICY_SPEC_BASE_URL, POLICY_SPEC_VERSION_RANGE);

        Ok(has_config.unwrap_or(true) && has_authorization_directives)
    }

    pub(crate) fn log_errors(configuration: &Configuration) -> ErrorConfig {
        configuration
            .apollo_plugins
            .plugins
            .iter()
            .find(|(s, _)| s.as_str() == "authorization")
            .and_then(|(_, v)| v.get("directives").and_then(|v| v.as_object()))
            .and_then(|v| {
                v.get("errors")
                    .and_then(|v| serde_json::from_value(v.clone()).ok())
            })
            .unwrap_or_default()
    }

    pub(crate) fn query_analysis(
        doc: &ParsedDocumentInner,
        operation_name: Option<&str>,
        schema: &Schema,
        context: &Context,
    ) {
        let CacheKeyMetadata {
            is_authenticated,
            scopes,
            policies,
        } = Self::generate_cache_metadata(
            &doc.executable,
            operation_name,
            schema.supergraph_schema(),
            false,
        );
        if is_authenticated {
            context.insert(AUTHENTICATED_KEY, true).unwrap();
        }

        if !scopes.is_empty() {
            context.insert(REQUIRED_SCOPES_KEY, scopes).unwrap();
        }

        if !policies.is_empty() {
            let policies: HashMap<String, Option<bool>> =
                policies.into_iter().map(|policy| (policy, None)).collect();
            context.insert(REQUIRED_POLICIES_KEY, policies).unwrap();
        }
    }

    pub(crate) fn generate_cache_metadata(
        document: &ExecutableDocument,
        operation_name: Option<&str>,
        schema: &apollo_compiler::Schema,
        entity_query: bool,
    ) -> CacheKeyMetadata {
        let mut is_authenticated = false;
        if let Some(mut visitor) = AuthenticatedCheckVisitor::new(schema, document, entity_query) {
            // if this fails, the query is invalid and will fail at the query planning phase.
            // We do not return validation errors here for now because that would imply a huge
            // refactoring of telemetry and tests
            if traverse::document(&mut visitor, document, operation_name).is_ok() && visitor.found {
                is_authenticated = true;
            }
        }

        let mut scopes = Vec::new();
        if let Some(mut visitor) = ScopeExtractionVisitor::new(schema, document, entity_query) {
            // if this fails, the query is invalid and will fail at the query planning phase.
            // We do not return validation errors here for now because that would imply a huge
            // refactoring of telemetry and tests
            if traverse::document(&mut visitor, document, operation_name).is_ok() {
                scopes = visitor.extracted_scopes.into_iter().collect();
            }
        }

        let mut policies: Vec<String> = Vec::new();
        if let Some(mut visitor) = PolicyExtractionVisitor::new(schema, document, entity_query) {
            // if this fails, the query is invalid and will fail at the query planning phase.
            // We do not return validation errors here for now because that would imply a huge
            // refactoring of telemetry and tests
            if traverse::document(&mut visitor, document, operation_name).is_ok() {
                policies = visitor.extracted_policies.into_iter().collect();
            }
        }

        CacheKeyMetadata {
            is_authenticated,
            scopes,
            policies,
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

        context.extensions().lock().insert(CacheKeyMetadata {
            is_authenticated,
            scopes,
            policies,
        });
    }

    pub(crate) fn intersect_cache_keys_subgraph(
        left: &CacheKeyMetadata,
        right: &CacheKeyMetadata,
    ) -> CacheKeyMetadata {
        CacheKeyMetadata {
            is_authenticated: left.is_authenticated && right.is_authenticated,
            scopes: left
                .scopes
                .iter()
                .collect::<HashSet<_>>()
                .intersection(&right.scopes.iter().collect::<HashSet<_>>())
                .map(|s| s.to_string())
                .collect(),
            policies: left
                .policies
                .iter()
                .collect::<HashSet<_>>()
                .intersection(&right.policies.iter().collect::<HashSet<_>>())
                .map(|s| s.to_string())
                .collect(),
        }
    }

    pub(crate) fn filter_query(
        configuration: &Configuration,
        key: &QueryKey,
        schema: &Schema,
    ) -> Result<Option<FilteredQuery>, QueryPlannerError> {
        let (reject_unauthorized, dry_run) = configuration
            .apollo_plugins
            .plugins
            .iter()
            .find(|(s, _)| s.as_str() == "authorization")
            .and_then(|(_, v)| v.get("directives").and_then(|v| v.as_object()))
            .map(|config| {
                (
                    config
                        .get("reject_unauthorized")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false),
                    config
                        .get("dry_run")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false),
                )
            })
            .unwrap_or((false, false));

        // The filtered query will then be used
        // to generate selections for response formatting, to execute introspection and
        // generating a query plan

        // TODO: do we need to (re)parse here?
        let doc = ast::Document::parse(&key.filtered_query, "filtered_query")
            // Ignore parse errors: assume they’ve been handled elsewhere
            .unwrap_or_else(|invalid| invalid.partial);

        let is_authenticated = key.metadata.is_authenticated;
        let scopes = &key.metadata.scopes;
        let policies = &key.metadata.policies;

        let mut is_filtered = false;
        let mut unauthorized_paths: Vec<Path> = vec![];

        let filter_res = Self::authenticated_filter_query(schema, dry_run, &doc, is_authenticated)?;

        let doc = match filter_res {
            None => doc,
            Some((filtered_doc, paths)) => {
                unauthorized_paths.extend(paths);

                // FIXME: consider only `filtered_doc.get_operation(key.operation_name)`?
                if filtered_doc.definitions.is_empty() {
                    return Err(QueryPlannerError::Unauthorized(unauthorized_paths));
                }

                is_filtered = true;

                filtered_doc
            }
        };

        let filter_res = Self::scopes_filter_query(schema, dry_run, &doc, scopes)?;

        let doc = match filter_res {
            None => doc,
            Some((filtered_doc, paths)) => {
                unauthorized_paths.extend(paths);

                // FIXME: consider only `filtered_doc.get_operation(key.operation_name)`?
                if filtered_doc.definitions.is_empty() {
                    return Err(QueryPlannerError::Unauthorized(unauthorized_paths));
                }

                is_filtered = true;

                filtered_doc
            }
        };

        let filter_res = Self::policies_filter_query(schema, dry_run, &doc, policies)?;

        let doc = match filter_res {
            None => doc,
            Some((filtered_doc, paths)) => {
                unauthorized_paths.extend(paths);

                // FIXME: consider only `filtered_doc.get_operation(key.operation_name)`?
                if filtered_doc.definitions.is_empty() {
                    return Err(QueryPlannerError::Unauthorized(unauthorized_paths));
                }

                is_filtered = true;

                filtered_doc
            }
        };

        if reject_unauthorized && !unauthorized_paths.is_empty() {
            return Err(QueryPlannerError::Unauthorized(unauthorized_paths));
        }

        if is_filtered {
            Ok(Some((unauthorized_paths, doc)))
        } else {
            Ok(None)
        }
    }

    fn authenticated_filter_query(
        schema: &Schema,
        dry_run: bool,
        doc: &ast::Document,
        is_authenticated: bool,
    ) -> Result<Option<(ast::Document, Vec<Path>)>, QueryPlannerError> {
        if let Some(mut visitor) = AuthenticatedVisitor::new(
            schema.supergraph_schema(),
            doc,
            &schema.implementers_map,
            dry_run,
        ) {
            let modified_query = transform::document(&mut visitor, doc)
                .map_err(|e| SpecError::TransformError(e.to_string()))?;

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
        } else {
            tracing::debug!("the schema does not contain @authenticated");
            Ok(None)
        }
    }

    fn scopes_filter_query(
        schema: &Schema,
        dry_run: bool,
        doc: &ast::Document,
        scopes: &[String],
    ) -> Result<Option<(ast::Document, Vec<Path>)>, QueryPlannerError> {
        if let Some(mut visitor) = ScopeFilteringVisitor::new(
            schema.supergraph_schema(),
            doc,
            &schema.implementers_map,
            scopes.iter().cloned().collect(),
            dry_run,
        ) {
            let modified_query = transform::document(&mut visitor, doc)
                .map_err(|e| SpecError::TransformError(e.to_string()))?;
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
        } else {
            tracing::debug!("the schema does not contain @requiresScopes");
            Ok(None)
        }
    }

    fn policies_filter_query(
        schema: &Schema,
        dry_run: bool,

        doc: &ast::Document,
        policies: &[String],
    ) -> Result<Option<(ast::Document, Vec<Path>)>, QueryPlannerError> {
        if let Some(mut visitor) = PolicyFilteringVisitor::new(
            schema.supergraph_schema(),
            doc,
            &schema.implementers_map,
            policies.iter().cloned().collect(),
            dry_run,
        ) {
            let modified_query = transform::document(&mut visitor, doc)
                .map_err(|e| SpecError::TransformError(e.to_string()))?;

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
        } else {
            tracing::debug!("the schema does not contain @policy");
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
                let filtered = !request.query_plan.query.unauthorized.paths.is_empty();
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
