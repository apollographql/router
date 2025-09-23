use std::fmt::Display;

use apollo_compiler::Name;
use apollo_compiler::Node;
use apollo_compiler::ast::InputValueDefinition;
use apollo_compiler::ast::Type;
use apollo_compiler::ast::Value;
use indexmap::IndexMap;
use indexmap::IndexSet;

use crate::error::CompositionError;
use crate::error::FederationError;
use crate::error::Locations;
use crate::merger::hints::HintCode;
use crate::merger::merge::Merger;
use crate::merger::merge::Sources;
use crate::merger::merge_field::PLACEHOLDER_TYPE_NAME;
use crate::schema::FederationSchema;
use crate::supergraph::CompositionHint;
use crate::utils::human_readable::human_readable_subgraph_names;

pub(crate) trait HasArguments {
    type ArgumentPosition;

    fn argument_position(&self, name: Name) -> Self::ArgumentPosition;

    fn get_argument<'schema>(
        &self,
        schema: &'schema FederationSchema,
        name: &Name,
    ) -> Option<&'schema Node<InputValueDefinition>>;

    fn get_arguments<'schema>(
        &self,
        schema: &'schema FederationSchema,
    ) -> Result<&'schema Vec<Node<InputValueDefinition>>, FederationError>;

    fn insert_argument(
        &self,
        schema: &mut FederationSchema,
        arg: Node<InputValueDefinition>,
    ) -> Result<(), FederationError>;

    fn remove_argument(
        &self,
        schema: &mut FederationSchema,
        name: &Name,
    ) -> Result<(), FederationError>;
}

pub(crate) trait HasDefaultValue {
    fn is_input_field() -> bool;

    fn get_default_value<'schema>(
        &self,
        schema: &'schema FederationSchema,
    ) -> Option<&'schema Node<Value>>;

    fn set_default_value(
        &self,
        schema: &mut FederationSchema,
        default: Option<Node<Value>>,
    ) -> Result<(), FederationError>;
}

