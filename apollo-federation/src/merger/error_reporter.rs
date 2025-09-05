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

    #[allow(dead_code)]
    pub(crate) fn add_error(&mut self, error: CompositionError) {
        self.errors.push(error);
    }

    #[allow(dead_code)]
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

    pub(crate) fn report_mismatch_error<T: Display, U>(
        &mut self,
        error: CompositionError,
        mismatched_element: &T,
        subgraph_elements: &Sources<T>,
        mismatch_accessor: impl Fn(&T, bool) -> Option<String>,
    ) {
        self.report_mismatch(
            Some(mismatched_element),
            subgraph_elements,
            mismatch_accessor,
            |elt, names| format!("{} in {}", elt, names.unwrap_or("undefined".to_string())),
            |elt, names| format!("{elt} in {names}"),
            |myself, distribution, _: Vec<U>| {
                let distribution_str = join_strings(
                    distribution.iter(),
                    JoinStringsOptions {
                        first_separator: Some(" and "),
                        separator: ", ",
                        last_separator: Some(" but "),
                        output_length_limit: None,
                    },
                );
                myself.add_error(error.append_message(distribution_str));
            },
            Some(|elt: Option<&T>| elt.is_none()),
            false,
        );
    }

    pub(crate) fn report_mismatch_error_without_supergraph<T: Display, U>(
        &mut self,
        error: CompositionError,
        subgraph_elements: &Sources<T>,
        mismatch_accessor: impl Fn(&T, bool) -> Option<String>,
    ) {
        self.report_mismatch(
            None,
            subgraph_elements,
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
            Some(|elt: Option<&T>| elt.is_none()),
            false,
        );
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn report_mismatch_hint<T: Display, U>(
        &mut self,
        code: HintCode,
        message: String,
        supergraph_element: &T,
        subgraph_elements: &Sources<T>,
        element_to_string: impl Fn(&T, bool) -> Option<String>,
        supergraph_element_printer: impl Fn(&str, Option<String>) -> String,
        other_elements_printer: impl Fn(&str, &str) -> String,
        ignore_predicate: Option<impl Fn(Option<&T>) -> bool>,
        include_missing_sources: bool,
        no_end_of_message_dot: bool,
    ) {
        self.report_mismatch(
            Some(supergraph_element),
            subgraph_elements,
            element_to_string,
            supergraph_element_printer,
            other_elements_printer,
            |myself, distribution, _: Vec<U>| {
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
            ignore_predicate,
            include_missing_sources,
        );
    }

    /// Reports a mismatch between a supergraph element and subgraph elements.
    /// Not meant to be used directly: use `report_mismatch_error` or `report_mismatch_hint` instead.
    ///
    /// TODO: The generic parameter `U` is meant to represent AST nodes (or locations) that are attached to error messages.
    /// When we decide on an implementation for those, they should be added to `ast_nodes` below.
    #[allow(clippy::too_many_arguments)]
    fn report_mismatch<T: Display, U>(
        &mut self,
        supergraph_element: Option<&T>,
        subgraph_elements: &Sources<T>,
        mismatch_accessor: impl Fn(&T, bool) -> Option<String>,
        supergraph_element_printer: impl Fn(&str, Option<String>) -> String,
        other_elements_printer: impl Fn(&str, &str) -> String,
        reporter: impl FnOnce(&mut Self, Vec<String>, Vec<U>),
        ignore_predicate: Option<impl Fn(Option<&T>) -> bool>,
        include_missing_sources: bool,
    ) {
        let mut distribution_map = HashMap::new();
        #[allow(unused_mut)] // We need this to be mutable when we decide how to handle AST nodes
        let mut ast_nodes: Vec<U> = Vec::new();
        let process_subgraph_element =
            |name: &str,
             subgraph_element: &T,
             distribution_map: &mut HashMap<String, Vec<String>>| {
                if ignore_predicate
                    .as_ref()
                    .is_some_and(|pred| pred(Some(subgraph_element)))
                {
                    return;
                }
                let element = mismatch_accessor(subgraph_element, false);
                distribution_map
                    .entry(element.unwrap_or("".to_string()))
                    .or_default()
                    .push(name.to_string());
                // TODO: Get AST node equivalent and push onto `ast_nodes`
            };
        if include_missing_sources {
            for (i, name) in self.names.iter().enumerate() {
                if let Some(Some(subgraph_element)) = subgraph_elements.get(&i) {
                    process_subgraph_element(name, subgraph_element, &mut distribution_map);
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
                    process_subgraph_element(name, subgraph_element, &mut distribution_map);
                }
            }
        }
        let supergraph_mismatch = supergraph_element
            .and_then(|se| mismatch_accessor(se, true))
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
            if v == &supergraph_mismatch {
                continue; // Skip the supergraph element as it's already added
            }
            let names_str = human_readable_subgraph_names(names.iter());
            distribution.push(other_elements_printer(v, &names_str));
        }
        reporter(self, distribution, ast_nodes);
    }
}
