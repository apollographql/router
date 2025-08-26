use std::collections::HashMap;
use std::collections::HashSet;
use std::sync::Arc;

use apollo_compiler::Name;
use apollo_compiler::ast::DirectiveDefinition;
use multimap::MultiMap;

use crate::error::CompositionError;
use crate::error::FederationError;
use crate::error::suggestion::did_you_mean;
use crate::error::suggestion::suggestion_list;
use crate::link::Link;
use crate::link::LinkedElement;
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
use crate::schema::ComposeDirectiveDirective;
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
    /// Map of subgraphs to directives being composed
    merge_directive_map: HashMap<String, HashSet<Name>>,
    /// Map of identities to the `Link` with latest version and the subgraph where it is applied
    latest_feature_map: HashMap<Identity, (Arc<Link>, String)>,
    /// Map of directive names to link identity and the directive's unaliased name
    directive_identity_map: HashMap<Name, (Identity, Name)>,
}

#[derive(Clone)]
struct MergeDirectiveItem<'a> {
    subgraph_name: String,
    compose_directive: ComposeDirectiveDirective<'a>,
    link: LinkedElement,
}

impl MergeDirectiveItem<'_> {
    fn identity(&self) -> &Identity {
        &self.link.link.url.identity
    }

    fn original_directive_name(&self) -> &Name {
        self.link
            .import
            .as_ref()
            .map(|i| i.element.as_ref())
            .unwrap_or_else(|| self.link.link.spec_name_in_schema())
    }

    fn aliased_directive_name(&self) -> &Name {
        self.link
            .import
            .as_ref()
            .map(|i| i.imported_name())
            .or_else(|| self.link.link.spec_alias.as_ref())
            .unwrap_or_else(|| self.link.link.spec_name_in_schema())
    }
}

impl ComposeDirectiveManager {
    pub(crate) fn new() -> Self {
        Self {
            merge_directive_map: HashMap::new(),
            latest_feature_map: HashMap::new(),
            directive_identity_map: HashMap::new(),
        }
    }

    pub(crate) fn all_composed_core_features(&self) -> Vec<LinkedElement> {
        todo!("Implement all_composed_core_features")
    }

    pub(crate) fn directive_exists_in_supergraph(&self, directive_name: &Name) -> bool {
        self.directive_identity_map.contains_key(directive_name)
    }

