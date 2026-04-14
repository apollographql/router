use std::collections::HashMap;

use apollo_compiler::Name;
use apollo_compiler::Node;
use apollo_compiler::ast::Argument;
use apollo_compiler::ast::Directive;
use apollo_compiler::ast::DirectiveDefinition;
use apollo_compiler::ast::DirectiveLocation;
use apollo_compiler::ast::Value;
use apollo_compiler::collections::IndexMap;
use apollo_compiler::collections::IndexSet;
use itertools::Itertools;
use tracing::instrument;
use tracing::trace;

use crate::bail;
use crate::error::FederationError;
use crate::link::authenticated_spec_definition::AUTHENTICATED_DIRECTIVE_NAME_IN_SPEC;
use crate::link::policy_spec_definition::POLICY_DIRECTIVE_NAME_IN_SPEC;
use crate::link::requires_scopes_spec_definition::REQUIRES_SCOPES_DIRECTIVE_NAME_IN_SPEC;
use crate::link::spec_definition::SpecDefinition;
use crate::merger::hints::HintCode;
use crate::merger::merge::Merger;
use crate::merger::merge::Sources;
use crate::merger::merge::map_sources;
use crate::schema::position::DirectiveDefinitionPosition;
use crate::schema::position::DirectiveTargetPosition;
use crate::schema::position::HasAppliedDirectives;
use crate::schema::position::HasDescription;
use crate::schema::position::InterfaceTypeDefinitionPosition;
use crate::schema::type_and_directive_specification::StaticArgumentsTransform;
use crate::subgraph::typestate::Subgraph;
use crate::subgraph::typestate::Validated;
use crate::supergraph::CompositionHint;
use crate::supergraph::EXECUTABLE_DIRECTIVE_LOCATIONS;
use crate::utils::first_max_by_key;

#[derive(Clone)]
pub(crate) struct AppliedDirectiveToMergeEntry {
    pub names: IndexSet<Name>,
    pub sources: Sources<DirectiveTargetPosition>,
    pub dest: DirectiveTargetPosition,
}

pub(crate) type AppliedDirectivesToMerge = Vec<AppliedDirectiveToMergeEntry>;

pub(in crate::merger) type AdditionalDirectiveSources =
    IndexMap<usize, IndexSet<DirectiveTargetPosition>>;

impl Merger {
    pub(in crate::merger) fn record_applied_directives_to_merge<T>(
        &mut self,
        sources: &Sources<T>,
        dest: &T,
    ) -> Result<(), FederationError>
    where
        T: Clone + Into<DirectiveTargetPosition>,
    {
        let inaccessible_name = self.inaccessible_directive_name_in_supergraph.clone();
        let mut directive_sources: Sources<DirectiveTargetPosition> =
            IndexMap::with_capacity_and_hasher(sources.len(), Default::default());
        let mut names = IndexSet::with_capacity_and_hasher(sources.len(), Default::default());

        // This loop corresponds to `gatherAppliedDirectivesToMerge` in the JS implementation.
        for (idx, source) in sources {
            let Some(source) = source else {
                continue;
            };
            let source: DirectiveTargetPosition = source.clone().into();
            let subgraph = &self.subgraphs[*idx];
            for directive in source.get_all_applied_directives(subgraph.schema()) {
                if self.is_merged_directive(&subgraph.name, directive) {
                    names.insert(directive.name.clone());
                }
            }
            directive_sources.insert(*idx, Some(source));
        }

        let dest = dest.clone().into();
        if let Some(inaccessible) = inaccessible_name.as_ref()
            && names.contains(inaccessible)
        {
            self.merge_applied_directive(inaccessible, &directive_sources, &dest)?;
            names.shift_remove(inaccessible);
        }

        // PORT NOTE: in JS we were capturing additional sources in record_applied_directives_to_merge.
        // JS was recording an array/vec of FieldDefinition/ObjectTypeDefinition/InterfaceTypeDefinition
        // but this doesn't work in RS implementations as we record lightweight references to the target
        // directive positions instead. Since we cannot just update source map with additional sources
        // (as it would overwrite the existing entries for the subgraphs), we make sure that target access
        // control directive will be merged.
        if matches!(
            dest,
            DirectiveTargetPosition::InterfaceField(_)
                | DirectiveTargetPosition::ObjectField(_)
                | DirectiveTargetPosition::InterfaceType(_)
        ) {
            for (name, name_in_supergraph) in &self.access_control_directives_in_supergraph {
                if names.contains(name_in_supergraph) {
                    // access control directive is already in the list of directives to be merged
                    continue;
                } else if self
                    .access_control_additional_sources()?
                    .contains_key(&format!("{dest}_{name}"))
                {
                    // need to add access control directive to the list of directives to be merged
                    names.insert(name_in_supergraph.clone());
                }
            }
        }

        if names.is_empty() {
            trace!("No applied directives to merge at {dest}");
        } else {
            trace!(
                "Position {dest} has applied directives to merge: {}",
                names.iter().join(", ")
            );
            self.applied_directives_to_merge
                .push(AppliedDirectiveToMergeEntry {
                    names,
                    sources: directive_sources,
                    dest,
                });
        }
        Ok(())
    }

