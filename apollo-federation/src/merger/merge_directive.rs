use apollo_compiler::Name;
use apollo_compiler::Node;
use apollo_compiler::ast::Argument;
use apollo_compiler::ast::Directive;
use apollo_compiler::ast::DirectiveDefinition;
use apollo_compiler::ast::DirectiveLocation;
use apollo_compiler::collections::IndexMap;
use indexmap::IndexSet;
use itertools::Itertools;
use tracing::instrument;
use tracing::trace;

use crate::bail;
use crate::error::FederationError;
use crate::merger::hints::HintCode;
use crate::merger::merge::Merger;
use crate::merger::merge::Sources;
use crate::merger::merge::map_sources;
use crate::schema::position::DirectiveDefinitionPosition;
use crate::schema::position::DirectiveTargetPosition;
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

#[allow(dead_code)]
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
        let mut directive_sources: Sources<DirectiveTargetPosition> = Sources::default();
        let mut names = IndexSet::new();

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
    fn merge_applied_directive(
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

        // In JS, there are several methods for checking if directive applications are the same, and the static
        // argument transforms are only applied for repeatable directives. In this version, we rely on the `Eq`
        // and `Hash` implementations of `Directive` to deduplicate applications, and the argument transforms
        // are applied up front so they are available in all locations.
        let directive_counts: IndexMap<Directive, usize> = sources
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
                for application in &mut applications {
                    // When we deduplicate directives below, we want to treat applications which
                    // explicitly pass the default the same as applications which omit it.
                    self.fill_argument_defaults(application, &definition);
                }
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
                *acc.entry(directive).or_insert(0) += 1;
                acc
            });

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
                            .or(arg_def.default_value.as_ref())
                            .map(|v| v.as_ref())
                    })
                    .cloned()
                    .collect_vec();
                if let Some(merged_value) = (merger.merge)(name, &values)? {
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
            self.error_reporter.report_mismatch_hint::<Directive, DirectiveTargetPosition, ()>(
                    HintCode::InconsistentNonRepeatableDirectiveArguments,
                    format!("Non-repeatable directive @{name} is applied to \"{dest}\" in multiple subgraphs but with incompatible arguments. "),
                    &most_used_directive,
                    sources,
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

    fn fill_argument_defaults(&self, directive: &mut Directive, definition: &DirectiveDefinition) {
        let existing_arg_names = directive
            .arguments
            .iter()
            .map(|arg| arg.name.clone())
            .collect::<IndexSet<_>>();
        for arg_def in &definition.arguments {
            if !existing_arg_names.contains(&arg_def.name)
                && let Some(default_value) = &arg_def.default_value
            {
                let arg = Argument {
                    name: arg_def.name.clone(),
                    value: default_value.clone(),
                };
                directive.arguments.push(Node::new(arg));
            }
        }
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
        let Some(def) = self
            .compose_directive_manager
            .get_latest_directive_definition(name)?
        else {
            return Ok(());
        };

        let dest = DirectiveDefinitionPosition {
            directive_name: name.clone(),
        };
        // This replaces the calls to target.set_description, target.set_repeatable, and target.add_locations in the JS implementation
        dest.insert(&mut self.merged, def.clone())?;

        let sources = self
            .subgraphs
            .iter()
            .enumerate()
            .map(|(idx, subgraph)| (idx, subgraph.schema().get_directive_definition(name)))
            .collect();
        let arg_names = self.add_arguments_shallow(&sources, &dest)?;

        for arg_name in arg_names {
            let sources = map_sources(&sources, |source| {
                source.as_ref().map(|s| s.argument(arg_name.clone()))
            });
            let dest_arg = dest.argument(arg_name);
            self.merge_argument(&sources, &dest_arg)?;
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
                self.error_reporter.report_mismatch_hint::<DirectiveDefinitionPosition, DirectiveDefinitionPosition,()>(
                    HintCode::InconsistentExecutableDirectivePresence,
                    format!("Executable directive \"@{name}\" will not be part of the supergraph as it does not appear in all subgraphs: "),
                    dest,
                    sources,
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
                    self.error_reporter.report_mismatch_hint::<DirectiveDefinitionPosition, DirectiveDefinitionPosition, ()>(
                        HintCode::NoExecutableDirectiveLocationsIntersection,
                        format!("Executable directive \"@{name}\" has no location that is common to all subgraphs: "),
                        dest,
                        sources,
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
            self.error_reporter.report_mismatch_hint::<Node<DirectiveDefinition>, DirectiveDefinitionPosition, ()>(
                HintCode::InconsistentExecutableDirectiveRepeatable,
                format!("Executable directive \"@{name}\" will not be marked repeatable in the supergraph as it is inconsistently marked repeatable in subgraphs: "),
                supergraph_dest,
                sources,
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
            self.error_reporter.report_mismatch_hint::<Node<DirectiveDefinition>, DirectiveDefinitionPosition, ()>(
                HintCode::InconsistentExecutableDirectiveLocations,
                format!(
                    "Executable directive \"@{name}\" has inconsistent locations across subgraphs "
                ),
                supergraph_dest,
                sources,
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
