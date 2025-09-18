use std::fmt::Display;

use apollo_compiler::Name;
use apollo_compiler::Node;
use apollo_compiler::ast::InputValueDefinition;
use apollo_compiler::ast::Type;
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
                            self.error_reporter.add_hint(CompositionHint {
                                code: HintCode::ContextualArgumentNotContextualInAllSubgraphs
                                    .code()
                                    .to_string(),
                                message: format!(
                                    "Contextual argument \"{dest_arg_pos}\" is contextual in at least one subgraph but in \"{pos}\" it does not have @fromContext, is not nullable and has no default value.",
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

                // Note that we remove the element after the hint/error because we
                // access it in the hint message generation.
                dest.remove_argument(&mut self.merged, &arg_name)?;
            }
        }

        Ok(())
    }
}
