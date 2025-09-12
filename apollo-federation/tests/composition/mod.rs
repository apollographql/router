mod compose_directive;
mod demand_control;
// TODO: remove #[ignore] from tests once all fns called by Merger::merge() are implemented
mod external;
mod override_directive;
mod subscription;
mod validation_errors;

pub(crate) mod test_helpers {
    use apollo_federation::composition::compose;
    use apollo_federation::error::CompositionError;
    use apollo_federation::subgraph::typestate::Subgraph;
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
            match subgraph.into_fed2_test_subgraph(true) {
                Ok(subgraph) => fed2_subgraphs.push(subgraph),
                Err(err) => errors.extend(err.to_composition_errors()),
            }
        }
        if !errors.is_empty() {
            return Err(errors);
        }

        compose(fed2_subgraphs)
    }
}

pub(crate) use test_helpers::ServiceDefinition;
pub(crate) use test_helpers::compose_as_fed2_subgraphs;
