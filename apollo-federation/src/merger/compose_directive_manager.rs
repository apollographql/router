use std::sync::Arc;

use apollo_compiler::Name;
use apollo_compiler::Node;
use apollo_compiler::ast::DirectiveDefinition;
use indexmap::IndexMap;
use indexmap::IndexSet;

use crate::error::CompositionError;
use crate::error::FederationError;
use crate::error::HasLocations;
use crate::error::SingleFederationError;
use crate::error::SubgraphLocation;
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
use crate::subgraph::typestate::HasMetadata;
use crate::subgraph::typestate::Subgraph;
use crate::supergraph::CompositionHint;
use crate::utils::MultiIndexMap as MultiMap;

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
    merge_directive_map: IndexMap<String, IndexSet<Name>>,
    /// Map of (original) directive name to the latest definition found across subgraphs
    latest_directive_definition_map: IndexMap<Name, DirectiveDefinition>,
    /// Map of identities to the `Link` with latest version and all subgraph names at that version
    latest_feature_map: IndexMap<Identity, (Arc<Link>, Vec<String>)>,
    /// Map of identities to the list of directives imported from that identity across all
    /// subgraphs. The directive names are recorded in a map of original name to aliased name.
    directives_for_feature_map: IndexMap<Identity, IndexMap<Name, Name>>,
}

#[derive(Clone)]
pub(crate) struct MergeDirectiveItem {
    subgraph_name: String,
    pub(crate) definition: DirectiveDefinition,
    link: LinkedElement,
    original_name: Name,
}

impl MergeDirectiveItem {
    fn new(subgraph_name: String, definition: DirectiveDefinition, link: LinkedElement) -> Self {
        let original_name = if let Some(import) = &link.import {
            import.element.clone()
        } else {
            let spec_name = link.link.spec_name_in_schema();
            let directive_name = &definition.name;

            if let Some(suffix) = directive_name
                .as_str()
                .strip_prefix(spec_name.as_str())
                .and_then(|s| s.strip_prefix("__"))
            {
                Name::new(suffix).unwrap_or_else(|_| directive_name.clone())
            } else if directive_name == spec_name {
                // The directive name matches the spec's name-in-schema (the "default"
                // spec directive, which inherits the spec alias when the spec is aliased
                // via `@link(as: "...")`). Its canonical name within the spec is the
                // URL's identity name — not the alias.
                link.link.url.identity.name.clone()
            } else {
                directive_name.clone()
            }
        };

        Self {
            subgraph_name,
            definition,
            link,
            original_name,
        }
    }

    fn identity(&self) -> &Identity {
        &self.link.link.url.identity
    }

    fn original_directive_name(&self) -> &Name {
        &self.original_name
    }

    fn aliased_directive_name(&self) -> &Name {
        self.link
            .import
            .as_ref()
            .map(|i| i.imported_name())
            .or_else(|| self.link.link.spec_alias.as_ref())
            .unwrap_or(&self.definition.name)
    }
}

impl std::fmt::Display for MergeDirectiveItem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.aliased_directive_name())
    }
}

#[allow(dead_code)]
impl ComposeDirectiveManager {
    pub(crate) fn new() -> Self {
        Self {
            merge_directive_map: Default::default(),
            latest_directive_definition_map: Default::default(),
            latest_feature_map: Default::default(),
            directives_for_feature_map: Default::default(),
        }
    }

    pub(crate) fn all_composed_core_features(&self) -> Vec<(Arc<Link>, IndexMap<Name, Name>)> {
        self.latest_feature_map
            .iter()
            .filter_map(|(identity, (link, _))| {
                if identity.domain == APOLLO_SPEC_DOMAIN {
                    None
                } else {
                    Some((
                        link.clone(),
                        self.directives_for_feature_map
                            .get(identity)
                            .cloned()
                            .unwrap_or_default(),
                    ))
                }
            })
            .collect()
    }

    pub(crate) fn directive_exists_in_supergraph(&self, directive_name: &Name) -> bool {
        self.latest_directive_definition_map
            .contains_key(directive_name)
    }

