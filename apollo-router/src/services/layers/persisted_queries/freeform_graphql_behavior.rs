use std::collections::HashMap;

use apollo_compiler::ast;

use super::PersistedQueryManifest;
use crate::Configuration;

/// Describes whether the router should allow or deny a given request.
/// with an error, or allow it but log the operation as unknown.
pub(crate) struct FreeformGraphQLAction {
    pub(crate) should_allow: bool,
    pub(crate) should_log: bool,
    pub(crate) pq_id: Option<String>,
}

/// How the router should respond to requests that are not resolved as the IDs
/// of an operation in the manifest. (For the most part this means "requests
/// sent as freeform GraphQL", though it also includes requests sent as an ID
/// that is not found in the PQ manifest but is found in the APQ cache; because
/// you cannot combine APQs with safelisting, this is only relevant in "allow
/// all" and "log unknown" modes.)
#[derive(Debug)]
pub(crate) enum FreeformGraphQLBehavior {
    AllowAll {
        apq_enabled: bool,
    },
    DenyAll {
        log_unknown: bool,
    },
    AllowIfInSafelist {
        safelist: FreeformGraphQLSafelist,
        log_unknown: bool,
    },
    LogUnlessInSafelist {
        safelist: FreeformGraphQLSafelist,
        apq_enabled: bool,
    },
}

impl FreeformGraphQLBehavior {
    pub(super) fn action_for_freeform_graphql(
        &self,
        ast: Result<&ast::Document, &str>,
    ) -> FreeformGraphQLAction {
        match self {
            FreeformGraphQLBehavior::AllowAll { .. } => FreeformGraphQLAction {
                should_allow: true,
                should_log: false,
                pq_id: None,
            },
            // Note that this branch doesn't get called in practice, because we catch
            // DenyAll at an earlier phase with never_allows_freeform_graphql.
            FreeformGraphQLBehavior::DenyAll { log_unknown, .. } => FreeformGraphQLAction {
                should_allow: false,
                should_log: *log_unknown,
                pq_id: None,
            },
            FreeformGraphQLBehavior::AllowIfInSafelist {
                safelist,
                log_unknown,
                ..
            } => {
                let pq_id = safelist.get_pq_id_for_body(ast);
                if pq_id.is_some() {
                    FreeformGraphQLAction {
                        should_allow: true,
                        should_log: false,
                        pq_id,
                    }
                } else {
                    FreeformGraphQLAction {
                        should_allow: false,
                        should_log: *log_unknown,
                        pq_id: None,
                    }
                }
            }
            FreeformGraphQLBehavior::LogUnlessInSafelist { safelist, .. } => {
                let pq_id = safelist.get_pq_id_for_body(ast);
                FreeformGraphQLAction {
                    should_allow: true,
                    should_log: pq_id.is_none(),
                    pq_id,
                }
            }
        }
    }
}

/// The normalized bodies of all operations in the PQ manifest. This is a map of
/// normalized body string to PQ operation ID (usually a hash of the operation body).
///
/// Normalization currently consists of:
/// - Sorting the top-level definitions (operation and fragment definitions)
///   deterministically.
/// - Printing the AST using apollo-encoder's default formatting (ie,
///   normalizing all ignored characters such as whitespace and comments).
///
/// Sorting top-level definitions is important because common clients such as
/// Apollo Client Web have modes of use where it is easy to find all the
/// operation and fragment definitions at build time, but challenging to
/// determine what order the client will put them in at run time.
///
/// Normalizing ignored characters is helpful because being strict on whitespace
/// is more likely to get in your way than to aid in security --- but more
/// importantly, once we're doing any normalization at all, it's much easier to
/// normalize to the default formatting instead of trying to preserve
/// formatting.
#[derive(Debug)]
pub(crate) struct FreeformGraphQLSafelist {
    normalized_bodies: HashMap<String, String>,
}

impl FreeformGraphQLSafelist {
    pub(super) fn new(manifest: &PersistedQueryManifest) -> Self {
        let mut safelist = Self {
            normalized_bodies: HashMap::new(),
        };

        for (key, body) in manifest.iter() {
            safelist.insert_from_manifest(body, &key.operation_id);
        }

        safelist
    }

    fn insert_from_manifest(&mut self, body_from_manifest: &str, operation_id: &str) {
        let normalized_body = self.normalize_body(
            ast::Document::parse(body_from_manifest, "from_manifest")
                .as_ref()
                .map_err(|_| body_from_manifest),
        );
        self.normalized_bodies
            .insert(normalized_body, operation_id.to_string());
    }

    pub(super) fn get_pq_id_for_body(&self, ast: Result<&ast::Document, &str>) -> Option<String> {
        // Note: consider adding an LRU cache that caches this function's return
        // value based solely on body_from_request without needing to normalize
        // the body.
        self.normalized_bodies
            .get(&self.normalize_body(ast))
            .cloned()
    }

