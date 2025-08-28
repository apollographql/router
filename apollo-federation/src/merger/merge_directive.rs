use apollo_compiler::Name;
use apollo_compiler::Node;
use apollo_compiler::ast::Argument;
use apollo_compiler::ast::Directive;
use apollo_compiler::ast::DirectiveDefinition;
use itertools::Itertools;

use crate::error::FederationError;
use crate::merger::hints::HintCode;
use crate::merger::merge::Merger;
use crate::merger::merge::Sources;
use crate::schema::FederationSchema;
use crate::schema::referencer::DirectiveReferencers;
use crate::subgraph::typestate::Subgraph;
use crate::subgraph::typestate::Validated;
use crate::supergraph::CompositionHint;

impl Merger {
    #[allow(dead_code)]
    fn merge_applied_directive(
        &mut self,
        name: &Name,
        sources: Sources<Subgraph<Validated>>,
        dest: &mut FederationSchema,
    ) -> Result<(), FederationError> {
        let Some(directive_in_supergraph) = self
            .merged_federation_directive_in_supergraph_by_directive_name
            .get(name)
        else {
            // Definition is missing, so we assume there is nothing to merge.
            return Ok(());
        };

        // Accumulate all positions of the directive in the source schemas
        let all_schema_referencers = sources
            .values()
            .filter_map(|subgraph| subgraph.as_ref())
            .fold(DirectiveReferencers::default(), |mut acc, subgraph| {
                if let Ok(drs) = subgraph.schema().referencers().get_directive(name) {
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
                .flat_map(|(idx, subgraph)| {
                    if let Some(subgraph) = subgraph {
                        let directives = Self::directive_applications_with_transformed_arguments(
                            &pos,
                            directive_in_supergraph,
                            subgraph,
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
                    pos.insert_directive(dest, (*directive).clone())?;
                }
            } else if directive_counts.len() == 1 {
                let only_application = directive_counts.iter().next().unwrap().0.clone();
                pos.insert_directive(dest, only_application)?;
            } else if let Some(merger) = &directive_in_supergraph.arguments_merger {
                // When we have multiple unique applications of the directive, and there is a
                // supplied argument merger, then we merge each of the arguments into a combined
                // directive.
                let mut merged_directive = Directive::new(name.clone());
                for arg_def in &directive_in_supergraph.definition.arguments {
                    let values = directive_counts
                        .keys()
                        .filter_map(|d| {
                            d.specified_argument_by_name(name)
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
                pos.insert_directive(dest, merged_directive)?;
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
                pos.insert_directive(dest, most_used_directive.clone())?;
                self.error_reporter.report_mismatch_hint::<Directive, ()>(
                    HintCode::InconsistentNonRepeatableDirectiveArguments,
                    format!("Non-repeatable directive @{name} is applied to \"{pos}\" in multiple subgraphs but with incompatible arguments. "),
                    &most_used_directive,
                    &directive_sources,
                    |elt, _| if elt.arguments.is_empty() {
                        Some("no arguments".to_string())
                    } else {
                        Some(format!("arguments: [{}]", elt.arguments.iter().map(|arg| format!("{}: {}", arg.name, arg.value)).join(", ")))
                    },
                    |application, subgraphs| format!("The supergraph will use {} (from {}), but found ", application, subgraphs.unwrap_or_else(|| "undefined".to_string())),
                    |application, subgraphs| format!("{application} in {subgraphs}"),
                    None::<fn(Option<&Directive>) -> bool>,
                    false,
                    false,
                );
            }
        }

        Ok(())
    }
    
    pub(crate) fn merge_directive_definition(&mut self, name: Name) -> Result<(), FederationError> {
        todo!("Implement merge_directive_definition")
    }
}
