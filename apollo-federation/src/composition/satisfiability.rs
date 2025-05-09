use crate::error::FederationError;
use crate::supergraph::Merged;
use crate::supergraph::Satisfiable;
use crate::supergraph::Supergraph;

pub(crate) fn validate_satisfiability(
    _supergraph: Supergraph<Merged>,
) -> Result<Supergraph<Satisfiable>, Vec<FederationError>> {
    panic!("validate_satisfiability is not implemented yet")
}
