use std::collections::HashMap;
use std::fmt::Display;
use std::ops::Range;

use apollo_compiler::parser::LineColumn;

use crate::error::CompositionError;
use crate::error::SingleFederationError;
use crate::error::SubgraphLocation;
use crate::merger::hints::HintCode;
use crate::merger::merge::Sources;
use crate::supergraph::CompositionHint;
use crate::utils::human_readable::JoinStringsOptions;
use crate::utils::human_readable::human_readable_subgraph_names;
use crate::utils::human_readable::join_strings;

pub(crate) struct ErrorReporter {
    errors: Vec<CompositionError>,
    hints: Vec<CompositionHint>,
    names: Vec<String>,
}

impl ErrorReporter {
    pub(crate) fn new(names: Vec<String>) -> Self {
        Self {
            errors: Vec::new(),
            hints: Vec::new(),
            names,
        }
    }

    #[allow(dead_code)]
    pub(crate) fn add_subgraph_error(
        &mut self,
        name: &str,
        error: impl Into<SingleFederationError>,
        locations: Vec<Range<LineColumn>>,
    ) {
        let error = CompositionError::SubgraphError {
            subgraph: name.into(),
            error: error.into(),
            locations: locations
                .iter()
                .map(|range| SubgraphLocation {
                    subgraph: name.into(),
                    range: range.clone(),
                })
                .collect(),
        };
        self.errors.push(error);
    }

    pub(crate) fn add_error(&mut self, error: CompositionError) {
        self.errors.push(error);
    }

    pub(crate) fn add_hint(&mut self, hint: CompositionHint) {
        self.hints.push(hint);
    }

    pub(crate) fn has_hints(&self) -> bool {
        !self.hints.is_empty()
    }

    pub(crate) fn has_errors(&self) -> bool {
        !self.errors.is_empty()
    }

    pub(crate) fn into_errors_and_hints(self) -> (Vec<CompositionError>, Vec<CompositionHint>) {
        (self.errors, self.hints)
    }

    pub(crate) fn report_mismatch_error<D: Display, S, L>(
        &mut self,
        error: CompositionError,
        mismatched_element: &D,
        subgraph_elements: &Sources<S>,
        supergraph_mismatch_accessor: impl Fn(&D) -> Option<String>,
        subgraph_mismatch_accessor: impl Fn(&S, usize) -> Option<String>,
    ) {
        self.report_mismatch::<D, S, L>(
            Some(mismatched_element),
            subgraph_elements,
            supergraph_mismatch_accessor,
            subgraph_mismatch_accessor,
            |elt, names| format!("{} in {}", elt, names.unwrap_or("undefined".to_string())),
            |elt, names| format!("{elt} in {names}"),
            |myself, distribution, _| {
                let distribution_str = join_strings(
                    distribution.iter(),
                    JoinStringsOptions {
                        first_separator: Some(" but "),
                        separator: " and ",
                        last_separator: Some(" and "),
                        output_length_limit: None,
                    },
                );
                myself.add_error(error.append_message(distribution_str));
            },
            false,
        );
    }

