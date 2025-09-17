use std::fmt::Display;

use apollo_compiler::Name;
use apollo_compiler::Node;
use apollo_compiler::ast::InputValueDefinition;
use apollo_compiler::ast::Type;
use indexmap::IndexMap;
use indexmap::IndexSet;

use crate::error::CompositionError;
use crate::error::FederationError;
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
        // We collect the union of argument names across all subgraphs that have the field.
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

        // Collect arguments which come from context per subgraph.
        let mut is_contextual_by_idx_arg: IndexMap<(usize, Name), bool> = Default::default();
        for (idx, source) in sources.iter() {
            let Some(pos) = source else {
                continue;
            };
            let subgraph = &self.subgraphs[*idx];
            let Ok(Some(from_context_name)) = subgraph.from_context_directive_name() else {
                continue;
            };
            let schema = subgraph.schema();
            for arg in pos.get_arguments(schema)? {
                let contextual = arg.directives.get(&from_context_name).is_some();
                is_contextual_by_idx_arg.insert((*idx, arg.name.clone()), contextual);
            }
        }

        // Add each argument to destination, then possibly remove per rules.
        for arg_name in arg_names {
            // We add the argument unconditionally even if we're going to remove it in some path. This enables consistent mismatch/hint reporting.
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

            // Build argument position for the destination for hint/error printing
            let dest_arg_pos = dest.argument_position(arg_name.clone());

            // If the argument is contextual in some subgraph, other subgraphs must also treat it as contextual,
            // unless it is nullable. Also, we remove it from the supergraph.
            let mut saw_contextual = false;
            let mut contextual_map: IndexMap<usize, bool> = Default::default();
            for (idx, source) in sources.iter() {
                let Some(pos) = source else {
                    continue;
                };
                let schema = self.subgraphs[*idx].schema();
                let arg_opt = pos.get_argument(schema, &arg_name);
                let mut contextual = false;
                if arg_opt.is_some() {
                    contextual = *is_contextual_by_idx_arg
                        .get(&(*idx, arg_name.clone()))
                        .unwrap_or(&false);
                }
                if contextual {
                    saw_contextual = true;
                }
                contextual_map.insert(*idx, contextual);
            }

            if saw_contextual {
                // If contextual in some subgraph, ensure others either are contextual or optional; otherwise hint.
                for (idx, is_contextual) in contextual_map.iter() {
                    if *is_contextual {
                        continue;
                    }
                    let Some(Some(pos)) = sources.get(idx) else {
                        continue;
                    };
                    let schema = self.subgraphs[*idx].schema();
                    if let Some(arg) = pos.get_argument(schema, &arg_name) {
                        if arg.is_required() && arg.default_value.is_none() {
                            // Hard error path in JS; we emit a hint for now to keep composition progressing
                            self.error_reporter.add_hint(CompositionHint {
                                code: HintCode::ContextualArgumentNotContextualInAllSubgraphs
                                    .code()
                                    .to_string(),
                                message: format!(
                                    "Contextual argument \"{dest_arg_pos}\" is contextual in at least one subgraph but in \"{pos}\" it does not have @fromContext, is not nullable and has no default value.",
                                ),
                                locations: Default::default(),
                            });
                        } else {
                            // Informational hint
                            self.error_reporter.add_hint(CompositionHint {
                                code: HintCode::ContextualArgumentNotContextualInAllSubgraphs
                                    .code()
                                    .to_string(),
                                message: format!(
                                    "Contextual argument \"{pos}\" will not be included in the supergraph since it is contextual in at least one subgraph",
                                ),
                                locations: Default::default(),
                            });
                        }
                    }
                }
                // Note: we remove the element after the hint/error because we access it in the hint message generation.
                dest.remove_argument(&mut self.merged, &arg_name)?;
                continue;
            }

            // If some subgraphs defining the field don’t define this argument, we cannot keep it in the supergraph.
            let mut some_missing = false;
            let mut present_in: Vec<usize> = Vec::new();
            let mut missing_in: Vec<usize> = Vec::new();
            let mut required_in: Vec<usize> = Vec::new();
            for (idx, source) in sources.iter() {
                let Some(pos) = source else {
                    continue;
                };
                let schema = self.subgraphs[*idx].schema();
                if let Some(arg) = pos.get_argument(schema, &arg_name) {
                    present_in.push(*idx);
                    // Determine if required in this source
                    if arg.is_required() {
                        required_in.push(*idx);
                    }
                } else {
                    missing_in.push(*idx);
                    some_missing = true;
                }
            }

            if some_missing {
                // Optional vs required handling:
                if !required_in.is_empty() {
                    // If the argument is mandatory in some subgraphs, fail composition.
                    let non_optional =
                        human_readable_subgraph_names(required_in.iter().map(|i| &self.names[*i]));
                    let missing =
                        human_readable_subgraph_names(missing_in.iter().map(|i| &self.names[*i]));
                    self.error_reporter.add_error(CompositionError::RequiredArgumentMissingInSomeSubgraph {
                        message: format!(
                            "Argument \"{dest_arg_pos}\" is required in some subgraphs but does not appear in all subgraphs: it is required in {non_optional} but does not appear in {missing}",
                        ),
                        locations: vec![], // TODO: add locations
                    });
                } else {
                    // If the argument is optional in all sources, compose properly but issue a hint.
                    let arg_sources: Sources<_> = sources
                        .iter()
                        .map(|(idx, source)| {
                            let pos_opt = source
                                .as_ref()
                                .map(|pos| pos.argument_position(arg_name.clone()));
                            (*idx, pos_opt)
                        })
                        .collect();

                    self.error_reporter.report_mismatch_hint::<T::ArgumentPosition, ()>(
                        HintCode::InconsistentArgumentPresence,
                        format!(
                            "Optional argument \"{}\" will not be included in the supergraph as it does not appear in all subgraphs: ",
                            dest_arg_pos
                        ),
                        &dest_arg_pos,
                        &arg_sources,
                        |_elt, _| Some("yes".to_string()),
                        |_, subgraphs| format!("it is defined in {}", subgraphs.unwrap_or_default()),
                        |_, subgraphs| format!(" but not in {}", subgraphs),
                        None::<fn(Option<&T::ArgumentPosition>) -> bool>,
                        true,
                        false,
                    );
                }

                // Note that we remove the element after the hint/error because we access it in the hint message generation.
                dest.remove_argument(&mut self.merged, &arg_name)?;
            }
        }

        Ok(())
    }
}
