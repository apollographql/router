use std::collections::HashMap;
use std::collections::HashSet;

use apollo_compiler::Name;
use apollo_compiler::ast::Directive;

use crate::error::CompositionError;
use crate::error::FederationError;
use crate::error::suggestion::did_you_mean;
use crate::error::suggestion::suggestion_list;
use crate::link::Link;
use crate::link::authenticated_spec_definition::AUTHENTICATED_DIRECTIVE_NAME_IN_SPEC;
use crate::link::federation_spec_definition::FEDERATION_CONTEXT_DIRECTIVE_NAME_IN_SPEC;
use crate::link::federation_spec_definition::FEDERATION_TAG_DIRECTIVE_NAME_IN_SPEC;
use crate::link::inaccessible_spec_definition::INACCESSIBLE_DIRECTIVE_NAME_IN_SPEC;
use crate::link::policy_spec_definition::POLICY_DIRECTIVE_NAME_IN_SPEC;
use crate::link::requires_scopes_spec_definition::REQUIRES_SCOPES_DIRECTIVE_NAME_IN_SPEC;
use crate::link::spec::APOLLO_SPEC_DOMAIN;
use crate::link::spec::Identity;
use crate::merger::error_reporter::ErrorReporter;
use crate::merger::hints::HintCode;
use crate::subgraph::typestate::HasMetadata;
use crate::subgraph::typestate::Subgraph;
use crate::supergraph::CompositionHint;

const DEFAULT_COMPOSED_DIRECTIVES: [Name; 6] = [
    FEDERATION_TAG_DIRECTIVE_NAME_IN_SPEC,
    INACCESSIBLE_DIRECTIVE_NAME_IN_SPEC,
    AUTHENTICATED_DIRECTIVE_NAME_IN_SPEC,
    REQUIRES_SCOPES_DIRECTIVE_NAME_IN_SPEC,
    POLICY_DIRECTIVE_NAME_IN_SPEC,
    FEDERATION_CONTEXT_DIRECTIVE_NAME_IN_SPEC,
];

pub(crate) struct ComposeDirectiveManager {
    merge_directive_map: HashMap<Name, HashSet<Name>>,
}

struct MergeDirectiveItem {
    subgraph_name: String,
    feature: Link,
    directive_name: Name,
    directive_name_as: Name,
    compose_directive: Directive,
}

impl ComposeDirectiveManager {
    pub(crate) fn new() -> Self {
        Self {
            merge_directive_map: HashMap::new(),
        }
    }

    pub(crate) fn should_compose_directive(
        &self,
        subgraph_name: &str,
        directive_name: &Name,
    ) -> bool {
        self.merge_directive_map
            .get(subgraph_name)
            .is_some_and(|set| set.contains(directive_name))
    }

    pub(crate) fn validate<T: HasMetadata>(
        subgraphs: &[Subgraph<T>],
        error_reporter: &mut ErrorReporter,
    ) -> Result<(), FederationError> {
        let mut seen_compose_directive_name: Option<&str> = None;
        let mut seen_compose_directive_identity: Option<Identity> = None;

        for subgraph in subgraphs {
            for application in subgraph.schema().compose_directive_applications()? {
                match application {
                    Ok(compose_directive) => {
                        // The parser will ensure this is not null, since `name` is defined as `String!`,
                        // but we still need to assert it is not empty.
                        if compose_directive.arguments.name.is_empty() {
                            error_reporter
                                .add_compose_directive_error_for_empty_name(subgraph.name.as_str());
                            continue;
                        }

                        // Ensure `name` has the proper directive format.
                        if !compose_directive.arguments.name.starts_with("@") {
                            error_reporter.add_compose_directive_error_for_missing_start_symbol(
                                compose_directive.arguments.name,
                                subgraph.name.as_str(),
                            );
                            continue;
                        }

                        // Ensure the directive being composed is defined
                        let name = compose_directive.arguments.name.split_at(1).1;
                        let Some(directive) =
                            subgraph.schema().schema().directive_definitions.get(name)
                        else {
                            error_reporter.add_compose_directive_error_for_undefined_directive(
                                compose_directive.arguments.name,
                                subgraph,
                            );
                            continue;
                        };

                        // Ensure the directive being composed is linked as a feature. This is almost
                        // certainly just a way to version the definitions, since we take the "latest"
                        // one when merging. Otherwise, it doesn't make much sense to force users to
                        // write a dummy URL, especially when we error/warn when used with any
                        // Federation directives.
                        let Some(feature) = subgraph
                            .schema()
                            .metadata()
                            .and_then(|links| links.source_link_of_directive(&directive.name))
                        else {
                            error_reporter.add_compose_directive_error_for_unrecognized_feature(
                                compose_directive.arguments.name,
                                subgraph.name.as_str(),
                            );
                            continue;
                        };

                        if feature.link.url.identity.domain == APOLLO_SPEC_DOMAIN
                            && DEFAULT_COMPOSED_DIRECTIVES.contains(&directive.name)
                        {
                            error_reporter
                                .add_compose_directive_hint_for_default_composed_directive(
                                    compose_directive.arguments.name,
                                );
                        } else if feature.link.url.identity.domain == APOLLO_SPEC_DOMAIN {
                            error_reporter.add_compose_directive_error_for_unsupported_directive(
                                compose_directive.arguments.name,
                                subgraph.name.as_str(),
                            );
                        } else if subgraph
                            .inaccessible_directive_name()?
                            // TODO: It seems like we'll never hit this case because we would have hit
                            // an Apollo link above
                            .is_some_and(|inaccessible| directive.name == inaccessible)
                        {
                            error_reporter.add_compose_directive_error_for_inaccessible_conflict(
                                compose_directive.arguments.name,
                                subgraph.name.as_str(),
                            );
                        } else if subgraph
                            .tag_directive_name()?
                            // TODO: It seems like we'll never hit this case because we would have hit
                            // an Apollo link above
                            .is_some_and(|tag| directive.name == tag)
                        {
                            error_reporter.add_compose_directive_error_for_tag_conflict(
                                compose_directive.arguments.name,
                                subgraph.name.as_str(),
                            );
                        }
                    }
                    Err(e) => e.into_errors().into_iter().for_each(|err| {
                        error_reporter.add_error(CompositionError::InternalError {
                            message: err.to_string(),
                        });
                    }),
                }
            }
        }

        Ok(())
    }
}

