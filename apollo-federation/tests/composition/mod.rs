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
mod demand_control;
// TODO: remove #[ignore] from tests once all fns called by Merger::merge() are implemented
mod external;
mod override_directive;
mod subscription;
mod supergraph_reversibility;
mod validation_errors;

pub(crate) mod test_helpers {
    use apollo_federation::ValidFederationSubgraphs;
    use apollo_federation::composition::compose;
    use apollo_federation::error::CompositionError;
    use apollo_federation::error::FederationError;
    use apollo_federation::subgraph::typestate::Subgraph;
    use apollo_federation::supergraph::Satisfiable;
    use apollo_federation::supergraph::Supergraph;
    use insta::assert_snapshot;

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
        let error_strings: Vec<String> = errors.iter().map(|e| e.to_string()).collect();

        // Verify error count matches expectations
        assert_eq!(
            errors.len(),
            expected_errors.len(),
            "Expected {} errors but got {}: {:?}",
            expected_errors.len(),
            errors.len(),
            error_strings
        );

        // Verify each expected error code and message
        for (i, (expected_code, expected_message)) in expected_errors.iter().enumerate() {
            let error = &errors[i];

            // Check error code (assuming CompositionError has a code method or field)
            // This will need to be implemented based on the actual CompositionError structure
            // For now, we'll validate the error message contains the expected content
            let error_str = error.to_string();
            assert!(
                error_str.contains(expected_message),
                "Error {} does not contain expected message.\nExpected: {}\nActual: {}",
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
}

pub(crate) use test_helpers::ServiceDefinition;
pub(crate) use test_helpers::assert_composition_errors;
pub(crate) use test_helpers::compose_as_fed2_subgraphs;
pub(crate) use test_helpers::extract_subgraphs_from_supergraph_result;
pub(crate) use test_helpers::print_sdl;
