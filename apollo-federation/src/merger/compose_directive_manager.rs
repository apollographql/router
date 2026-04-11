use std::borrow::Cow;
use std::sync::Arc;

use apollo_compiler::Name;
use apollo_compiler::ast::DirectiveDefinition;
use indexmap::IndexMap;
use indexmap::IndexSet;

use crate::bail;
use crate::error::CompositionError;
use crate::error::FederationError;
use crate::error::HasLocations;
use crate::error::Locations;
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
use crate::schema::position::DirectiveDefinitionPosition;
use crate::subgraph::typestate::HasMetadata;
use crate::subgraph::typestate::Subgraph;
use crate::supergraph::CompositionHint;
use crate::utils::MultiIndexMap as MultiMap;
use crate::utils::human_readable::human_readable_subgraph_names;

const DEFAULT_COMPOSED_DIRECTIVES: [Name; 6] = [
    FEDERATION_TAG_DIRECTIVE_NAME_IN_SPEC,
    INACCESSIBLE_DIRECTIVE_NAME_IN_SPEC,
    AUTHENTICATED_DIRECTIVE_NAME_IN_SPEC,
    REQUIRES_SCOPES_DIRECTIVE_NAME_IN_SPEC,
    POLICY_DIRECTIVE_NAME_IN_SPEC,
    FEDERATION_CONTEXT_DIRECTIVE_NAME_IN_SPEC,
];

pub(crate) struct ComposeDirectiveManager {
    /// Map of subgraphs to directives that should be composed in that subgraph; note this will not
    /// include directives for
    merge_directive_map: IndexMap<String, IndexSet<Name>>,
    /// Map of identities to the `Link` with latest version and all subgraph names at that version.
    latest_feature_map: IndexMap<Identity, (Arc<Link>, IndexSet<String>)>,
    /// Map from directives that should be composed to their spec's identity and their name in the
    /// spec (a.k.a. the original name). Note that when validation fails, this map may still contain
    /// entries for @composeDirective directives that have errored. This has the effect of ensuring
    /// directive definition merging, when there's a non-composed directive definition with the same
    /// name, will still attempt to use a definition from the appropriate spec in the face of these
    /// errors.
    directive_identity_map: IndexMap<Name, (Identity, Name)>,
    /// Inverse of the [directive_identity_map] above, mapping merged identities to another map from
    /// the directive's name in schema to its name in spec (a.k.a. the original name).
    identity_directive_map: IndexMap<Identity, IndexMap<Name, Name>>,
}

#[derive(Clone)]
pub(crate) struct MergeDirectiveItem {
    subgraph_name: String,
    pub(crate) definition: DirectiveDefinition,
    link: LinkedElement,
}

impl MergeDirectiveItem {
    fn new(subgraph_name: String, definition: DirectiveDefinition, link: LinkedElement) -> Self {
        Self {
            subgraph_name,
            definition,
            link,
        }
    }

    fn identity(&self) -> &Identity {
        &self.link.link.url.identity
    }

    fn directive_name_in_spec(&self) -> &Name {
        &self.link.name_in_spec
    }

    fn directive_name(&self) -> &Name {
        &self.link.name
    }

    fn directive_has_different_name_in_subgraph<T: HasMetadata>(
        &self,
        subgraph: &Subgraph<T>,
    ) -> bool {
        let Some(metadata) = subgraph.schema().metadata() else {
            return false;
        };
        let Some(link) = metadata.for_identity(&self.link.link.url.identity) else {
            return false;
        };
        let Some(imp) = link
            .imports
            .iter()
            .find(|i| i.is_directive && &i.element == self.directive_name_in_spec())
        else {
            return false;
        };
        let name_in_subgraph = imp.alias.as_ref().unwrap_or(&imp.element);
        name_in_subgraph != self.directive_name()
    }
}

impl std::fmt::Display for MergeDirectiveItem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.directive_name())
    }
}

type DirectiveImportSpecNamesByAlias<'a> = Cow<'a, IndexMap<Name, Name>>;

#[allow(dead_code)]
impl ComposeDirectiveManager {
    pub(crate) fn new() -> Self {
        Self {
            merge_directive_map: Default::default(),
            latest_feature_map: Default::default(),
            directive_identity_map: Default::default(),
            identity_directive_map: Default::default(),
        }
    }