impl Merger {
    pub(in crate::merger) fn add_arguments_shallow<T>(
        &mut self,
        sources: &Sources<T>,
        dest: &T,
    ) -> Result<(), FederationError>
    where
        T: HasArguments + Display,
        <T as HasArguments>::ArgumentPosition: Display,
    {
        let mut arg_names: IndexSet<Name> = IndexSet::new();
        for (idx, source) in sources.iter() {
            let Some(pos) = source else {
                continue;
            };
            let schema = self.subgraphs[*idx].schema();
            for arg in pos.get_arguments(schema)? {
                arg_names.insert(arg.name.clone());
            }
        }

        for arg_name in arg_names {
            // We add the argument unconditionally even if we're going to remove it later on.
            // This enables consistent mismatch/hint reporting.
            dest.insert_argument(
                &mut self.merged,
                Node::new(InputValueDefinition {
                    description: None,
                    name: arg_name.clone(),
                    default_value: None,
                    ty: Node::new(Type::Named(PLACEHOLDER_TYPE_NAME)),
                    directives: Default::default(),
                }),
            )?;
            let dest_arg_pos = dest.argument_position(arg_name.clone());

            // Record whether the argument comes from context in each subgraph.
            let mut is_contextual_in_subgraph: IndexMap<usize, bool> = Default::default();
            for (idx, source) in sources.iter() {
                let Some(pos) = source else {
                    continue;
                };
                let subgraph = &self.subgraphs[*idx];
                let arg_opt = pos.get_argument(subgraph.schema(), &arg_name);

                if let Some(arg) = arg_opt
                    && let Ok(Some(from_context)) = subgraph.from_context_directive_name()
                    && arg.directives.iter().any(|d| d.name == from_context)
                {
                    is_contextual_in_subgraph.insert(*idx, true);
                } else {
                    is_contextual_in_subgraph.insert(*idx, false);
                }
            }

            if is_contextual_in_subgraph
                .values()
                .any(|&contextual| contextual)
            {
                // If the argument is contextual in some subgraph, other subgraphs must also treat
                // it as contextual, unless it is nullable.
                for (idx, is_contextual) in is_contextual_in_subgraph.iter() {
                    if *is_contextual {
                        continue;
                    }
                    let Some(Some(pos)) = sources.get(idx) else {
                        continue;
                    };
                    let subgraph = &self.subgraphs[*idx];
                    if let Some(arg) = pos.get_argument(subgraph.schema(), &arg_name) {
                        if arg.is_required() && arg.default_value.is_none() {
                            self.error_reporter.add_error(CompositionError::ContextualArgumentNotContextualInAllSubgraphs {
                                message: format!(
                                    "Argument \"{dest_arg_pos}\" is contextual in at least one subgraph but in \"{pos}\" it does not have @fromContext, is not nullable and has no default value.",
                                ),
                                locations: subgraph.node_locations(arg),
                            });
                        } else {
                            self.error_reporter.add_hint(CompositionHint {
                                code: HintCode::ContextualArgumentNotContextualInAllSubgraphs
                                    .code()
                                    .to_string(),
                                message: format!(
                                    "Contextual argument \"{pos}\" will not be included in the supergraph since it is contextual in at least one subgraph",
                                ),
                                locations: subgraph.node_locations(arg),
                            });
                        }
                    }
                }
                // Note: we remove the element after the hint/error because we access it in
                // the hint message generation.
                dest.remove_argument(&mut self.merged, &arg_name)?;
                continue;
            }

            // If some subgraphs defining the field don’t define this argument, we cannot keep it in the supergraph.
            let mut present_in: Vec<usize> = Vec::new();
            let mut missing_in: Vec<usize> = Vec::new();
            let mut required_in: Vec<usize> = Vec::new();
            let mut locations: Locations = Vec::new();
            for (idx, source) in sources.iter() {
                let Some(pos) = source else {
                    continue;
                };
                let subgraph = &self.subgraphs[*idx];
                if let Some(arg) = pos.get_argument(subgraph.schema(), &arg_name) {
                    present_in.push(*idx);
                    if arg.is_required() {
                        required_in.push(*idx);
                        locations.extend(subgraph.node_locations(arg));
                    }
                } else {
                    missing_in.push(*idx);
                }
            }

            if !missing_in.is_empty() {
                // If a required argument is missing in a subgraph, we fail composition. If it is
                // not required, we remove it from the supergraph and add a hint.
                if !required_in.is_empty() {
                    let non_optional =
                        human_readable_subgraph_names(required_in.iter().map(|i| &self.names[*i]));
                    let missing =
                        human_readable_subgraph_names(missing_in.iter().map(|i| &self.names[*i]));
                    self.error_reporter.add_error(CompositionError::RequiredArgumentMissingInSomeSubgraph {
                        message: format!(
                            "Argument \"{dest_arg_pos}\" is required in some subgraphs but does not appear in all subgraphs: it is required in {non_optional} but does not appear in {missing}",
                        ),
                        locations,
                    });
                } else {
                    let arg_sources: Sources<_> = sources
                        .iter()
                        .map(|(idx, source)| {
                            let pos_opt = source
                                .as_ref()
                                .map(|pos| pos.argument_position(arg_name.clone()));
                            (*idx, pos_opt)
                        })
                        .collect();

                    self.error_reporter.report_mismatch_hint::<T::ArgumentPosition, T::ArgumentPosition, ()>(
                        HintCode::InconsistentArgumentPresence,
                        format!(
                            "Optional argument \"{}\" will not be included in the supergraph as it does not appear in all subgraphs: ",
                            dest_arg_pos
                        ),
                        &dest_arg_pos,
                        &arg_sources,
                        |_elt| Some("yes".to_string()),
                        |_elt, _| Some("yes".to_string()),
                        |_, subgraphs| format!("it is defined in {}", subgraphs.unwrap_or_default()),
                        |_, subgraphs| format!(" but not in {}", subgraphs),
                        true,
                        false,
                    );
                }

                // Note that we remove the element after the hint/error because we
                // access it in the hint message generation.
                dest.remove_argument(&mut self.merged, &arg_name)?;
            }
        }

        Ok(())
    }