    pub(crate) fn get_latest_directive_definition(
        &self,
        directive_name: &Name,
    ) -> Result<Option<DirectiveDefinition>, FederationError> {
        todo!("Implement get_latest_directive_definition")
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
        &mut self,
        subgraphs: &[Subgraph<T>],
        error_reporter: &mut ErrorReporter,
    ) -> Result<(), FederationError> {
        let mut wont_merge_features = HashSet::new();
        let mut wont_merge_directive_names = HashSet::new();
        let mut items_by_subgraph: MultiMap<String, MergeDirectiveItem> = MultiMap::new();
        let mut items_by_identity: MultiMap<Identity, MergeDirectiveItem> = MultiMap::new();
        let mut items_by_directive_name: MultiMap<Name, MergeDirectiveItem> = MultiMap::new();
        let mut items_by_orig_directive_name: MultiMap<Name, MergeDirectiveItem> = MultiMap::new();

        let tag_names_in_subgraphs: MultiMap<Name, String> = subgraphs
            .iter()
            .filter_map(|s| {
                s.tag_directive_name()
                    .ok()
                    .flatten()
                    .zip(Some(s.name.clone()))
            })
            .collect();
        let inaccessible_names_in_subgraphs: MultiMap<Name, String> = subgraphs
            .iter()
            .filter_map(|s| {
                s.inaccessible_directive_name()
                    .ok()
                    .flatten()
                    .zip(Some(s.name.clone()))
            })
            .collect();

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

                        // Ensure the directive does not conflict with a federation directive. For
                        // directives like `@tag` which are composed by default, we raise a hint
                        // to say that applying `@composeDirective(name: "@tag")` is redundant.
                        // Note that check for conflicts across subgraphs to make sure that we
                        // don't compose a custom `@tag` directive with the federation one, if it
                        // hasn't been properly renamed in another subgraph.
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
                        } else if let Some(subgraphs_with_conflict) =
                            inaccessible_names_in_subgraphs.get_vec(&directive.name)
                        {
                            error_reporter.add_compose_directive_error_for_inaccessible_conflict(
                                compose_directive.arguments.name,
                                subgraph.name.as_str(),
                                subgraphs_with_conflict,
                            );
                        } else if let Some(subgraphs_with_conflict) =
                            tag_names_in_subgraphs.get_vec(&directive.name)
                        {
                            error_reporter.add_compose_directive_error_for_tag_conflict(
                                compose_directive.arguments.name,
                                subgraph.name.as_str(),
                                subgraphs_with_conflict,
                            );
                        } else {
                            let item = MergeDirectiveItem {
                                subgraph_name: subgraph.name.clone(),
                                compose_directive: compose_directive.clone(),
                                link: feature.clone(),
                            };
                            items_by_subgraph.insert(subgraph.name.clone(), item.clone());
                            items_by_identity
                                .insert(feature.link.url.identity.clone(), item.clone());
                            items_by_directive_name.insert(directive.name.clone(), item.clone());
                            items_by_orig_directive_name
                                .insert(item.original_directive_name().clone(), item);
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

        // Find the latest version for each imported feature. If we find a major version mismatch,
        // add this feature to the list of features that won't be merged.
        for (identity, linked_elements) in items_by_identity.iter_all() {
            for linked_element in linked_elements {
                let latest = self.latest_feature_map.entry(identity.clone()).or_insert((
                    linked_element.link.link.clone(),
                    linked_element.subgraph_name.clone(),
                ));
                if linked_element
                    .link
                    .link
                    .url
                    .version
                    .satisfies(&latest.0.url.version)
                {
                    *latest = (
                        linked_element.link.link.clone(),
                        linked_element.subgraph_name.clone(),
                    );
                } else {
                    wont_merge_features.insert(identity.clone());
                }
            }
        }

        // Ensure the composed directives refer to the same directive from the same spec in every subgraph
        for (name, items) in items_by_directive_name.iter_all() {
            if !Self::all_elements_equal(items, |item| item.original_directive_name()) {
                wont_merge_directive_names.insert(name.clone());
                error_reporter.add_error(CompositionError::DirectiveCompositionError {
                    message: format!("Composed directive \"@{name}\" does not refer to the same directive in every subgraph"),
                });
            }
            if !Self::all_elements_equal(items, |item| item.identity()) {
                wont_merge_directive_names.insert(name.clone());
                error_reporter.add_error(CompositionError::DirectiveCompositionError {
                    message: format!("Composed directive \"@{name}\" is not linked by the same core feature in every subgraph"),
                });
            }
        }

        // Ensure each composed directive is exported with the same name in every subgraph
        for (name, items) in items_by_orig_directive_name.iter_all() {
            if !Self::all_elements_equal(items, |item| item.aliased_directive_name()) {
                for item in items {
                    wont_merge_directive_names.insert(item.original_directive_name().clone());
                }
                error_reporter
                    .add_compose_directive_error_for_inconsistent_imports(name, subgraphs);
            } else {
                let subgraphs_exporting_this_directive = items
                    .iter()
                    .map(|i| i.subgraph_name.clone())
                    .collect::<HashSet<_>>();
                for subgraph in subgraphs {
                    if subgraphs_exporting_this_directive.contains(&subgraph.name) {
                        continue;
                    }
                    // TODO: If this subgraph imports this directive under a different alias, we
                    // should push a hint
                }
            }
        }

        // For anything which hasn't been ruled out, add it to the map of directives to be merged
        for (subgraph, items) in items_by_subgraph.iter_all() {
            let mut directives_for_subgraph = HashSet::with_capacity(items.len());
            for item in items {
                if !wont_merge_features.contains(item.identity())
                    && !wont_merge_directive_names.contains(item.aliased_directive_name())
                {
                    directives_for_subgraph.insert(item.aliased_directive_name().clone());
                }
                self.directive_identity_map.insert(
                    item.aliased_directive_name().clone(),
                    (
                        item.identity().clone(),
                        item.original_directive_name().clone(),
                    ),
                );
            }
            self.merge_directive_map
                .insert(subgraph.clone(), directives_for_subgraph);
        }

        Ok(())
    }

    fn all_elements_equal<'a, T: PartialEq>(
        items: &'a [MergeDirectiveItem],
        select: impl Fn(&'a MergeDirectiveItem) -> T,
    ) -> bool {
        let mut value = None;
        for item in items {
            let item_value = select(item);
            if let Some(value) = &value {
                if *value != item_value {
                    return false;
                }
            } else {
                value = Some(item_value);
            }
        }
        true
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

    fn add_compose_directive_error_for_tag_conflict(
        &mut self,
        name: &str,
        subgraph: &str,
        subgraphs_with_conflict: &[String],
    ) {
        self.add_error(CompositionError::DirectiveCompositionError {
            message: format!(
                "Directive \"{name}\" in subgraph \"{subgraph}\" cannot be composed because it conflicts with the automatically composed federation directive \"@tag\". Conflict exists in subgraph(s): ({})",
                subgraphs_with_conflict.join(",")
            )
        });
    }

    fn add_compose_directive_error_for_inaccessible_conflict(
        &mut self,
        name: &str,
        subgraph: &str,
        subgraphs_with_conflict: &[String],
    ) {
        self.add_error(CompositionError::DirectiveCompositionError {
            message: format!(
                "Directive \"{name}\" in subgraph \"{subgraph}\" cannot be composed because it conflicts with the automatically composed federation directive \"@inaccessible\". Conflict exists in subgraph(s): ({})",
                subgraphs_with_conflict.join(",")
            ),
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

    fn add_compose_directive_error_for_inconsistent_imports<T: HasMetadata>(
        &mut self,
        name: &str,
        subgraphs: &[Subgraph<T>],
    ) {
        todo!()
    }

    fn add_compose_directive_hint_for_inconsistent_imports<T: HasMetadata>(
        &mut self,
        name: &str,
        subgraphs: &[Subgraph<T>],
    ) {
        todo!()
    }
}