    pub(crate) fn report_mismatch_error_without_supergraph<T: Display, U>(
        &mut self,
        error: CompositionError,
        subgraph_elements: &Sources<T>,
        mismatch_accessor: impl Fn(&T, usize) -> Option<String>,
    ) {
        self.report_mismatch::<String, T, U>(
            None,
            subgraph_elements,
            |_| None,
            mismatch_accessor,
            |_, _| String::new(),
            |elt, names| format!("{elt} in {names}"),
            |myself, distribution, _: Vec<U>| {
                let distribution_str = join_strings(
                    distribution.iter(),
                    JoinStringsOptions {
                        first_separator: Some(" but "),
                        separator: " and ",
                        last_separator: Some(" and "),
                        output_length_limit: None,
                    },
                );
                myself.add_error(error.append_message(distribution_str));
            },
            false,
        );
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn report_mismatch_hint<D: Display, S, L>(
        &mut self,
        code: HintCode,
        message: String,
        supergraph_element: &D,
        subgraph_elements: &Sources<S>,
        supergraph_element_to_string: impl Fn(&D) -> Option<String>,
        subgraph_element_to_string: impl Fn(&S, usize) -> Option<String>,
        supergraph_element_printer: impl Fn(&str, Option<String>) -> String,
        other_elements_printer: impl Fn(&str, &str) -> String,
        include_missing_sources: bool,
        no_end_of_message_dot: bool,
    ) {
        self.report_mismatch::<D, S, L>(
            Some(supergraph_element),
            subgraph_elements,
            supergraph_element_to_string,
            subgraph_element_to_string,
            supergraph_element_printer,
            other_elements_printer,
            |myself, distribution, _| {
                let distribution_str = join_strings(
                    distribution.iter(),
                    JoinStringsOptions {
                        first_separator: Some(" and "),
                        separator: ", ",
                        last_separator: Some(" but "),
                        output_length_limit: None,
                    },
                );
                let suffix = if no_end_of_message_dot { "" } else { "." };
                myself.add_hint(CompositionHint {
                    code: code.code().to_string(),
                    message: format!("{message}{distribution_str}{suffix}"),
                    locations: Default::default(), // TODO
                });
            },
            include_missing_sources,
        );
    }

    /// Reports a mismatch between a supergraph element and subgraph elements.
    /// Not meant to be used directly: use `report_mismatch_error` or `report_mismatch_hint` instead.
    ///
    /// TODO: The generic parameter `L` is meant to represent AST nodes (or locations) that are attached to error messages.
    /// When we decide on an implementation for those, they should be added to `ast_nodes` below.
    #[allow(clippy::too_many_arguments)]
    fn report_mismatch<D: Display, S, L>(
        &mut self,
        supergraph_element: Option<&D>,
        subgraph_elements: &Sources<S>,
        // Note that these two parameters used to be `mismatchAccessor`, which took a boolean
        // indicating whether it was a supergraph element or a subgraph element. Now, we have two
        // separate functions, which allows us to use different types for the destination and
        // source data.
        supergraph_mismatch_accessor: impl Fn(&D) -> Option<String>,
        subgraph_mismatch_accessor: impl Fn(&S, usize) -> Option<String>,
        supergraph_element_printer: impl Fn(&str, Option<String>) -> String,
        other_elements_printer: impl Fn(&str, &str) -> String,
        reporter: impl FnOnce(&mut Self, Vec<String>, Vec<L>),
        include_missing_sources: bool,
    ) {
        let mut distribution_map = HashMap::new();
        #[allow(unused_mut)] // We need this to be mutable when we decide how to handle AST nodes
        let mut locations: Vec<L> = Vec::new();
        let process_subgraph_element =
            |name: &str,
             idx: usize,
             subgraph_element: &S,
             distribution_map: &mut HashMap<String, Vec<String>>| {
                if let Some(element) = subgraph_mismatch_accessor(subgraph_element, idx) {
                    distribution_map
                        .entry(element)
                        .or_default()
                        .push(name.to_string());
                }
                // TODO: Get AST node equivalent and push onto `locations`
            };
        if include_missing_sources {
            for (i, name) in self.names.iter().enumerate() {
                if let Some(Some(subgraph_element)) = subgraph_elements.get(&i) {
                    process_subgraph_element(name, i, subgraph_element, &mut distribution_map);
                } else {
                    distribution_map
                        .entry("".to_string())
                        .or_default()
                        .push(name.to_string());
                }
            }
        } else {
            for (i, name) in self.names.iter().enumerate() {
                if let Some(Some(subgraph_element)) = subgraph_elements.get(&i) {
                    process_subgraph_element(name, i, subgraph_element, &mut distribution_map);
                }
            }
        }
        let supergraph_mismatch = supergraph_element
            .and_then(supergraph_mismatch_accessor)
            .unwrap_or_default();
        assert!(
            distribution_map.len() > 1,
            "Should not have been called for {}",
            supergraph_element
                .map(|elt| elt.to_string())
                .unwrap_or_else(|| "undefined".to_string())
        );
        let mut distribution = Vec::new();
        let subgraphs_like_supergraph = distribution_map.get(&supergraph_mismatch);
        // We always add the "supergraph" first (proper formatting of hints rely on this in particular)
        distribution.push(supergraph_element_printer(
            &supergraph_mismatch,
            subgraphs_like_supergraph.map(|s| human_readable_subgraph_names(s.iter())),
        ));
        for (v, names) in distribution_map.iter() {
            if *v == supergraph_mismatch {
                continue; // Skip the supergraph element as it's already added
            }
            let names_str = human_readable_subgraph_names(names.iter());
            distribution.push(other_elements_printer(v, &names_str));
        }
        reporter(self, distribution, locations);
    }
}