    /// For a given directive name, merges all its applications at the given destination position.
    /// Note that this logic relies on the fact that the directive must have the same name across
    /// all subgraphs.
    #[instrument(skip(self, sources))]
    pub(in crate::merger) fn merge_applied_directive(
        &mut self,
        name: &Name,
        sources: &Sources<DirectiveTargetPosition>,
        dest: &DirectiveTargetPosition,
    ) -> Result<(), FederationError> {
        let Some(definition) = self
            .merged
            .schema()
            .directive_definitions
            .get(name)
            .cloned()
        else {
            // This should never happen as we only record directives to merge that we know are merged.
            bail!(
                "Cannot merge applied directive @{name} at {dest} as the directive is not defined in the supergraph schema"
            );
        };
        let directive_in_supergraph = self
            .merged_federation_directive_in_supergraph_by_directive_name
            .get(name);

        let mut directive_counts: IndexMap<Directive, usize> = sources
            .iter()
            .flat_map(|(idx, source)| {
                let Some(source) = source else {
                    return vec![];
                };

                let subgraph = &self.subgraphs[*idx];
                let mut applications = source
                    .get_applied_directives(subgraph.schema(), name)
                    .into_iter()
                    .map(|d| (**d).clone())
                    .collect_vec();
                if let Some(transform) =
                    &directive_in_supergraph.and_then(|d| d.static_argument_transform.as_ref())
                {
                    for application in &mut applications {
                        self.transform_arguments(application, subgraph, transform.as_ref());
                    }
                }
                applications
            })
            .fold(Default::default(), |mut acc, directive| {
                // Note that when comparing arguments, we include default values. This means that we
                // consider it the same thing (as far as merging application goes) to rely on a default
                // value or to pass that very exact value explicitly.
                let args = self.directive_arguments_with_defaults(&directive, &definition);
                if let Some((_, count)) = acc.iter_mut().find(|(existing, _)| {
                    let existing_args =
                        self.directive_arguments_with_defaults(existing, &definition);
                    existing_args == args
                }) {
                    *count += 1;
                } else {
                    acc.insert(directive, 1);
                }
                acc
            });

        // PORT NOTE: in JS version we were populating additional sources for access control in record_applied_directives_to_merge
        // without any changes in the merge_applied_directive_logic.
        // We need to explicitly look up the target schema element in RS so we handle this logic here directly
        if matches!(
            dest,
            DirectiveTargetPosition::InterfaceField(_)
                | DirectiveTargetPosition::ObjectField(_)
                | DirectiveTargetPosition::InterfaceType(_)
        ) {
            for (access_control_directive_name, access_control_directive_name_in_supergraph) in
                &self.access_control_directives_in_supergraph
            {
                if name == access_control_directive_name_in_supergraph
                    && let Some(additional_sources_for_position) = self
                        .access_control_additional_sources()?
                        .get(&format!("{dest}_{access_control_directive_name}"))
                {
                    // we need to propagate access control
                    // - upwards from object types to interfaces
                    // - upwards from object fields to interface fields
                    // - downwards from interface object fields to object fields
                    additional_sources_for_position
                        .iter()
                        .flat_map(|(index, sources)| {
                            let subgraph = &self.subgraphs[*index];
                            let mut applications = sources
                                .iter()
                                .flat_map(|source| {
                                    source
                                        .get_applied_directives(subgraph.schema(), name)
                                        .into_iter()
                                        .map(|d| (**d).clone())
                                })
                                .collect_vec();
                            if let Some(transform) = &directive_in_supergraph
                                .and_then(|d| d.static_argument_transform.as_ref())
                            {
                                for application in &mut applications {
                                    self.transform_arguments(
                                        application,
                                        subgraph,
                                        transform.as_ref(),
                                    );
                                }
                            }
                            applications
                        })
                        .for_each(|d| {
                            // access control directives don't have default args so we don't need to transform them
                            *directive_counts.entry(d).or_insert(0) += 1;
                        });
                }
            }
        }

        if definition.repeatable {
            trace!(
                "Directive @{name} is repeatable, merging all {} applications at {dest}",
                directive_counts.len()
            );
            for directive in directive_counts.into_keys() {
                dest.insert_directive(&mut self.merged, directive)?;
            }
        } else if directive_counts.len() == 1 {
            trace!(
                "Directive @{name} is non-repeatable but only applied once, merging application at {dest}"
            );
            let only_application = directive_counts.into_keys().next().unwrap();
            dest.insert_directive(&mut self.merged, only_application)?;
        } else if let Some(merger) =
            &directive_in_supergraph.and_then(|d| d.arguments_merger.as_ref())
        {
            // When we have multiple unique applications of the directive, and there is a
            // supplied argument merger, then we merge each of the arguments into a combined
            // directive.
            let mut merged_directive = Directive::new(name.clone());
            for arg_def in &definition.arguments {
                let values = directive_counts
                    .keys()
                    .filter_map(|d| {
                        d.specified_argument_by_name(&arg_def.name)
                            .map(|v| v.as_ref())
                    })
                    .cloned()
                    .collect_vec();
                if let Some(merged_value) = (merger.merge)(&arg_def.name, &values)? {
                    let merged_arg = Argument {
                        name: arg_def.name.clone(),
                        value: Node::new(merged_value),
                    };
                    merged_directive.arguments.push(Node::new(merged_arg));
                }
            }
            trace!(
                "Directive @{name} is non-repeatable but has an argument merger, merging applications at {dest}"
            );
            dest.insert_directive(&mut self.merged, merged_directive)?;
            self.error_reporter.add_hint(CompositionHint {
                    code: HintCode::MergedNonRepeatableDirectiveArguments.code().to_string(),
                    message: format!(
                        "Directive @{name} is applied to \"{dest}\" in multiple subgraphs with different arguments. Merging strategies used by arguments: {}",
                        directive_in_supergraph.and_then(|d| d.arguments_merger.as_ref()).map_or("undefined".to_string(), |m| (m.to_string)())
                    ),
                    locations: Default::default(), // PORT_NOTE: No locations in JS implementation.
                });
        } else if let Some(most_used_directive) =
            first_max_by_key(directive_counts.into_iter(), |(_, count)| *count)
                .map(|(directive, _)| directive)
        {
            trace!(
                "Directive @{name} is non-repeatable and has no argument merger, picking most used application at {dest}"
            );
            // When there is no argument merger, we use the application appearing in the most
            // subgraphs. Adding it to the destination here allows the error reporter to
            // determine which one we selected when it's looking through the sources.
            dest.insert_directive(&mut self.merged, most_used_directive.clone())?;
            fn print_arguments(elt: &Directive) -> Option<String> {
                if elt.arguments.is_empty() {
                    Some("no arguments".to_string())
                } else {
                    Some(format!(
                        // This is a single set of curly braces with a value interpolated in the middle.
                        "arguments {{{}}}",
                        elt.arguments
                            .iter()
                            .map(|arg| format!("{}: {}", arg.name, arg.value))
                            .join(", ")
                    ))
                }
            }
            self.error_reporter.report_mismatch_hint(
                    HintCode::InconsistentNonRepeatableDirectiveArguments,
                    format!("Non-repeatable directive @{name} is applied to \"{dest}\" in multiple subgraphs but with incompatible arguments. "),
                    &most_used_directive,
                    sources,
                    &self.subgraphs,
                    print_arguments,
                    |pos, idx| {
                        pos.get_applied_directives(self.subgraphs[idx].schema(), name)
                            .first()
                            .and_then(|d| print_arguments(d))
                },
                    |application, subgraphs| format!("The supergraph will use {} (from {}), but found ", application, subgraphs.unwrap_or_else(|| "undefined".to_string())),
                    |application, subgraphs| format!("{application} in {subgraphs}"),
                    false,
                    false,
                );
        } else {
            trace!(
                "Directive @{name} is non-repeatable but has no applications to merge at {dest} (this should not happen)"
            );
        }

        Ok(())
    }

