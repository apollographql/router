use std::ops::Range;

use apollo_compiler::parser::LineColumn;
use indexmap::IndexMap;

use crate::error::CompositionError;
use crate::error::HasLocations;
use crate::error::Locations;
use crate::error::SingleFederationError;
use crate::error::SubgraphLocation;
use crate::merger::hints::HintCode;
use crate::merger::merge::Sources;
use crate::subgraph::typestate::HasMetadata;
use crate::subgraph::typestate::Subgraph;
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

    pub(crate) fn report_mismatch_error<D, S: HasLocations, T: HasMetadata>(
        &mut self,
        error: CompositionError,
        mismatched_element: &D,
        subgraph_elements: &Sources<S>,
        subgraphs: &[Subgraph<T>],
        supergraph_mismatch_accessor: impl Fn(&D) -> Option<String>,
        subgraph_mismatch_accessor: impl Fn(&S, usize) -> Option<String>,
    ) {
        self.report_mismatch(
            Some(mismatched_element),
            subgraph_elements,
            subgraphs,
            supergraph_mismatch_accessor,
            subgraph_mismatch_accessor,
            |elt, names| format!("{} in {}", elt, names.unwrap_or("undefined".to_string())),
            |elt, names| format!("{elt} in {names}"),
            |myself, distribution, locations| {
                let distribution_str = join_strings(
                    distribution.iter(),
                    JoinStringsOptions {
                        first_separator: Some(" but "),
                        separator: " and ",
                        last_separator: Some(" and "),
                        output_length_limit: None,
                    },
                );
                myself.add_error(
                    error
                        .append_message(distribution_str)
                        .append_locations(locations),
                );
            },
            false,
        );
    }

    pub(crate) fn report_mismatch_error_without_supergraph<T: HasLocations, S: HasMetadata>(
        &mut self,
        error: CompositionError,
        subgraph_elements: &Sources<T>,
        subgraphs: &[Subgraph<S>],
        mismatch_accessor: impl Fn(&T, usize) -> Option<String>,
    ) {
        self.report_mismatch::<T, T, _>(
            None,
            subgraph_elements,
            subgraphs,
            |_| None,
            mismatch_accessor,
            |_, _| String::new(),
            |elt, names| format!("{elt} in {names}"),
            |myself, distribution, locations| {
                let distribution_str = join_strings(
                    distribution.iter(),
                    JoinStringsOptions {
                        first_separator: Some(" but "),
                        separator: " and ",
                        last_separator: Some(" and "),
                        output_length_limit: None,
                    },
                );
                myself.add_error(
                    error
                        .append_message(distribution_str)
                        .append_locations(locations),
                );
            },
            false,
        );
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn report_mismatch_error_with_specifics<D, S: HasLocations, T: HasMetadata>(
        &mut self,
        error: CompositionError,
        mismatched_element: &D,
        subgraph_elements: &Sources<S>,
        subgraphs: &[Subgraph<T>],
        supergraph_mismatch_accessor: impl Fn(&D) -> Option<String>,
        subgraph_mismatch_accessor: impl Fn(&S, usize) -> Option<String>,
        supergraph_element_printer: impl Fn(&str, Option<String>) -> String,
        other_elements_printer: impl Fn(&str, &str) -> String,
        include_missing_sources: bool,
    ) {
        self.report_mismatch(
            Some(mismatched_element),
            subgraph_elements,
            subgraphs,
            supergraph_mismatch_accessor,
            subgraph_mismatch_accessor,
            supergraph_element_printer,
            other_elements_printer,
            |myself, mut distribution, locations| {
                let mut distribution_str = distribution.remove(0);
                distribution_str.push_str(&join_strings(
                    distribution.iter(),
                    JoinStringsOptions {
                        first_separator: None,
                        separator: " and ",
                        last_separator: None,
                        output_length_limit: None,
                    },
                ));
                myself.add_error(
                    error
                        .append_message(distribution_str)
                        .append_locations(locations),
                );
            },
            include_missing_sources,
        );
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn report_mismatch_hint<D, S: HasLocations, T: HasMetadata>(
        &mut self,
        code: HintCode,
        message: String,
        supergraph_element: &D,
        subgraph_elements: &Sources<S>,
        subgraphs: &[Subgraph<T>],
        supergraph_element_to_string: impl Fn(&D) -> Option<String>,
        subgraph_element_to_string: impl Fn(&S, usize) -> Option<String>,
        supergraph_element_printer: impl Fn(&str, Option<String>) -> String,
        other_elements_printer: impl Fn(&str, &str) -> String,
        include_missing_sources: bool,
        no_end_of_message_dot: bool,
    ) {
        self.report_mismatch(
            Some(supergraph_element),
            subgraph_elements,
            subgraphs,
            supergraph_element_to_string,
            subgraph_element_to_string,
            supergraph_element_printer,
            other_elements_printer,
            |myself, mut distribution, locations| {
                let mut distribution_str = distribution.remove(0);
                distribution_str.push_str(&join_strings(
                    distribution.iter(),
                    JoinStringsOptions {
                        first_separator: None,
                        separator: " and ",
                        last_separator: None,
                        output_length_limit: None,
                    },
                ));
                let suffix = if no_end_of_message_dot { "" } else { "." };
                myself.add_hint(CompositionHint {
                    code: code.code().to_string(),
                    message: format!("{message}{distribution_str}{suffix}"),
                    locations,
                });
            },
            include_missing_sources,
        );
    }

    /// Reports a mismatch between a supergraph element and subgraph elements.
    /// Not meant to be used directly: use `report_mismatch_error` or `report_mismatch_hint` instead.
    #[allow(clippy::too_many_arguments)]
    fn report_mismatch<D, S: HasLocations, T: HasMetadata>(
        &mut self,
        supergraph_element: Option<&D>,
        subgraph_elements: &Sources<S>,
        subgraphs: &[Subgraph<T>],
        // Note that these two parameters used to be `mismatchAccessor`, which took a boolean
        // indicating whether it was a supergraph element or a subgraph element. Now, we have two
        // separate functions, which allows us to use different types for the destination and
        // source data.
        supergraph_mismatch_accessor: impl Fn(&D) -> Option<String>,
        subgraph_mismatch_accessor: impl Fn(&S, usize) -> Option<String>,
        supergraph_element_printer: impl Fn(&str, Option<String>) -> String,
        other_elements_printer: impl Fn(&str, &str) -> String,
        reporter: impl FnOnce(&mut Self, Vec<String>, Locations),
        include_missing_sources: bool,
    ) {
        let mut distribution_map = Default::default();
        let mut locations = Locations::new();
        let mut process_subgraph_element =
            |name: &str,
             idx: usize,
             subgraph_element: &S,
             distribution_map: &mut IndexMap<String, Vec<String>>| {
                if let Some(element) = subgraph_mismatch_accessor(subgraph_element, idx) {
                    locations.extend(
                        subgraphs
                            .get(idx)
                            .map(|sg| subgraph_element.locations(sg))
                            .unwrap_or_default(),
                    );
                    distribution_map
                        .entry(element)
                        .or_default()
                        .push(name.to_string());
                }
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
        if distribution_map.len() <= 1 {
            tracing::warn!(
                "report_mismatch called but no mismatch found for element {supergraph_mismatch}",
            );
        }
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
