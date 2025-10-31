mod compose_auth;
mod compose_basic;
mod compose_directive;
mod compose_directive_sharing;
mod compose_inaccessible;
mod compose_interface_object;
mod compose_misc;
mod compose_tag;
mod compose_type_merging;
mod compose_types;
mod compose_validation;
mod connectors;
mod demand_control;
mod directive_argument_merge_strategies;
// TODO: remove #[ignore] from tests once all fns called by Merger::merge() are implemented
mod external;
mod override_directive;
mod subscription;
mod supergraph_reversibility;
mod validation_errors;

pub(crate) mod test_helpers {
    use std::iter::zip;

    use apollo_federation::ValidFederationSubgraphs;
    use apollo_federation::composition::compose;
    use apollo_federation::error::CompositionError;
    use apollo_federation::error::FederationError;
    use apollo_federation::subgraph::typestate::Subgraph;
    use apollo_federation::supergraph::CompositionHint;
    use apollo_federation::supergraph::Satisfiable;
    use apollo_federation::supergraph::Supergraph;

    pub(crate) struct ServiceDefinition<'a> {
        pub(crate) name: &'a str,
        pub(crate) type_defs: &'a str,
    }

    /// Composes a set of subgraphs as if they had the latest federation 2 spec link in them.
    /// Also, all federation directives are automatically imported.
    // PORT_NOTE: This function corresponds to `composeAsFed2Subgraphs` in JS implementation.
    pub(crate) fn compose_as_fed2_subgraphs(
        service_list: &[ServiceDefinition<'_>],
    ) -> Result<Supergraph<Satisfiable>, Vec<CompositionError>> {
        let mut subgraphs = Vec::new();
        let mut errors = Vec::new();
        for service in service_list {
            let result = Subgraph::parse(
                service.name,
                &format!("http://{}", service.name),
                service.type_defs,
            );
            match result {
                Ok(subgraph) => {
                    subgraphs.push(subgraph);
                }
                Err(err) => {
                    errors.extend(err.to_composition_errors());
                }
            }
        }
        if !errors.is_empty() {
            return Err(errors);
        }

        // PORT_NOTE: This statement corresponds to `asFed2Service` function in JS.
        let mut fed2_subgraphs = Vec::new();
        for subgraph in subgraphs {
            match subgraph.into_fed2_test_subgraph(true, false) {
                Ok(subgraph) => fed2_subgraphs.push(subgraph),
                Err(err) => errors.extend(err.to_composition_errors()),
            }
        }
        if !errors.is_empty() {
            return Err(errors);
        }

        compose(fed2_subgraphs)
    }

    /// Helper function to print schema SDL with consistent formatting for snapshots
    pub(crate) fn print_sdl(schema: &apollo_compiler::Schema) -> String {
        let mut schema = schema.clone();
        schema.types.sort_keys();
        schema.directive_definitions.sort_keys();
        schema.to_string()
    }

    /// Helper function to assert composition errors
    pub(crate) fn assert_composition_errors(
        result: &Result<Supergraph<Satisfiable>, Vec<CompositionError>>,
        expected_errors: &[(&str, &str)],
    ) {
        let errors = result.as_ref().expect_err("Expected composition to fail");
        let error_strings: Vec<(String, String)> = errors
            .iter()
            .map(|e| (e.code().definition().code().to_string(), e.to_string()))
            .collect();

        // Verify error count matches expectations
        assert_eq!(
            expected_errors.len(),
            errors.len(),
            "Expected {} errors but got {}:\nEXPECTED:\n{:?}\nACTUAL:\n{:?}",
            expected_errors.len(),
            errors.len(),
            expected_errors,
            error_strings
        );

        // Verify each expected error code and message
        for (i, (expected_code, expected_message)) in expected_errors.iter().enumerate() {
            let (error_code, error_str) = &error_strings[i];

            // Check error code
            assert!(
                error_code.contains(expected_code),
                "Error at index {} does not contain expected code.\nEXPECTED:\n{}\nACTUAL:\n{}",
                i,
                expected_code,
                error_code
            );
            // Check error message
            assert!(
                error_str.contains(expected_message),
                "Error at index {} does not contain expected message.\nEXPECTED:\n{}\nACTUAL:\n{}",
                i,
                expected_message,
                error_str
            );
        }
    }

    /// Helper function to extract subgraphs from supergraph for testing
    /// Equivalent to extractSubgraphFromSupergraph from the JS tests
    pub(crate) fn extract_subgraphs_from_supergraph_result(
        supergraph: &Supergraph<Satisfiable>,
    ) -> Result<ValidFederationSubgraphs, FederationError> {
        // Use the public API on Supergraph to extract subgraphs
        let schema_sdl = supergraph.schema().schema().to_string();
        let api_supergraph = apollo_federation::Supergraph::new(&schema_sdl)?;
        api_supergraph.extract_subgraphs()
    }

    pub(crate) fn assert_hints_equal(
        actual_hints: &Vec<CompositionHint>,
        expected_hints: &Vec<CompositionHint>,
    ) {
        if actual_hints.len() != expected_hints.len() {
            panic!(
                "Mismatched number of hints: expected {} hint(s) but got {} hint(s)\nEXPECTED:\n{expected_hints:#?}\nACTUAL:\n{actual_hints:#?}",
                expected_hints.len(),
                actual_hints.len()
            )
        }
        let zipped = zip(actual_hints, expected_hints);
        zipped.for_each(|(ch1, ch2)| {
            assert!(
                ch1.code() == ch2.code() && ch1.message() == ch2.message(),
                "EXPECTED:\n{:#?}\nACTUAL:\n{:#?}",
                expected_hints,
                actual_hints
            )
        });
    }
}

pub(crate) use test_helpers::ServiceDefinition;
pub(crate) use test_helpers::assert_composition_errors;
pub(crate) use test_helpers::assert_hints_equal;
pub(crate) use test_helpers::compose_as_fed2_subgraphs;
pub(crate) use test_helpers::extract_subgraphs_from_supergraph_result;
pub(crate) use test_helpers::print_sdl;