    pub(in crate::merger) fn merge_default_value<T>(
        &mut self,
        sources: &Sources<T>,
        dest: &T,
    ) -> Result<(), FederationError>
    where
        T: Display + HasDefaultValue,
    {
        let mut dest_default: Option<Node<Value>> = None;
        let mut locations: Locations = Vec::with_capacity(sources.len());
        let mut has_seen_source = false;
        let mut is_inconsistent = false;
        let mut is_incompatible = false;

        // Because default values are always in input/contra-variant positions, we use an intersection strategy. Namely,
        // the result only has a default if _all_ have a default (which has to be the same, but we error if we found
        // 2 different defaults no matter what). Essentially, an argument/input field can only be made optional
        // in the supergraph API if it is optional in all subgraphs, or we may query a subgraph that expects the
        // value to be provided when it isn't. Note that an alternative could be to use an union strategy instead
        // but have the router/gateway fill in the default for subgraphs that don't know it, but that imply parsing
        // all the subgraphs fetches and we probably don't want that.
        for (idx, source_pos) in sources.iter() {
            let Some(pos) = source_pos else { continue };
            let subgraph = &self.subgraphs[*idx];
            let source_default = pos.get_default_value(subgraph.schema()).inspect(|v| {
                locations.extend(subgraph.node_locations(v));
            });

            match &dest_default {
                None => {
                    // Note that we set dest_default even if we have seen a source before and maybe thus be inconsistent.
                    // We won't use that value later if we're inconsistent, but keeping it allows us to always error out
                    // if we any 2 incompatible defaults.
                    dest_default = source_default.cloned();
                    // dest_default may be undefined either because we haven't seen any source (having the argument)
                    // or because we've seen one but that source had no default. In the later case (`hasSeenSource`),
                    // if the new source _has_ a default, then we're inconsistent.
                    if has_seen_source && source_default.is_some() {
                        is_inconsistent = true;
                    }
                }
                Some(current) => {
                    if source_default.is_none_or(|next| **next != **current) {
                        is_inconsistent = true;
                        // It's only incompatible if neither is undefined
                        if source_default.is_some() {
                            is_incompatible = true;
                        }
                    }
                }
            }
            has_seen_source = true;
        }

        // Note that we set the default if is_incompatible mostly to help the building of the error message. But
        // as we'll error out, it doesn't really matter.
        if !is_inconsistent || is_incompatible {
            dest.set_default_value(&mut self.merged, dest_default.clone())?;
        }

        let Some(dest_default) = &dest_default else {
            // If this is `None`, then all subgraphs have no default, and thus everything is consistent.
            return Ok(());
        };

        if is_incompatible {
            self.error_reporter.report_mismatch_error::<Node<Value>, T, ()>(
                if T::is_input_field() {
                    CompositionError::InputFieldDefaultMismatch {
                        message: format!(
                            "Input field \"{dest}\" has incompatible default values across subgraphs: it has ",
                        ),
                        locations
                    }
                } else {
                    CompositionError::ArgumentDefaultMismatch {
                        message: format!(
                            "Argument \"{dest}\" has incompatible default values across subgraphs: it has ",
                        ),
                        locations
                    }
                },
                dest_default,
                sources,
                |v| Some(format!("default value {v}")),
                |pos, idx| {
                    pos.get_default_value(self.subgraphs[idx].schema())
                        .map(|v| v.to_string())
                },
            );
        } else if is_inconsistent {
            self.error_reporter.report_mismatch_hint::<Node<Value>, T, ()>(
                HintCode::InconsistentDefaultValuePresence,
                format!(
                    "Argument \"{}\" has a default value in only some subgraphs: ",
                    dest
                ),
                dest_default,
                sources,
                |v| Some(format!("default value {v}")),
                |pos, idx| todo!(),
                |_, subgraphs| {
                    format!(
                        "will not use a default in the supergraph (there is no default in {}) but ",
                        subgraphs.unwrap_or_default()
                    )
                },
                |elt, subgraphs| format!("\"{}\" has default value {} in {}", dest, elt, subgraphs),
                false,
                false,
            );
        }

        Ok(())
    }
}
