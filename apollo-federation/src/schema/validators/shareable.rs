use crate::error::FederationError;
use crate::error::MultipleFederationErrors;
use crate::schema::FederationSchema;
use crate::schema::subgraph_metadata::SubgraphMetadata;

pub(crate) fn validate_shareable_directives(
    schema: &FederationSchema,
    meta: &SubgraphMetadata,
    errors: &mut MultipleFederationErrors,
) -> Result<(), FederationError> {
    todo!()
}