    pub(super) fn normalize_body(&self, ast: Result<&ast::Document, &str>) -> String {
        match ast {
            Err(body_from_request) => {
                // If we can't parse the operation (whether from the PQ list or the
                // incoming request), then we can't normalize it. We keep it around
                // unnormalized, so that it at least works as a byte-for-byte
                // safelist entry.
                body_from_request.to_string()
            }
            Ok(ast) => {
                let mut operations = vec![];
                let mut fragments = vec![];

                for definition in &ast.definitions {
                    match definition {
                        ast::Definition::OperationDefinition(def) => operations.push(def.clone()),
                        ast::Definition::FragmentDefinition(def) => fragments.push(def.clone()),
                        _ => {}
                    }
                }

                let mut new_document = ast::Document::new();

                // First include operation definitions, sorted by name.
                operations.sort_by_key(|x| x.name.clone());
                new_document
                    .definitions
                    .extend(operations.into_iter().map(Into::into));

                // Next include fragment definitions, sorted by name.
                fragments.sort_by_key(|x| x.name.clone());
                new_document
                    .definitions
                    .extend(fragments.into_iter().map(Into::into));
                new_document.to_string()
            }
        }
    }
}

/// Determine behavior based on PQ configuration
pub(super) fn get_freeform_graphql_behavior(
    config: &Configuration,
    new_manifest: &PersistedQueryManifest,
) -> FreeformGraphQLBehavior {
    if config.persisted_queries.safelist.enabled {
        if config.persisted_queries.safelist.require_id {
            FreeformGraphQLBehavior::DenyAll {
                log_unknown: config.persisted_queries.log_unknown,
            }
        } else {
            FreeformGraphQLBehavior::AllowIfInSafelist {
                safelist: FreeformGraphQLSafelist::new(new_manifest),
                log_unknown: config.persisted_queries.log_unknown,
            }
        }
    } else if config.persisted_queries.log_unknown {
        FreeformGraphQLBehavior::LogUnlessInSafelist {
            safelist: FreeformGraphQLSafelist::new(new_manifest),
            apq_enabled: config.apq.enabled,
        }
    } else {
        FreeformGraphQLBehavior::AllowAll {
            apq_enabled: config.apq.enabled,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::configuration::Apq;
    use crate::configuration::PersistedQueries;
    use crate::configuration::PersistedQueriesSafelist;
    use crate::services::layers::persisted_queries::manifest::ManifestOperation;

    #[test]
    fn safelist_body_normalization() {
        let safelist = FreeformGraphQLSafelist::new(&PersistedQueryManifest::from(vec![
            ManifestOperation {
                id: "valid-syntax".to_string(),
                body: "fragment A on T { a }    query SomeOp { ...A ...B }    fragment,,, B on U{b c  } # yeah".to_string(),
                client_name: None,
            },
            ManifestOperation {
                id: "invalid-syntax".to_string(),
                body: "}}}".to_string(),
                client_name: None,
            },
            ManifestOperation {
                id: "multiple-ops".to_string(),
                body: "query Op1 { a b } query Op2 { b a }".to_string(),
                client_name: None,
            },
        ]));

        let is_allowed = |body: &str| -> bool {
            safelist
                .get_pq_id_for_body(ast::Document::parse(body, "").as_ref().map_err(|_| body))
                .is_some()
        };

        // Precise string matches.
        assert!(is_allowed(
            "fragment A on T { a }    query SomeOp { ...A ...B }    fragment,,, B on U{b c  } # yeah"
        ));

        // Reordering definitions and reformatting a bit matches.
        assert!(is_allowed(
            "#comment\n  fragment, B on U  , { b    c }    query SomeOp {  ...A ...B }  fragment    \nA on T { a }"
        ));

        // Reordering operation definitions matches
        assert!(is_allowed("query Op2 { b a } query Op1 { a b }"));

        // Reordering fields does not match!
        assert!(!is_allowed(
            "fragment A on T { a }    query SomeOp { ...A ...B }    fragment,,, B on U{c b  } # yeah"
        ));

        // Documents with invalid syntax don't match...
        assert!(!is_allowed("}}}}"));

        // ... unless they precisely match a safelisted document that also has invalid syntax.
        assert!(is_allowed("}}}"));
    }

    fn freeform_behavior_from_pq_options(
        safe_list: bool,
        require_id: Option<bool>,
        log_unknown: Option<bool>,
    ) -> FreeformGraphQLBehavior {
        let manifest = &PersistedQueryManifest::from(vec![ManifestOperation {
            id: "valid-syntax".to_string(),
            body: "query SomeOp { a b }".to_string(),
            client_name: None,
        }]);

        let config = Configuration::builder()
            .persisted_query(
                PersistedQueries::builder()
                    .enabled(true)
                    .safelist(
                        PersistedQueriesSafelist::builder()
                            .enabled(safe_list)
                            .require_id(require_id.unwrap_or_default())
                            .build(),
                    )
                    .log_unknown(log_unknown.unwrap_or_default())
                    .build(),
            )
            .apq(Apq::fake_new(Some(false)))
            .build()
            .unwrap();
        get_freeform_graphql_behavior(&config, manifest)
    }

    #[test]
    fn test_get_freeform_graphql_behavior() {
        // safelist disabled
        assert!(matches!(
            freeform_behavior_from_pq_options(false, None, None),
            FreeformGraphQLBehavior::AllowAll { .. }
        ));

        // safelist disabled, log_unknown enabled
        assert!(matches!(
            freeform_behavior_from_pq_options(false, None, Some(true)),
            FreeformGraphQLBehavior::LogUnlessInSafelist { .. }
        ));

        // safelist enabled, id required
        assert!(matches!(
            freeform_behavior_from_pq_options(true, Some(true), None),
            FreeformGraphQLBehavior::DenyAll { .. }
        ));

        // safelist enabled, id not required
        assert!(matches!(
            freeform_behavior_from_pq_options(true, None, None),
            FreeformGraphQLBehavior::AllowIfInSafelist { .. }
        ));
    }
}