    /// Returns all non-Apollo links/features that should be composed via @composeDirective,
    /// specifically an example of a subgraph link with the correct identity/version paired with a
    /// map of directive names in supergraph schema to their names in spec (a.k.a. original name).
    pub(crate) fn all_composed_core_features(
        &self,
    ) -> Vec<(Arc<Link>, DirectiveImportSpecNamesByAlias<'_>)> {
        self.latest_feature_map
            .iter()
            .filter_map(|(identity, (link, _))| {
                if identity.domain == APOLLO_SPEC_DOMAIN {
                    None
                } else {
                    Some((
                        link.clone(),
                        self.identity_directive_map
                            .get(identity)
                            .map(Cow::Borrowed)
                            .unwrap_or_default(),
                    ))
                }
            })
            .collect()
    }

    pub(crate) fn directive_exists_in_supergraph(&self, directive_name: &Name) -> bool {
        self.directive_identity_map.contains_key(directive_name)
    }

    /// If the given directive name uses @composeDirective in some subgraph, this returns the name
    /// of the subgraph containing the definition for the version of the spec that will be embedded
    /// in the supergraph schema (this will be the latest spec version among subgraphs that use
    /// @composeDirective for the spec). Note this potentially may be from a subgraph that doesn't
    /// use @composeDirective for the spec (the subgraph just needs to have the right spec version
    /// and the definition present). Also note the name of this directive may be different from the
    /// one given due to aliasing, and accordingly we also return the position in the subgraph.
    pub(crate) fn get_latest_directive_definition<T: HasMetadata>(
        &self,
        directive_name: &Name,
        subgraphs: &[Subgraph<T>],
        error_reporter: &mut ErrorReporter,
    ) -> Result<Option<(String, DirectiveDefinitionPosition)>, FederationError> {
        let Some((identity, name_in_spec)) = self.directive_identity_map.get(directive_name) else {
            return Ok(None);
        };
        let Some((link, subgraph_names)) = self.latest_feature_map.get(identity) else {
            bail!("link feature identity must exist in map");
        };
        for subgraph in subgraphs {
            if !subgraph_names.contains(&subgraph.name) {
                continue;
            }
            // We need to convert from the name that is used in the schema(s) that export the
            // directive via @composeDirective to the name used in the schema that contains the
            // definition for the latest version (which may or may not export). See the test
            // "exported directive not imported everywhere. imported with different name".
            let Some(metadata) = subgraph.schema().metadata() else {
                continue;
            };
            let Some(link) = metadata.for_identity(identity) else {
                continue;
            };
            let name_in_schema = link.directive_name_in_schema(name_in_spec);
            let Some(def) = subgraph.schema().get_directive_definition(&name_in_schema) else {
                continue;
            };
            return Ok(Some((subgraph.name.clone(), def)));
        }
        let plural = subgraph_names.len() != 1;
        error_reporter.add_error(CompositionError::DirectiveCompositionError {
            message: format!(
                "Core feature \"{}/v{}\" in {} {} not have a directive definition for \"@{}\"",
                identity,
                link.url.version,
                human_readable_subgraph_names(subgraph_names.iter()),
                if plural { "do" } else { "does" },
                directive_name,
            ),
        });
        Ok(None)
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
        let mut items_by_directive_name: MultiMap<Name, MergeDirectiveItem> = MultiMap::new();
        let mut items_by_directive_name_in_spec: MultiMap<Name, MergeDirectiveItem> =
            MultiMap::new();

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
                        if feature.link.url.identity.domain == APOLLO_SPEC_DOMAIN
                            && DEFAULT_COMPOSED_DIRECTIVES.contains(&feature.name_in_spec)
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
                            tag_names_in_subgraphs.get_vec(&directive.name)
                        {
                            error_reporter.add_compose_directive_error_for_tag_conflict(
                                compose_directive.arguments.name,
                                subgraph.name.as_str(),
                                subgraphs_with_conflict,
                            );
                        } else if let Some(subgraphs_with_conflict) =
                            inaccessible_names_in_subgraphs.get_vec(&directive.name)
                        {
                            error_reporter.add_compose_directive_error_for_inaccessible_conflict(
                                compose_directive.arguments.name,
                                subgraph.name.as_str(),
                                subgraphs_with_conflict,
                            );
                        } else {
                            let item = MergeDirectiveItem::new(
                                subgraph.name.clone(),
                                directive.as_ref().clone(),
                                feature,
                            );
                            items_by_subgraph.insert(subgraph.name.clone(), item.clone());
                            items_by_directive_name.insert(directive.name.clone(), item.clone());
                            items_by_directive_name_in_spec
                                .insert(item.directive_name_in_spec().to_owned(), item);
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

        // Build a set of all identities across subgraphs. Note that in order to ensure that we
        // properly hint or error when there is a major version incompatibility, it's important that
        // we examine all core features, even if the directives within them will not be composed via
        // @composeDirective.
        let all_identities: IndexSet<&Identity> = subgraphs
            .iter()
            .filter_map(|subgraph| {
                Some(
                    subgraph
                        .schema()
                        .metadata()?
                        .links
                        .iter()
                        .map(|link| &link.url.identity),
                )
            })
            .flatten()
            .collect();

        for identity in all_identities {
            let subgraphs_used = subgraphs
                .iter()
                .filter_map(|subgraph| {
                    let items = items_by_subgraph.get(&subgraph.name)?;
                    if items.iter().any(|item| item.identity() == identity) {
                        Some(subgraph.name.clone())
                    } else {
                        None
                    }
                })
                .collect();
            if let Some(latest) =
                Self::get_latest_if_compatible(identity, subgraphs, &subgraphs_used, error_reporter)
            {
                self.latest_feature_map.insert(identity.clone(), latest);
            } else {
                wont_merge_features.insert(identity);
            }
        }

        // Ensure the composed directives refer to the same directive from the same spec in every subgraph
        for (name, items) in items_by_directive_name.iter_all() {
            if !Self::all_elements_equal(items, |item| item.directive_name_in_spec()) {
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
        for (name, items) in items_by_directive_name_in_spec.iter_all() {
            if !Self::all_elements_equal(items, |item| item.directive_name()) {
                for item in items {
                    wont_merge_directive_names.insert(item.directive_name().clone());
                }
                error_reporter
                    .add_compose_directive_error_for_inconsistent_imports(items, subgraphs);
            }
            // Also check that subgraphs that don't export the directive don't have inconsistent naming.
            let Some(item) = items.first() else {
                continue;
            };
            let subgraphs_exporting_this_directive = items
                .iter()
                .map(|i| i.subgraph_name.clone())
                .collect::<IndexSet<_>>();
            let subgraphs_with_different_naming: Vec<_> = subgraphs
                .iter()
                .filter(|subgraph| {
                    if subgraphs_exporting_this_directive.contains(&subgraph.name) {
                        return false;
                    }
                    item.directive_has_different_name_in_subgraph(subgraph)
                })
                .collect();
            if !subgraphs_with_different_naming.is_empty() {
                error_reporter.add_hint(CompositionHint {
                    code: HintCode::DirectiveCompositionWarn.code().to_string(),
                    message: format!("Composed directive \"@{name}\" is named differently in a subgraph that doesn't export it. Consistent naming will be required to export it."),
                    locations: Self::locations_for_identity(item.identity(), subgraphs_with_different_naming),
                });
            }
        }

        // For anything which hasn't been ruled out, add it to the map of directives to be merged
        for (subgraph, items) in items_by_subgraph.iter_all() {
            let mut directives_for_subgraph = IndexSet::with_capacity(items.len());
            for item in items {
                if !wont_merge_features.contains(item.identity())
                    && !wont_merge_directive_names.contains(item.directive_name())
                {
                    directives_for_subgraph.insert(item.directive_name().clone());
                }
                // We specifically insert into `directive_identity_map` (and its inverse map
                // `identity_directive_map`) even when there's failures because we want directive
                // definition merging to use a definition from a spec we've found, in the event
                // merging finds non-composed directive definitions that happen to have the same
                // name.
                self.directive_identity_map.insert(
                    item.directive_name().clone(),
                    (
                        item.identity().clone(),
                        item.directive_name_in_spec().to_owned(),
                    ),
                );
                self.identity_directive_map
                    .entry(item.identity().clone())
                    .or_default()
                    // Note that normally we would just always insert (meaning last-write-wins), but
                    // we use first-write-wins here to maintain backwards compatibility.
                    .entry(item.directive_name().clone())
                    .or_insert(item.directive_name_in_spec().clone());
            }
            self.merge_directive_map
                .insert(subgraph.clone(), directives_for_subgraph);
        }

        Ok(())
    }

    /// If the subgraph's features/links are compatible for the given identity, then return the
    /// link with the latest spec version for those subgraphs that are actually used (a.k.a. the
    /// subgraphs with @composeDirective with a directive from that spec), along with the subgraph
    /// names of all subgraphs with that specific spec version (regardless of whether they use
    /// @composeDirective).
    fn get_latest_if_compatible<T: HasMetadata>(
        identity: &Identity,
        subgraphs: &[Subgraph<T>],
        subgraphs_used: &IndexSet<String>,
        error_reporter: &mut ErrorReporter,
    ) -> Option<(Arc<Link>, IndexSet<String>)> {
        let mut links_and_subgraphs: Vec<(Arc<Link>, &String)> = Vec::new();
        // Keep track of the latest link among subgraphs with @composeDirective for the identity, or
        // among all subgraphs if none of them have @composeDirective for the identity.
        let mut latest_link: Option<Arc<Link>> = None;
        // Whether any subgraphs have @composeDirective for the identity.
        let mut any_composed = false;
        // Whether a hint has been raised around major version mismatch yet.
        let mut major_mismatch_hint_raised = false;
        for subgraph in subgraphs {
            let Some(link) = subgraph.schema().metadata()?.for_identity(identity) else {
                continue;
            };
            links_and_subgraphs.push((link.clone(), &subgraph.name));
            let composed = subgraphs_used.contains(&subgraph.name);
            let Some(previous_latest_link) = &mut latest_link else {
                // If this is the first link seen with the identity, then track it, along with
                // whether it uses @composeDirective.
                latest_link = Some(link);
                any_composed = composed;
                continue;
            };
            if previous_latest_link.url.version.major != link.url.version.major {
                // If both subgraphs use @composeDirective with differing major versions, we can't
                // reconcile the incompatible directive semantics so we immediately error and unset
                // the latest link.
                if any_composed && composed {
                    latest_link = None;
                    any_composed = false;
                    error_reporter.add_error(CompositionError::DirectiveCompositionError {
                        message: format!("Core feature \"{identity}\" requested to be merged has major version mismatch across subgraphs")
                    });
                    break;
                }
                // If only one (or none) of the subgraphs use @composeDirective, we can just ignore
                // the non-composing subgraph(s). But we don't really know whether it's permissible
                // for these non-composing subgraphs to have differing major versions, as it's
                // dependent on the user-defined spec semantics. So we raise a hint here, and let
                // the user determine whether it's an issue.
                if !major_mismatch_hint_raised {
                    error_reporter.add_hint(CompositionHint {
                        code: HintCode::DirectiveCompositionInfo.code().to_string(),
                        message: format!("Non-composed core feature \"{identity}\" has major version mismatch across subgraphs"),
                        locations: Self::locations_for_identity(identity, subgraphs),
                    });
                    major_mismatch_hint_raised = true;
                }
                // The previous latest link is incomparable to the current link, so track the
                // current one unless some previous latest link was composed (which means the
                // current one isn't composed, otherwise we would have errored above).
                if !any_composed {
                    *previous_latest_link = link;
                    any_composed = composed;
                }
                continue;
            }
            // If the previous latest link has used @composeDirective while the current link has,
            // then we ignore the current link completely.
            if any_composed && !composed {
                continue;
            }
            // If no previous latest link has used @composeDirective while the current link has,
            // then we start tracking the current link, regardless of minor versions.
            if !any_composed && composed {
                *previous_latest_link = link;
                any_composed = true;
                continue;
            }
            // At this point, we know `any_composed` is equal to `composed`, so we just track the
            // current link provided its minor version isn't earlier than that of the previous
            // latest link.
            if previous_latest_link.url.version.minor <= link.url.version.minor {
                *previous_latest_link = link;
            }
        }
        let latest_link = latest_link?;
        if !any_composed {
            return None;
        }
        let subgraph_names_with_latest_link = links_and_subgraphs
            .into_iter()
            .filter(|(link, _)| link.url.version == latest_link.url.version)
            .map(|(_, subgraph_name)| subgraph_name.clone())
            // The rev() here is necessary to maintain backwards compatibility with previous
            // behavior (specifically, the search order of subgraphs when multiple of them have the
            // right version).
            .rev()
            .collect();
        Some((latest_link, subgraph_names_with_latest_link))
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

    fn locations_for_identity<'a, T: HasMetadata + 'a>(
        identity: &Identity,
        subgraphs: impl IntoIterator<Item = &'a Subgraph<T>>,
    ) -> Locations {
        subgraphs
            .into_iter()
            .flat_map(|subgraph| {
                let Some(metadata) = subgraph.schema().metadata() else {
                    return Locations::new();
                };
                let Some(link) = metadata.for_identity(identity) else {
                    return Locations::new();
                };
                link.locations(subgraph)
            })
            .collect()
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
            |elt, _| Some(format!("\"@{}\"", elt.directive_name())),
        );
    }
}