impl ErrorReporter {
    fn add_compose_directive_error_for_empty_name(&mut self, subgraph: &str) {
        self.add_error(CompositionError::DirectiveCompositionError {
            message: format!("Argument to @composeDirective in subgraph \"{subgraph}\" cannot be NULL or an empty String")
        });
    }

    fn add_compose_directive_error_for_missing_start_symbol(
        &mut self,
        invalid_name: &str,
        subgraph: &str,
    ) {
        self.add_error(CompositionError::DirectiveCompositionError {
            message: format!("Argument to @composeDirective \"{invalid_name}\" in subgraph \"{subgraph}\" must have a leading \"@\""),
        });
    }

    fn add_compose_directive_error_for_unrecognized_feature(&mut self, name: &str, subgraph: &str) {
        self.add_error(CompositionError::DirectiveCompositionError {
            message: format!("Directive \"{name}\" in subgraph \"{subgraph}\" cannot be composed because it is not a member of a core feature")
        });
    }

    fn add_compose_directive_hint_for_default_composed_directive(&mut self, name: &str) {
        self.add_hint(CompositionHint {
            code: HintCode::DirectiveCompositionInfo.code().to_string(),
            message: format!("Directive \"{name}\" should not be explicitly composed since it is a federation directive composed by default"),
            locations: vec![] // TODO: Add locations
        });
    }

    fn add_compose_directive_error_for_unsupported_directive(
        &mut self,
        name: &str,
        subgraph: &str,
    ) {
        self.add_error(CompositionError::DirectiveCompositionError {
            message: format!("Composing federation directive \"{name}\" in subgraph \"{subgraph}\" is not supported")
        });
    }

    fn add_compose_directive_error_for_tag_conflict(&mut self, name: &str, subgraph: &str) {
        self.add_error(CompositionError::DirectiveCompositionError {
            message: format!("Directive \"{name}\" in subgraph \"{subgraph}\" cannot be composed because it conflicts with the automatically composed federation directive \"@tag\""),
        });
    }

    fn add_compose_directive_error_for_inaccessible_conflict(
        &mut self,
        name: &str,
        subgraph: &str,
    ) {
        self.add_error(CompositionError::DirectiveCompositionError {
            message: format!("Directive \"{name}\" in subgraph \"{subgraph}\" cannot be composed because it conflicts with the automatically composed federation directive \"@inaccessible\""),
        });
    }

    fn add_compose_directive_error_for_undefined_directive<T: HasMetadata>(
        &mut self,
        name: &str,
        subgraph: &Subgraph<T>,
    ) {
        let words = suggestion_list(
            name,
            subgraph
                .schema()
                .schema()
                .directive_definitions
                .keys()
                .map(|d| format!("@{d}")),
        );
        self.add_error(CompositionError::DirectiveCompositionError {
            message: format!(
                "Could not find matching directive definition for argument to @composeDirective \"{name}\" in subgraph \"{}\".{}",
                subgraph.name,
                did_you_mean(words),
            ),
        });
    }
}
