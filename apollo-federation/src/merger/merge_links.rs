use std::sync::Arc;

use apollo_compiler::Name;
use apollo_compiler::ast::DirectiveDefinition;
use apollo_compiler::collections::IndexMap;
use itertools::Itertools;
use tracing::trace;

use crate::bail;
use crate::error::CompositionError;
use crate::error::FederationError;
use crate::link::Import;
use crate::link::Link;
use crate::link::authenticated_spec_definition::AUTHENTICATED_DIRECTIVE_NAME_IN_SPEC;
use crate::link::policy_spec_definition::POLICY_DIRECTIVE_NAME_IN_SPEC;
use crate::link::requires_scopes_spec_definition::REQUIRES_SCOPES_DIRECTIVE_NAME_IN_SPEC;
use crate::link::spec::Identity;
use crate::link::spec::Url;
use crate::link::spec_definition::SPEC_REGISTRY;
use crate::link::spec_definition::SpecDefinition;
use crate::merger::merge::MergedDirectiveInfo;
use crate::merger::merge::Merger;
use crate::schema::type_and_directive_specification::DirectiveCompositionSpecification;

pub(crate) struct CoreDirectiveInSubgraphs {
    url: Url,
    name: Name,
    definitions_per_subgraph: IndexMap<String, DirectiveDefinition>,
    composition_spec: DirectiveCompositionSpecification,
}

impl std::fmt::Debug for CoreDirectiveInSubgraphs {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CoreDirectiveInSubgraphs")
            .field("url", &self.url)
            .field("name", &self.name)
            .field("definitions_per_subgraph", &self.definitions_per_subgraph)
            .finish()
    }
}

struct CoreDirectiveInSupergraph {
    spec_in_supergraph: &'static dyn SpecDefinition,
    name_in_feature: Name,
    name_in_supergraph: Name,
    composition_spec: DirectiveCompositionSpecification,
}

impl Merger {
    pub(crate) fn collect_core_directives_to_compose(
        &self,
    ) -> Result<Vec<CoreDirectiveInSubgraphs>, FederationError> {
        trace!("Collecting core directives used in subgraphs");
        // Groups directives by their feature and major version (we use negative numbers for
        // pre-1.0 version numbers on the minor, since all minors are incompatible).
        let mut directives_per_feature_and_version: IndexMap<
            String,
            IndexMap<i32, CoreDirectiveInSubgraphs>,
        > = IndexMap::default();

        for subgraph in &self.subgraphs {
            let Some(features) = subgraph.schema().metadata() else {
                bail!("Subgraphs should be core schemas")
            };

            for (directive, referencers) in &subgraph.schema().referencers().directives {
                let Some(linked_elem) = features.source_link_of_directive(directive) else {
                    continue;
                };
                if referencers.is_empty() {
                    continue;
                }
                let source = linked_elem.link;
                let import = match linked_elem.import {
                    Some(import) => import,
                    None => {
                        // If there is no explicit import, we create a synthetic import for merging
                        let Some((_, directive_name_in_spec)) = directive.split_once("__") else {
                            continue;
                        };
                        let Ok(element_name) = Name::new(directive_name_in_spec) else {
                            continue;
                        };
                        Arc::new(Import {
                            element: element_name,
                            is_directive: true,
                            alias: None,
                        })
                    }
                };
                let Some(composition_spec) = SPEC_REGISTRY.get_composition_spec(&source, &import)
                else {
                    trace!(
                        "Directive @{directive} from {} has no registered composition spec, skipping",
                        source.url
                    );
                    continue;
                };
                let Some(definition) = subgraph
                    .schema()
                    .schema()
                    .directive_definitions
                    .get(directive)
                else {
                    bail!(
                        "Missing directive definition for @{directive} in {}",
                        source
                    )
                };

                let fqn = format!("{}-{}", import.element, source.url.identity);
                let for_feature = directives_per_feature_and_version.entry(fqn).or_default();

                let major = if source.url.version.major > 0 {
                    source.url.version.major as i32
                } else {
                    -(source.url.version.minor as i32)
                };

                if let Some(for_version) = for_feature.get_mut(&major) {
                    if source.url.version > for_version.url.version {
                        for_version.url = source.url.clone();
                    }
                    for_version
                        .definitions_per_subgraph
                        .insert(subgraph.name.clone(), (**definition).clone());
                } else {
                    let mut definitions_per_subgraph = IndexMap::default();
                    definitions_per_subgraph.insert(subgraph.name.clone(), (**definition).clone());
                    let for_version = CoreDirectiveInSubgraphs {
                        url: source.url.clone(),
                        name: import.element.clone(),
                        definitions_per_subgraph,
                        composition_spec,
                    };
                    for_feature.insert(major, for_version);
                }
            }
        }

        Ok(directives_per_feature_and_version
            .into_iter()
            .flat_map(|(_, values)| values.into_values())
            .collect())
    }