    pub(crate) fn has_latest_directive_definition(&self, directive_name: &Name) -> bool {
        self.latest_directive_definition_map
            .contains_key(directive_name)
    }

    pub(crate) fn composed_directive_names(&self) -> impl Iterator<Item = &Name> {
        self.latest_directive_definition_map.keys()
    }

    /// Returns the latest definition found across subgraphs for the given directive name. It
    /// expects the name to be the aliased name of the directive as it appears in the schema.
    pub(crate) fn get_latest_directive_definition(
        &self,
        directive_name: &Name,
    ) -> Result<Option<Node<DirectiveDefinition>>, FederationError> {
        Ok(self
            .latest_directive_definition_map
            .get(directive_name)
            .map(|d| Node::new(d.clone())))
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
        let mut wont_merge_features: IndexSet<_> = Default::default();
        let mut wont_merge_directive_names: IndexSet<_> = Default::default();
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
            let Ok(compose_directive_applications) =
                subgraph.schema().compose_directive_applications()
            else {
                continue;
            };
            for application in compose_directive_applications {
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
                        // Note that we check for conflicts across subgraphs to make sure that we
                        // don't compose a custom `@tag` directive with the federation one, if it
                        // hasn't been properly renamed in another subgraph.
                        let original_directive_name = feature
                            .import
                            .as_ref()
                            .map(|import| &import.element)
                            .unwrap_or(&directive.name);
                        if feature.link.url.identity.domain == APOLLO_SPEC_DOMAIN
                            && DEFAULT_COMPOSED_DIRECTIVES.contains(original_directive_name)
                        {
                            error_reporter
                                .add_compose_directive_hint_for_default_composed_directive(
                                    compose_directive.arguments.name,
                                    directive.locations(subgraph),
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
                            let item = MergeDirectiveItem::new(
                                subgraph.name.clone(),
                                directive.as_ref().clone(),
                                feature.clone(),
                            );
                            items_by_subgraph.insert(subgraph.name.clone(), item.clone());
                            items_by_identity
                                .insert(feature.link.url.identity.clone(), item.clone());
                            items_by_directive_name.insert(directive.name.clone(), item.clone());
                            items_by_orig_directive_name
                                .insert(item.original_directive_name().clone(), item);
                        }
                    }
                    Err(FederationError::SingleFederationError(
                        SingleFederationError::Internal { message },
                    )) if message.as_str()
                        == "Required argument \"name\" of directive \"@composeDirective\" was not present." =>
                    {
                        error_reporter
                            .add_compose_directive_error_for_empty_name(subgraph.name.as_str());
                    }
                    Err(e) => e.into_errors().into_iter().for_each(|err| {
                        error_reporter.add_error(CompositionError::InternalError {
                            message: err.to_string(),
                        });
                    }),
                }
            }
        }

        // Build a map of all core features used across subgraphs (even non-composed ones)
        let mut all_features_by_identity: MultiMap<Identity, (Arc<Link>, String)> = MultiMap::new();
        for subgraph in subgraphs {
            if let Some(links) = subgraph.schema().metadata() {
                for link in &links.links {
                    all_features_by_identity.insert(
                        link.url.identity.clone(),
                        (link.clone(), subgraph.name.clone()),
                    );
                }
            }
        }

        // Check for major version mismatches in ALL features (composed and non-composed)
        // For non-composed features with mismatches, emit a hint
        for (identity, features) in all_features_by_identity.iter_all() {
            if identity.domain == APOLLO_SPEC_DOMAIN {
                continue;
            }

            let mut major_versions: IndexSet<_> = Default::default();
            for (link, _) in features {
                major_versions.insert(link.url.version.major);
            }

            if major_versions.len() > 1 {
                // Check if this feature is being composed
                let is_composed = items_by_identity.contains_key(identity);

                if is_composed {
                    error_reporter.add_error(CompositionError::DirectiveCompositionError {
                        message: format!(
                            "Core feature \"{}\" requested to be merged has major version mismatch across subgraphs",
                            identity
                        )
                    });
                    wont_merge_features.insert(identity.clone());
                } else {
                    error_reporter.add_hint(CompositionHint {
                        code: HintCode::DirectiveCompositionInfo.code().to_string(),
                        message: format!("Non-composed core feature \"{}\" has major version mismatch across subgraphs", identity),
                        locations: vec![],
                    });
                }
            }
        }

        // Find the latest version and all subgraphs at that version for each composed feature.
        // Directive definitions are sourced exclusively from subgraphs at the latest version:
        // if a subgraph at an older (but still compatible) minor version composes a directive
        // whose definition is absent from every latest-version subgraph, composition fails with
        // an explicit error rather than silently using an outdated definition.
        for (identity, linked_elements) in items_by_identity.iter_all() {
            if wont_merge_features.contains(identity) {
                continue;
            }

            // Step 1: find the highest minor version among all composed items for this identity.
            // Major version mismatches were already caught above, so all elements share the same
            // major version here.
            let Some(latest_minor) = linked_elements
                .iter()
                .map(|item| item.link.link.url.version.minor)
                .max()
            else {
                continue;
            };

            // Step 2: collect all subgraph names (and a representative link) at the latest version.
            let mut latest_link: Option<Arc<Link>> = None;
            let mut latest_subgraph_names: Vec<String> = Vec::new();
            for item in linked_elements.iter() {
                if item.link.link.url.version.minor == latest_minor {
                    if latest_link.is_none() {
                        latest_link = Some(item.link.link.clone());
                    }
                    if !latest_subgraph_names.contains(&item.subgraph_name) {
                        latest_subgraph_names.push(item.subgraph_name.clone());
                    }
                }
            }
            let latest_link = latest_link.unwrap(); // safe: max() succeeded above

            self.latest_feature_map.insert(
                identity.clone(),
                (latest_link, latest_subgraph_names.clone()),
            );

            // Step 3: build the set of (original_name → aliased_name) pairs being composed from
            // this identity across all versions, then find a definition for each from a
            // latest-version subgraph.
            let mut composed_directive_pairs: IndexMap<Name, Name> = IndexMap::new();
            for item in linked_elements.iter() {
                composed_directive_pairs
                    .entry(item.original_directive_name().clone())
                    .or_insert_with(|| item.aliased_directive_name().clone());
            }

            // Collect definitions before committing to maps so that a single failure for any
            // directive in this identity causes the entire identity to be marked wont_merge
            // (matching the TypeScript behavior) without leaving partial entries behind.
            let mut pending_definitions: Vec<(Name, DirectiveDefinition)> = Vec::new();
            let mut pending_identity_directives: IndexMap<Name, Name> = IndexMap::new();
            let mut identity_has_error = false;

            for (original_name, aliased_name) in &composed_directive_pairs {
                // First try: a latest-version item already carries the definition (the subgraph
                // actively composes this directive at the latest version).
                let definition = linked_elements
                    .iter()
                    .filter(|item| {
                        item.link.link.url.version.minor == latest_minor
                            && item.original_directive_name() == original_name
                    })
                    .map(|item| item.definition.clone())
                    .next()
                    // Second try: the subgraph is at the latest version but does not itself
                    // compose this directive — look directly in its schema. This covers the case
                    // where a later-version subgraph defines a directive from an older subgraph's
                    // composed set, allowing the definition to be safely lifted to the supergraph.
                    .or_else(|| {
                        latest_subgraph_names.iter().find_map(|sg_name| {
                            let subgraph = subgraphs.iter().find(|s| s.name == *sg_name)?;
                            subgraph
                                .schema()
                                .schema()
                                .directive_definitions
                                .get(aliased_name.as_str())
                                .map(|d| d.as_ref().clone())
                        })
                    });

                match definition {
                    Some(def) => {
                        pending_definitions.push((aliased_name.clone(), def));
                        pending_identity_directives
                            .insert(original_name.clone(), aliased_name.clone());
                    }
                    None => {
                        identity_has_error = true;
                        wont_merge_features.insert(identity.clone());
                        let plural = if latest_subgraph_names.len() == 1 {
                            ""
                        } else {
                            "s"
                        };
                        let do_does = if latest_subgraph_names.len() == 1 {
                            "does"
                        } else {
                            "do"
                        };
                        let names = latest_subgraph_names
                            .iter()
                            .map(|s| format!("\"{s}\""))
                            .collect::<Vec<_>>()
                            .join(", ");
                        error_reporter.add_error(CompositionError::DirectiveCompositionError {
                            message: format!(
                                "Core feature \"{identity}\" in subgraph{plural} \
                                 ({names}) {do_does} not have a directive definition \
                                 for \"@{aliased_name}\""
                            ),
                        });
                    }
                }
            }

            if !identity_has_error {
                for (name, def) in pending_definitions {
                    self.latest_directive_definition_map.insert(name, def);
                }
                self.directives_for_feature_map
                    .insert(identity.clone(), pending_identity_directives);
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
                    .add_compose_directive_error_for_inconsistent_imports(items, subgraphs);
            } else {
                let subgraphs_exporting_this_directive = items
                    .iter()
                    .map(|i| i.subgraph_name.clone())
                    .collect::<IndexSet<_>>();
                for subgraph in subgraphs {
                    if subgraphs_exporting_this_directive.contains(&subgraph.name) {
                        continue;
                    }
                    let Some(links) = subgraph.schema().metadata() else {
                        continue;
                    };
                    if links
                        .directives_by_original_name
                        .get(name)
                        .is_some_and(|(_, import)| {
                            import.imported_name() != items[0].aliased_directive_name()
                        })
                    {
                        error_reporter.add_hint(CompositionHint {
                            code: HintCode::DirectiveCompositionWarn.code().to_string(),
                            message: format!("Composed directive \"@{name}\" is named differently in a subgraph that doesn't export it. Consistent naming will be required to export it."),
                            locations: vec![],
                        });
                        break;
                    }
                }
            }
        }

        // For anything which hasn't been ruled out, add it to the map of directives to be merged
        for (subgraph, items) in items_by_subgraph.iter_all() {
            let mut directives_for_subgraph = IndexSet::with_capacity(items.len());
            for item in items {
                if !wont_merge_features.contains(item.identity())
                    && !wont_merge_directive_names.contains(item.aliased_directive_name())
                {
                    directives_for_subgraph.insert(item.aliased_directive_name().clone());
                }
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

    fn add_compose_directive_hint_for_default_composed_directive(
        &mut self,
        name: &str,
        locations: Vec<SubgraphLocation>,
    ) {
        self.add_hint(CompositionHint {
            code: HintCode::DirectiveCompositionInfo.code().to_string(),
            message: format!("Directive \"{name}\" should not be explicitly manually composed since it is a federation directive composed by default"),
            locations,
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
                "Directive \"{name}\" in subgraph \"{subgraph}\" cannot be composed because it conflicts with automatically composed federation directive \"@tag\". Conflict exists in subgraph(s): ({})",
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
                "Directive \"{name}\" in subgraph \"{subgraph}\" cannot be composed because it conflicts with automatically composed federation directive \"@inaccessible\". Conflict exists in subgraph(s): ({})",
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
        items: &[MergeDirectiveItem],
        subgraphs: &[Subgraph<T>],
    ) {
        let sources = subgraphs
            .iter()
            .enumerate()
            .map(|(idx, subgraph)| {
                let item_in_this_subgraph = items
                    .iter()
                    .find(|item| item.subgraph_name == subgraph.name);
                (idx, item_in_this_subgraph.cloned())
            })
            .collect();
        self.report_mismatch_error_without_supergraph(
            CompositionError::DirectiveCompositionError {
                message: "Composed directive is not named consistently in all subgraphs"
                    .to_string(),
            },
            &sources,
            subgraphs,
            |elt, _| Some(format!("\"@{}\"", elt.aliased_directive_name())),
        );
    }
}
