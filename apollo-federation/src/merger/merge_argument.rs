use std::collections::HashSet;
use std::fmt::Display;

use apollo_compiler::Name;
use apollo_compiler::Node;
use apollo_compiler::ast::InputValueDefinition;
use apollo_compiler::ast::Type;
use apollo_compiler::ast::Value;
use indexmap::IndexMap;
use indexmap::IndexSet;
use tracing::instrument;
use tracing::trace;

use crate::error::CompositionError;
use crate::error::FederationError;
use crate::error::Locations;
use crate::merger::hints::HintCode;
use crate::merger::merge::Merger;
use crate::merger::merge::Sources;
use crate::schema::FederationSchema;
use crate::schema::position::DirectiveTargetPosition;
use crate::schema::position::HasDescription;
use crate::schema::position::HasType;
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
    #[instrument(skip(self, sources))]
    pub(in crate::merger) fn add_arguments_shallow<T>(
        &mut self,
        sources: &Sources<T>,
        dest: &T,
    ) -> Result<IndexSet<Name>, FederationError>
    where
        T: HasArguments + std::fmt::Debug + Display,
        <T as HasArguments>::ArgumentPosition: Display,
    {
        let mut arg_types: IndexMap<Name, Node<Type>> = Default::default();
        let mut removed_args = HashSet::new();
        for (idx, source) in sources.iter() {
            let Some(pos) = source else {
                continue;
            };
            let schema = self.subgraphs[*idx].schema();
            for arg in pos.get_arguments(schema)? {
                arg_types.insert(arg.name.clone(), arg.ty.clone());
            }
        }

        for (arg_name, arg_type) in &arg_types {
            // We add the argument unconditionally even if we're going to remove it later on.
            // This enables consistent mismatch/hint reporting.
            trace!("Inserting shallow definition for argument \"{arg_name}\" in \"{dest}\"");
            if dest.get_argument(&self.merged, arg_name).is_none() {
                dest.insert_argument(
                    &mut self.merged,
                    Node::new(InputValueDefinition {
                        description: None,
                        name: arg_name.clone(),
                        default_value: None,
                        ty: arg_type.clone(),
                        directives: Default::default(),
                    }),
                )?;
            }

            let dest_arg_pos = dest.argument_position(arg_name.clone());

            // Record whether the argument comes from context in each subgraph.
            let mut is_contextual_in_subgraph: IndexMap<usize, bool> = Default::default();
            for (idx, source) in sources.iter() {
                let Some(pos) = source else {
                    continue;
                };
                let subgraph = &self.subgraphs[*idx];
                let arg_opt = pos.get_argument(subgraph.schema(), arg_name);

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
                    if let Some(arg) = pos.get_argument(subgraph.schema(), arg_name) {
                        if arg.is_required() && arg.default_value.is_none() {
                            self.error_reporter.add_error(CompositionError::ContextualArgumentNotContextualInAllSubgraphs {
                                message: format!(
                                    "Argument \"{dest_arg_pos}\" is contextual in at least one subgraph but in \"{dest_arg_pos}\" it does not have @fromContext, is not nullable and has no default value.",
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
                dest.remove_argument(&mut self.merged, arg_name)?;
                removed_args.insert(arg_name.clone());
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
                if let Some(arg) = pos.get_argument(subgraph.schema(), arg_name) {
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
                            let pos_opt = source.as_ref().and_then(|pos| {
                                pos.get_argument(self.subgraphs[*idx].schema(), arg_name)
                            });
                            (*idx, pos_opt)
                        })
                        .collect();

                    self.error_reporter.report_mismatch_hint::<T::ArgumentPosition, &Node<InputValueDefinition>, ()>(
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
                dest.remove_argument(&mut self.merged, arg_name)?;
                removed_args.insert(arg_name.clone());
            }
        }

        Ok(arg_types
            .into_keys()
            .filter(|n| !removed_args.contains(n))
            .collect())
    }

    #[instrument(skip(self, sources, dest))]
    pub(in crate::merger) fn merge_argument<T>(
        &mut self,
        sources: &Sources<T>,
        dest: &T,
    ) -> Result<(), FederationError>
    where
        T: Clone
            + Display
            + HasDefaultValue
            + HasDescription
            + HasType
            + Into<DirectiveTargetPosition>,
    {
        trace!("Merging argument \"{dest}\"");
        self.merge_description(sources, dest)?;
        self.record_applied_directives_to_merge(sources, dest)?;
        self.merge_type_reference(sources, dest, true)?;
        self.merge_default_value(sources, dest)?;
        Ok(())
    }

    pub(in crate::merger) fn merge_default_value<T>(
        &mut self,
        sources: &Sources<T>,
        dest: &T,
    ) -> Result<(), FederationError>
    where
        T: Display + HasDefaultValue + HasType,
    {
        trace!("Merging default value for \"{dest}\"");
        let mut dest_default: Option<Node<Value>> = None;
        let mut locations = Locations::with_capacity(sources.len());
        let mut has_seen_source = false;
        let mut is_inconsistent = false;
        let mut is_incompatible = false;

        // Get the target type for coercibility checking
        let target_type = dest.get_type(&self.merged)?;

        // Because default values are always in input/contra-variant positions, we use an intersection strategy. Namely,
        // the result only has a default if _all_ have a default (which has to be the same, but we error if we found
        // 2 different defaults no matter what). Essentially, an argument/input field can only be made optional
        // in the supergraph API if it is optional in all subgraphs, or we may query a subgraph that expects the
        // value to be provided when it isn't. Note that an alternative could be to use an union strategy instead
        // but have the router/gateway fill in the default for subgraphs that don't know it, but that implies parsing
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
                    // We have `&Node<Value>` here, so we need the double deref to get the values.
                    // Check if values are equivalent considering type coercibility (e.g., Int to Float)
                    if source_default.is_none_or(|next| {
                        !Self::are_default_values_equivalent(next, current, target_type)
                    }) {
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
            trace!("Setting merged default value for \"{dest}\" to {dest_default:?}");
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
                        message: format!("Input field \"{dest}\" has incompatible default values across subgraphs: it has "),
                        locations
                    }
                } else {
                    CompositionError::ArgumentDefaultMismatch {
                        message: format!("Argument \"{dest}\" has incompatible default values across subgraphs: it has "),
                        locations
                    }
                },
                dest_default,
                sources,
                |v| Some(format!("default value {v}")),
                |pos, idx| {
                    Some(pos.get_default_value(self.subgraphs[idx].schema())
                            .map(|v| format!("default value {v}"))
                            .unwrap_or_else(|| "no default value".to_string()))
                },
            );
        } else if is_inconsistent {
            self.error_reporter.report_mismatch_hint::<Node<Value>, T, ()>(
                HintCode::InconsistentDefaultValuePresence,
                format!("Argument \"{dest}\" has a default value in only some subgraphs: "),
                dest_default,
                sources,
                // When inconsistent, we set no default. So, the supergraph element should always
                // be "no default value". The matching strings drive the ordering in the message.
                |_| Some("no default value".to_string()),
                |pos, idx| Some(pos.get_default_value(self.subgraphs[idx].schema())
                            .map(|v| v.to_string())
                            .unwrap_or_else(|| "no default value".to_string())),
                |_, subgraphs| {
                    let subgraphs = subgraphs.unwrap_or_default();
                    format!("will not use a default in the supergraph (there is no default in {subgraphs}) but ")
                },
                |elt, subgraphs| format!("\"{dest}\" has default value {elt} in {subgraphs}"),
                false,
                false,
            );
        }

        Ok(())
    }

    /// Check if two default values are equivalent, considering type coercibility.
    /// For example, Int value 200 is equivalent to Float value 200.0 when the target type is Float.
    fn are_default_values_equivalent(value1: &Value, value2: &Value, target_type: &Type) -> bool {
        // TODO: This coercibility check should really come from `apollo_compiler`
        // First check for direct equality
        if value1 == value2 {
            return true;
        }

        // Check for Int to Float coercibility
        // According to GraphQL spec, Int values can be coerced to Float
        if target_type.inner_named_type() == "Float" {
            match (value1, value2) {
                // Int literal coercible to Float literal
                (Value::Int(int_val), Value::Float(float_val))
                | (Value::Float(float_val), Value::Int(int_val)) => {
                    let Ok(int_val_parsed) = int_val.try_to_f64() else {
                        return false;
                    };
                    let Ok(float_val_parsed) = float_val.try_to_f64() else {
                        return false;
                    };
                    int_val_parsed == float_val_parsed
                }
                _ => false,
            }
        } else {
            false
        }
    }
}
