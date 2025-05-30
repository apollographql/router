mod satisfiability_error;

use crate::error::CompositionError;
use crate::supergraph::Merged;
use crate::supergraph::Satisfiable;
use crate::supergraph::Supergraph;

pub fn validate_satisfiability(
    _supergraph: Supergraph<Merged>,
) -> Result<Supergraph<Satisfiable>, Vec<CompositionError>> {
    panic!("validate_satisfiability is not implemented yet")
}
