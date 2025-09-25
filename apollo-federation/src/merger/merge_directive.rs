use apollo_compiler::Name;
use apollo_compiler::Node;
use apollo_compiler::ast::Argument;
use apollo_compiler::ast::Directive;
use apollo_compiler::ast::DirectiveDefinition;
use apollo_compiler::ast::DirectiveLocation;
use apollo_compiler::collections::HashSet;
use indexmap::IndexSet;
use itertools::Itertools;

use crate::error::FederationError;
use crate::merger::hints::HintCode;
use crate::merger::merge::Merger;
use crate::merger::merge::Sources;
use crate::merger::merge::map_sources;
use crate::schema::position::DirectiveDefinitionPosition;
use crate::schema::position::DirectiveTargetPosition;
use crate::schema::referencer::DirectiveReferencers;
use crate::supergraph::CompositionHint;
use crate::supergraph::EXECUTABLE_DIRECTIVE_LOCATIONS;

#[derive(Clone)]
pub(crate) struct AppliedDirectiveToMergeEntry {
    pub names: HashSet<Name>,
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
        self.applied_directives_to_merge
            .push(AppliedDirectiveToMergeEntry {
                names: names.into_iter().collect(),
                sources: directive_sources,
                dest,
            });
        Ok(())
    }

    fn merge_applied_directive<T>(
        &mut self,
        name: &Name,
        sources: &Sources<T>,
        dest: &DirectiveTargetPosition,
    ) -> Result<(), FederationError> {
        let Some(directive_in_supergraph) = self
            .merged_federation_directive_in_supergraph_by_directive_name
            .get(name)
        else {
            // Definition is missing, so we assume there is nothing to merge.
            return Ok(());
        };

        // Accumulate all positions of the directive in the source schemas
        let all_schema_referencers =
            sources
                .iter()
                .fold(DirectiveReferencers::default(), |mut acc, (idx, source)| {
                    if source.is_some()
                        && let Ok(drs) = self.subgraphs[*idx]
                            .schema()
                            .referencers()
                            .get_directive(name)
                    {
                        acc.extend(drs);
                    }
                    acc
                });

        for pos in all_schema_referencers.iter() {
            // In JS, there are several methods for checking if directive applications are the same, and the static
            // argument transforms are only applied for repeatable directives. In this version, we rely on the `Eq`
            // and `Hash` implementations of `Directive` to deduplicate applications, and the argument transforms
            // are applied up front so they are available in all locations.
            let mut directive_sources: Sources<Directive> = Default::default();
            let directive_counts = sources
                .iter()
                .flat_map(|(idx, source)| {
                    if source.is_some() {
                        let directives = Self::directive_applications_with_transformed_arguments(
                            &pos,
                            directive_in_supergraph,
                            &self.subgraphs[*idx],
                        );
                        directive_sources.insert(*idx, directives.first().cloned());
                        directives
                    } else {
                        vec![]
                    }
                })
                .counts();

            if directive_in_supergraph.definition.repeatable {
                for directive in directive_counts.keys() {
                    dest.insert_directive(&mut self.merged, (*directive).clone())?;
                }
            } else if directive_counts.len() == 1 {
                let only_application = directive_counts.iter().next().unwrap().0.clone();
                dest.insert_directive(&mut self.merged, only_application)?;
            } else if let Some(merger) = &directive_in_supergraph.arguments_merger {
                // When we have multiple unique applications of the directive, and there is a
                // supplied argument merger, then we merge each of the arguments into a combined
                // directive.
                let mut merged_directive = Directive::new(name.clone());
                for arg_def in &directive_in_supergraph.definition.arguments {
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
                dest.insert_directive(&mut self.merged, merged_directive)?;
                self.error_reporter.add_hint(CompositionHint {
                    code: HintCode::MergedNonRepeatableDirectiveArguments.code().to_string(),
                    message: format!(
                        "Directive @{name} is applied to \"{pos}\" in multiple subgraphs with different arguments. Merging strategies used by arguments: {}",
                        directive_in_supergraph.arguments_merger.as_ref().map_or("undefined".to_string(), |m| (m.to_string)())
                    ),
                    locations: Default::default(), // PORT_NOTE: No locations in JS implementation.
                });
            } else if let Some(most_used_directive) = directive_counts
                .into_iter()
                .max_by_key(|(_, count)| *count)
                .map(|(directive, _)| directive)
            {
                // When there is no argument merger, we use the application appearing in the most
                // subgraphs. Adding it to the destination here allows the error reporter to
                // determine which one we selected when it's looking through the sources.
                dest.insert_directive(&mut self.merged, most_used_directive.clone())?;
                fn print_arguments(elt: &Directive) -> Option<String> {
                    if elt.arguments.is_empty() {
                        Some("no arguments".to_string())
                    } else {
                        Some(format!(
                            "arguments: [{}]",
                            elt.arguments
                                .iter()
                                .map(|arg| format!("{}: {}", arg.name, arg.value))
                                .join(", ")
                        ))
                    }
                }
                self.error_reporter.report_mismatch_hint::<Directive, Directive, ()>(
                    HintCode::InconsistentNonRepeatableDirectiveArguments,
                    format!("Non-repeatable directive @{name} is applied to \"{pos}\" in multiple subgraphs but with incompatible arguments. "),
                    &most_used_directive,
                    &directive_sources,
                    print_arguments,
                    |d, _| print_arguments(d),
                    |application, subgraphs| format!("The supergraph will use {} (from {}), but found ", application, subgraphs.unwrap_or_else(|| "undefined".to_string())),
                    |application, subgraphs| format!("{application} in {subgraphs}"),
                    false,
                    false,
                );
            }
        }

        Ok(())
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
                .filter_map(|(idx, subgraph)| {
                    subgraph
                        .schema()
                        .get_directive_definition(name)
                        .map(|def| (idx, Some(def)))
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

        let supergraph_dest = dest.get(self.merged.schema())?.clone();

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
                    format!("Executable directive \"{name}\" will not be part of the supergraph as it does not appear in all subgraphs: "),
                    dest,
                    sources,
                    |_elt| Some("yes".to_string()),
                    |_elt, _idx| Some("yes".to_string()),
                    |_, subgraphs| format!("it is defined in {}", subgraphs.unwrap_or_default()),
                    |_, subgraphs| format!(" but not in {subgraphs}"),
                    false,
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
            if locations.is_empty() {
                locations = source_locations;
            } else {
                if locations != source_locations {
                    inconsistent_locations = true;
                }
                locations.retain(|loc| source_locations.contains(loc));

                if locations.is_empty() {
                    self.error_reporter.report_mismatch_hint::<Node<DirectiveDefinition>, DirectiveDefinitionPosition, ()>(
                        HintCode::NoExecutableDirectiveLocationsIntersection,
                        format!("Executable directive \"{name}\" has no location that is common to all subgraphs: "),
                        &supergraph_dest,
                        sources,
                        |elt| Some(location_string(&extract_executable_locations(elt))),
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
        dest.add_locations(&mut self.merged, &locations)?;

        self.merge_description(sources, dest)?;

        if inconsistent_repeatable {
            self.error_reporter.report_mismatch_hint::<Node<DirectiveDefinition>, DirectiveDefinitionPosition, ()>(
                HintCode::InconsistentExecutableDirectiveRepeatable,
                format!("Executable directive \"{name}\" will not be marked repeatable in the supergraph as it is inconsistently marked repeatable in subgraphs: "),
                &supergraph_dest,
                sources,
                |elt| if elt.repeatable { Some("yes".to_string()) } else { Some("no".to_string()) },
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
                    "Executable directive \"{name}\" has inconsistent locations across subgraphs: "
                ),
                &supergraph_dest,
                sources,
                |elt| Some(location_string(&extract_executable_locations(elt))),
                |pos, idx| pos.try_get(self.subgraphs[idx].schema().schema()).map(|elt| location_string(&extract_executable_locations(elt))),
                |_, _subgraphs| {
                    "it will not appear in the supergraph as there no intersection between "
                        .to_string()
                },
                |locs, subgraphs| format!("{locs} in {subgraphs}"),
                false,
                false,
            );
        }

        // Doing args last, mostly so we don't bother adding if the directive doesn't make it in.
        self.add_arguments_shallow(sources, dest)?;
        for arg in &supergraph_dest.arguments {
            let subgraph_args = map_sources(sources, |src| {
                src.as_ref().map(|src| src.argument(arg.name.clone()))
            });
            self.merge_argument(&subgraph_args, &dest.argument(arg.name.clone()))?;
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
            // There is some cases where we had to call the method that records directives to merged
            // on a `dest` that ended up being removed from the ouptut (typically because we needed
            // to known if that `dest` was @inaccessible before deciding if it should be kept or
            // not). So check that the `dest` is still there (still "attached") and skip it entirely
            // otherwise.
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
    // Note: I don't think the sort order matters here so long as it's consistent
    source
        .locations
        .iter()
        .filter(|location| EXECUTABLE_DIRECTIVE_LOCATIONS.contains(*location))
        .copied()
        .sorted_by_key(|loc| {
            EXECUTABLE_DIRECTIVE_LOCATIONS
                .get_index_of(loc)
                .unwrap_or(usize::MAX)
        })
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