    fn directive_arguments_with_defaults<'dir>(
        &self,
        directive: &'dir Directive,
        definition: &'dir DirectiveDefinition,
    ) -> IndexMap<Name, Option<&'dir Node<Value>>> {
        definition
            .arguments
            .iter()
            .map(|arg_def| {
                (
                    arg_def.name.clone(),
                    directive
                        .specified_argument_by_name(&arg_def.name)
                        .or(arg_def.default_value.as_ref()),
                )
            })
            .collect()
    }

    fn transform_arguments(
        &self,
        directive: &mut Directive,
        subgraph: &Subgraph<Validated>,
        transform: &StaticArgumentsTransform,
    ) {
        let indexed_args = directive
            .arguments
            .drain(..)
            .map(|arg| (arg.name.clone(), (*arg.value).clone()))
            .collect::<IndexMap<_, _>>();
        directive.arguments = transform(subgraph, indexed_args)
            .into_iter()
            .map(|(name, value)| {
                Node::new(Argument {
                    name,
                    value: Node::new(value),
                })
            })
            .collect();
    }

    pub(crate) fn merge_directive_definition(
        &mut self,
        name: &Name,
    ) -> Result<(), FederationError> {
        // We have 2 behavior depending on the kind of directives:
        // 1) for the few handpicked type system directives that we merge, we always want to keep
        //   them (it's ok if a subgraph decided to not include the definition because that particular
        //   subgraph didn't use the directive on its own definitions). For those, we essentially take
        //   a "union" strategy.
        // 2) for other directives, the ones we keep for their 'execution' locations, we instead
        //   use an "intersection" strategy: we only keep directives that are defined everywhere.
        //   The reason is that those directives may be used anywhere in user queries (those made
        //   against the supergraph API), and hence can end up in queries to any subgraph, and as
        //   a consequence all subgraphs need to be able to handle any application of the directive.
        //   Which we can only guarantee if all the subgraphs know the directive, and that the directive
        //   definition is the intersection of all definitions (meaning that if there divergence in
        //   locations, we only expose locations that are common everywhere).
        if self
            .compose_directive_manager
            .directive_exists_in_supergraph(name)
        {
            self.merge_custom_core_directive(name)?;
        } else {
            let sources = self
                .subgraphs
                .iter()
                .enumerate()
                .map(|(idx, subgraph)| {
                    let def = subgraph.schema().get_directive_definition(name);
                    (idx, def)
                })
                .collect();
            if Self::some_sources(&sources, |source, idx| {
                let Some(source) = source else {
                    return false;
                };
                let Some(def) = source.try_get(self.subgraphs[idx].schema().schema()) else {
                    return false;
                };
                self.is_merged_directive_definition(&self.names[idx], def)
            }) {
                self.merge_executable_directive_definition(
                    name,
                    &sources,
                    &DirectiveDefinitionPosition {
                        directive_name: name.clone(),
                    },
                )?;
            }
        }
        Ok(())
    }

    pub(crate) fn merge_custom_core_directive(
        &mut self,
        name: &Name,
    ) -> Result<(), FederationError> {
        // Note that the source here may have a different name than the destination.
        let Some((subgraph_name, source)) = self
            .compose_directive_manager
            .get_latest_directive_definition(name, &self.subgraphs, &mut self.error_reporter)?
        else {
            return Ok(());
        };
        let Some((source_idx, subgraph)) = self
            .subgraphs
            .iter()
            .enumerate()
            .find(|(_, subgraph)| subgraph.name == subgraph_name)
        else {
            return Ok(());
        };

        let dest = DirectiveDefinitionPosition {
            directive_name: name.clone(),
        };
        let subgraph_def = source.get(subgraph.schema().schema())?;
        dest.set_repeatable(&mut self.merged, subgraph_def.repeatable)?;
        dest.set_locations(&mut self.merged, subgraph_def.locations.clone())?;
        dest.set_description(&mut self.merged, subgraph_def.description.clone())?;

        let sources: Sources<DirectiveDefinitionPosition> =
            std::iter::once((source_idx, Some(source.clone()))).collect();
        let arg_names = self.add_arguments_shallow(&sources, &dest)?;

        for arg_name in arg_names {
            let sources_arg = map_sources(&sources, |source| {
                source.as_ref().map(|s| s.argument(arg_name.clone()))
            });
            let dest_arg = dest.argument(arg_name);
            self.merge_argument(&sources_arg, &dest_arg)?;
        }
        Ok(())
    }

    fn merge_executable_directive_definition(
        &mut self,
        name: &Name,
        sources: &Sources<DirectiveDefinitionPosition>,
        dest: &DirectiveDefinitionPosition,
    ) -> Result<(), FederationError> {
        let mut repeatable: Option<bool> = None;
        let mut inconsistent_repeatable = false;
        let mut locations: Vec<DirectiveLocation> = Vec::new();
        let mut inconsistent_locations = false;

        for (idx, source) in sources {
            let Some(source) = source
                .as_ref()
                .and_then(|s| s.try_get(self.subgraphs[*idx].schema().schema()))
            else {
                // An executable directive could appear in any place of a query and thus get to any subgraph, so we cannot keep an
                // executable directive unless it is in all subgraphs. We use an 'intersection' strategy.
                dest.remove(&mut self.merged)?;
                self.error_reporter.report_mismatch_hint(
                    HintCode::InconsistentExecutableDirectivePresence,
                    format!("Executable directive \"@{name}\" will not be part of the supergraph as it does not appear in all subgraphs: "),
                    dest,
                    sources,
                    &self.subgraphs,
                    |_elt| Some("yes".to_string()),
                    |_elt, _idx| Some("yes".to_string()),
                    |_, subgraphs| format!("it is defined in {}", subgraphs.unwrap_or_default()),
                    |_, subgraphs| format!(" but not in {subgraphs}"),
                    true,
                    false,
                );
                return Ok(());
            };

            if let Some(val) = repeatable {
                if val != source.repeatable {
                    inconsistent_repeatable = true;
                    repeatable = Some(false);
                }
            } else {
                repeatable = Some(source.repeatable);
            }

            let source_locations = extract_executable_locations(source);
            trace!(
                "Source locations for executable directive \"@{name}\" in subgraph {}: {:?}",
                self.subgraphs[*idx].name, source_locations
            );
            if locations.is_empty() {
                locations = source_locations;
            } else {
                if locations != source_locations {
                    inconsistent_locations = true;
                }
                locations.retain(|loc| source_locations.contains(loc));

                trace!(
                    "After processing subgraph {}, executable directive \"@{name}\" has locations: {:?}",
                    self.subgraphs[*idx].name, locations
                );
                if locations.is_empty() {
                    self.error_reporter.report_mismatch_hint(
                        HintCode::NoExecutableDirectiveLocationsIntersection,
                        format!("Executable directive \"@{name}\" has no location that is common to all subgraphs: "),
                        dest,
                        sources,
                        &self.subgraphs,
                        |_| Some(location_string(&[])),
                        |pos, idx| pos.try_get(self.subgraphs[idx].schema().schema())
                            .map(|elt| location_string(&extract_executable_locations(elt))),
                        |_, _subgraphs| "it will not appear in the supergraph as there no intersection between ".to_string(),
                        |locs, subgraphs| format!("{locs} in {subgraphs}"),
                        false,
                        false,
                    );
                }
            }
        }
        dest.set_repeatable(&mut self.merged, repeatable.unwrap_or_default())?; // repeatable will always be Some() here
        dest.set_locations(&mut self.merged, locations)?;

        self.merge_description(sources, dest)?;
        let supergraph_dest = dest.get(self.merged.schema())?;

        if inconsistent_repeatable {
            self.error_reporter.report_mismatch_hint(
                HintCode::InconsistentExecutableDirectiveRepeatable,
                format!("Executable directive \"@{name}\" will not be marked repeatable in the supergraph as it is inconsistently marked repeatable in subgraphs: "),
                supergraph_dest,
                sources,
                &self.subgraphs,
                |_| if repeatable.unwrap_or_default() { Some("yes".to_string()) } else { Some("no".to_string()) },
                |pos, idx| pos.try_get(self.subgraphs[idx].schema().schema())
                    .map(|elt|  if elt.repeatable { "yes".to_string() } else { "no".to_string() }),
                |_, subgraphs| format!("it is not repeatable in {}", subgraphs.unwrap_or_default()),
                |_, subgraphs| format!(" but is repeatable in {}", subgraphs),
                false,
                false,
            );
        }
        if inconsistent_locations {
            self.error_reporter.report_mismatch_hint(
                HintCode::InconsistentExecutableDirectiveLocations,
                format!(
                    "Executable directive \"@{name}\" has inconsistent locations across subgraphs "
                ),
                supergraph_dest,
                sources,
                &self.subgraphs,
                |elt| Some(location_string(&extract_executable_locations(elt))),
                |pos, idx| pos.try_get(self.subgraphs[idx].schema().schema()).map(|elt| location_string(&extract_executable_locations(elt))),
                |locs, subgraphs| {
                    format!("and will use {locs} (intersection of all subgraphs) in the supergraph, but has: {}",
                    subgraphs.map(|s| format!("{locs} in {s} and ")).unwrap_or_default())
                },
                |locs, subgraphs| format!("{locs} in {subgraphs}"),
                false,
                false,
            );
        }

        // Doing args last, mostly so we don't bother adding if the directive doesn't make it in.
        let arg_names = self.add_arguments_shallow(sources, dest)?;
        for arg in arg_names {
            let subgraph_args = map_sources(sources, |src| {
                src.as_ref().map(|src| src.argument(arg.clone()))
            });
            self.merge_argument(&subgraph_args, &dest.argument(arg))?;
        }
        Ok(())
    }

    pub(crate) fn merge_all_applied_directives(&mut self) -> Result<(), FederationError> {
        for AppliedDirectiveToMergeEntry {
            names,
            sources,
            dest,
        } in self.applied_directives_to_merge.drain(..).collect_vec()
        {
            trace!(
                "Merging applied directives {} as {dest}",
                names.iter().join(", ")
            );
            // There are some cases where we recorded directives to be merged on a `dest` that ended
            // up being removed from the ouptut. This is typically because we needed to known if that
            // `dest` was @inaccessible before deciding if it should be kept or not. If it no
            // longer exists in the schema, then we skip this destination.
            if !dest.exists_in(&self.merged) {
                continue;
            }
            for name in names {
                self.merge_applied_directive(&name, &sources, &dest)?;
            }
        }
        Ok(())
    }

    pub(crate) fn access_control_additional_sources(
        &self,
    ) -> Result<&HashMap<String, AdditionalDirectiveSources>, FederationError> {
        self.access_control_additional_sources.get_or_try_init(|| {
            let mut sources: HashMap<String, AdditionalDirectiveSources> = HashMap::default();
            for (index, subgraph) in self.subgraphs.iter().enumerate() {
                let metadata = subgraph.metadata();
                let federation_spec = metadata.federation_spec_definition();
                let subgraph_referencers = subgraph.schema().referencers();
                for access_control_directive in [
                    &AUTHENTICATED_DIRECTIVE_NAME_IN_SPEC,
                    &REQUIRES_SCOPES_DIRECTIVE_NAME_IN_SPEC,
                    &POLICY_DIRECTIVE_NAME_IN_SPEC,
                ] {
                    if let Some(directive) = federation_spec
                        .directive_definition(subgraph.schema(), access_control_directive)?
                    {
                        let referencers = subgraph_referencers.get_directive(&directive.name);
                        for type_position in &referencers.object_types {
                            // we will be propagating access control from objects up to the interfaces
                            let object_type = type_position.get(self.merged.schema())?;
                            for interface in &object_type.implements_interfaces {
                                let key = format!("{}_{}", interface, access_control_directive);
                                let existing_sources = sources.entry(key).or_default();
                                existing_sources.entry(index).or_default().insert(
                                    DirectiveTargetPosition::ObjectType(type_position.clone()),
                                );
                            }
                        }
                        for field_definition_position in &referencers.object_fields {
                            if metadata.is_field_external(&field_definition_position.clone().into())
                            {
                                continue;
                            }

                            if metadata
                                .is_interface_object_type(&field_definition_position.type_name)
                            {
                                // we need to propagate field access control downwards from interface object fields to object fields
                                // we need to look up the interface in the merged supergraph to find all its implementations
                                let supergraph_type: InterfaceTypeDefinitionPosition =
                                    InterfaceTypeDefinitionPosition {
                                        type_name: field_definition_position.type_name.clone(),
                                    };
                                for implementation in
                                    self.merged.all_implementation_types(&supergraph_type)?
                                {
                                    let key = format!(
                                        "{}.{}_{}",
                                        implementation,
                                        &field_definition_position.field_name,
                                        access_control_directive
                                    );
                                    sources
                                        .entry(key)
                                        .or_default()
                                        .entry(index)
                                        .or_default()
                                        .insert(DirectiveTargetPosition::ObjectField(
                                            field_definition_position.clone(),
                                        ));

                                    // we now need to propagate field access control upwards from @interfaceObject fields to any
                                    // other interfaces implemented by the given implementation type
                                    for other_interface in
                                        implementation.implemented_interfaces(&self.merged)?
                                    {
                                        if other_interface.name
                                            == field_definition_position.type_name
                                        {
                                            // skip current @interfaceObject
                                            continue;
                                        }
                                        let key = format!(
                                            "{}.{}_{}",
                                            other_interface,
                                            &field_definition_position.field_name,
                                            access_control_directive
                                        );
                                        sources
                                            .entry(key)
                                            .or_default()
                                            .entry(index)
                                            .or_default()
                                            .insert(DirectiveTargetPosition::ObjectField(
                                                field_definition_position.clone(),
                                            ));
                                    }
                                }
                            } else {
                                // we need to propagate field access control upwards from object fields to the interface fields
                                let merged_object_type = field_definition_position
                                    .parent()
                                    .get(self.merged.schema())?;
                                for interface in &merged_object_type.implements_interfaces {
                                    let key = format!(
                                        "{}.{}_{}",
                                        interface,
                                        field_definition_position.field_name,
                                        access_control_directive
                                    );
                                    sources
                                        .entry(key)
                                        .or_default()
                                        .entry(index)
                                        .or_default()
                                        .insert(DirectiveTargetPosition::ObjectField(
                                            field_definition_position.clone(),
                                        ));
                                }
                            }
                        }
                    }
                }
            }
            Ok(sources)
        })
    }
}

fn extract_executable_locations(source: &Node<DirectiveDefinition>) -> Vec<DirectiveLocation> {
    source
        .locations
        .iter()
        .filter(|location| EXECUTABLE_DIRECTIVE_LOCATIONS.contains(*location))
        .copied()
        // JS decided to sort by name to enforce some consistent order. Really, we just want a
        // stable order, but there's a test asserting alphabetical order, so we follow that.
        .sorted_by_key(|loc| loc.name())
        .collect()
}

fn location_string(locations: &[DirectiveLocation]) -> String {
    if locations.is_empty() {
        return "".to_string();
    }
    format!(
        "{} \"{}\"",
        if locations.len() == 1 {
            "location"
        } else {
            "locations"
        },
        locations.iter().join(", ")
    )
}
