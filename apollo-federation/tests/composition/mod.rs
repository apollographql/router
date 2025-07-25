mod demand_control;
mod validation_errors;

pub(crate) mod test_helpers {
    use apollo_federation::composition::compose;
    use apollo_federation::error::CompositionError;
    use apollo_federation::subgraph::typestate::Initial;
    use apollo_federation::subgraph::typestate::Subgraph;
    use apollo_federation::supergraph::Satisfiable;
    use apollo_federation::supergraph::Supergraph;

    pub(crate) struct ServiceDefinition<'a> {
        pub(crate) name: &'a str,
        pub(crate) type_defs: &'a str,
    }

    // PORT_NOTE: This function corresponds to `composeAsFed2Subgraphs` in JS implementation.
    pub(crate) fn compose_as_fed2_subgraphs<'a>(
        service_list: &[ServiceDefinition<'a>],
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
                    errors.push(err.into());
                }
            }
        }
        if !errors.is_empty() {
            return Err(errors);
        }
        compose_as_fed2_subgraphs_inner(subgraphs)
    }

    fn compose_as_fed2_subgraphs_inner(
        subgraphs: Vec<Subgraph<Initial>>,
        // composition_options: CompositionOptions, // TODO
    ) -> Result<Supergraph<Satisfiable>, Vec<CompositionError>> {
        // PORT_NOTE: This statement corresponds to `asFed2Service` function in JS.
        let mut fed2_subgraphs = Vec::new();
        let mut errors = Vec::new();
        for subgraph in subgraphs {
            match subgraph.into_fed2_test_subgraph(true) {
                Ok(subgraph) => fed2_subgraphs.push(subgraph),
                Err(err) => errors.push(CompositionError::from(err)),
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