    pub(crate) fn validate_and_maybe_add_specs(
        &mut self,
        directives_merge_info: &[CoreDirectiveInSubgraphs],
    ) -> Result<(), FederationError> {
        let mut supergraph_info_by_identity: IndexMap<Identity, Vec<CoreDirectiveInSupergraph>> =
            IndexMap::default();

        trace!("Determining supergraph names for directives used in subgraphs");
        for subgraph_core_directive in directives_merge_info {
            let mut name_in_supergraph: Option<&Name> = None;
            for subgraph in &self.subgraphs {
                let Some(directive) = subgraph_core_directive
                    .definitions_per_subgraph
                    .get(&subgraph.name)
                else {
                    continue;
                };

                if name_in_supergraph.is_none() {
                    name_in_supergraph = Some(&directive.name);
                } else if name_in_supergraph.is_some_and(|n| *n != directive.name) {
                    let definition_sources = self
                        .subgraphs
                        .iter()
                        .enumerate()
                        .map(|(idx, s)| {
                            (
                                idx,
                                subgraph_core_directive
                                    .definitions_per_subgraph
                                    .get(&s.name),
                            )
                        })
                        .collect();
                    self.error_reporter.report_mismatch_error(
                        CompositionError::LinkImportNameMismatch {
                            message: format!("The \"@{}\" directive (from {}) is imported with mismatched name between subgraphs: it is imported as ", directive.name, subgraph_core_directive.url),
                        },
                        &directive,
                        &definition_sources,
                        &self.subgraphs,
                        |def| Some(format!("\"@{}\"", def.name)),
                        |def, _| Some(format!("\"@{}\"", def.name)),
                    );
                    return Ok(());
                }
            }

            // If we get here with `name_in_supergraph` unset, it means there is no usage for the
            // directive at all, and we don't bother adding the spec to the supergraph.
            let Some(name_in_supergraph) = name_in_supergraph else {
                trace!(
                    "Directive @{} is not used in any subgraph, skipping",
                    subgraph_core_directive.name
                );
                continue;
            };
            let Some(spec_in_supergraph) =
                (subgraph_core_directive
                    .composition_spec
                    .supergraph_specification)(&self.latest_federation_version_used)
            else {
                trace!(
                    "Directive @{name_in_supergraph} has no registered composition spec, skipping"
                );
                continue;
            };
            let supergraph_info = supergraph_info_by_identity
                .entry(spec_in_supergraph.identity().clone())
                .or_default();

            if !supergraph_info
                .iter()
                .any(|d| d.name_in_feature == subgraph_core_directive.name)
            {
                supergraph_info.push(CoreDirectiveInSupergraph {
                    spec_in_supergraph,
                    name_in_feature: subgraph_core_directive.name.clone(),
                    name_in_supergraph: name_in_supergraph.clone(),
                    composition_spec: subgraph_core_directive.composition_spec.clone(),
                });
            }

            if supergraph_info
                .iter()
                .any(|s| s.spec_in_supergraph.url() != spec_in_supergraph.url())
            {
                bail!(
                    "Spec {} directives disagree on version for supergraph",
                    spec_in_supergraph.url()
                )
            }

            if subgraph_core_directive.composition_spec.use_join_directive {
                self.directives_using_join_directive
                    .insert(name_in_supergraph.clone());
            }
        }

        for supergraph_core_directives in supergraph_info_by_identity.values() {
            let mut imports = Vec::new();
            for supergraph_core_directive in supergraph_core_directives {
                // Directives composed via @join__directive are not imported in the supergraph schema.
                if supergraph_core_directive
                    .composition_spec
                    .use_join_directive
                {
                    continue;
                }
                let default_name_in_supergraph = Link::directive_name_in_schema_for_core_arguments(
                    supergraph_core_directive.spec_in_supergraph.url(),
                    &supergraph_core_directive
                        .spec_in_supergraph
                        .url()
                        .identity
                        .name,
                    &[],
                    &supergraph_core_directive.name_in_feature,
                );
                if supergraph_core_directive.name_in_supergraph != default_name_in_supergraph {
                    let alias = if supergraph_core_directive.name_in_feature
                        == supergraph_core_directive.name_in_supergraph
                    {
                        None
                    } else {
                        Some(supergraph_core_directive.name_in_supergraph.clone())
                    };
                    imports.push(Import {
                        element: supergraph_core_directive.name_in_feature.clone(),
                        is_directive: true,
                        alias,
                    });
                }
            }

            self.link_spec_definition.apply_feature_to_schema(
                &mut self.merged,
                supergraph_core_directives[0].spec_in_supergraph,
                None,
                supergraph_core_directives[0].spec_in_supergraph.purpose(),
                Some(imports),
            )?;

            let Some(links_metadata) = self.merged.metadata() else {
                bail!("Missing links metadata in supergraph schema");
            };
            let feature = links_metadata.for_identity(
                &supergraph_core_directives[0]
                    .spec_in_supergraph
                    .url()
                    .identity,
            );
            for supergraph_core_directive in supergraph_core_directives {
                let arguments_merger = if let Some(merger_factory) = supergraph_core_directive
                    .composition_spec
                    .argument_merger
                    .as_ref()
                {
                    Some(merger_factory(&self.merged, feature.as_ref())?)
                } else {
                    None
                };
                self.merged_federation_directive_names
                    .insert(supergraph_core_directive.name_in_supergraph.to_string());
                self.merged_federation_directive_in_supergraph_by_directive_name
                    .insert(
                        supergraph_core_directive.name_in_supergraph.clone(),
                        MergedDirectiveInfo {
                            arguments_merger,
                            static_argument_transform: supergraph_core_directive
                                .composition_spec
                                .static_argument_transform
                                .clone(),
                        },
                    );
                // If we encounter the @inaccessible directive, we need to record its definition so
                // certain merge validations that care about @inaccessible can act accordingly.
                if *supergraph_core_directive.spec_in_supergraph.identity()
                    == Identity::inaccessible_identity()
                    && supergraph_core_directive.name_in_feature
                        == supergraph_core_directive
                            .spec_in_supergraph
                            .url()
                            .identity
                            .name
                {
                    self.inaccessible_directive_name_in_supergraph =
                        Some(supergraph_core_directive.name_in_supergraph.clone());
                }

                if *supergraph_core_directive.spec_in_supergraph.identity()
                    == Identity::authenticated_identity()
                    && supergraph_core_directive.name_in_feature
                        == supergraph_core_directive
                            .spec_in_supergraph
                            .url()
                            .identity
                            .name
                {
                    self.access_control_directives_in_supergraph.push((
                        AUTHENTICATED_DIRECTIVE_NAME_IN_SPEC,
                        supergraph_core_directive.name_in_supergraph.clone(),
                    ));
                }
                if *supergraph_core_directive.spec_in_supergraph.identity()
                    == Identity::requires_scopes_identity()
                    && supergraph_core_directive.name_in_feature
                        == supergraph_core_directive
                            .spec_in_supergraph
                            .url()
                            .identity
                            .name
                {
                    self.access_control_directives_in_supergraph.push((
                        REQUIRES_SCOPES_DIRECTIVE_NAME_IN_SPEC,
                        supergraph_core_directive.name_in_supergraph.clone(),
                    ));
                }
                if *supergraph_core_directive.spec_in_supergraph.identity()
                    == Identity::policy_identity()
                    && supergraph_core_directive.name_in_feature
                        == supergraph_core_directive
                            .spec_in_supergraph
                            .url()
                            .identity
                            .name
                {
                    self.access_control_directives_in_supergraph.push((
                        POLICY_DIRECTIVE_NAME_IN_SPEC,
                        supergraph_core_directive.name_in_supergraph.clone(),
                    ));
                }
            }
        }
        trace!(
            "The following federation directives will be merged if applications are found: {}",
            self.merged_federation_directive_names.iter().join(", ")
        );

        Ok(())
    }
}
