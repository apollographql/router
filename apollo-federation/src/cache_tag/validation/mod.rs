pub(super) mod field_definition;
pub(super) mod object;

use apollo_compiler::{Name, Schema};
use thiserror::Error;

use crate::{error::FederationError, schema::FederationSchema};

#[derive(Debug, Error)]
pub enum ValidationError {
    #[error(
        "error on field {field_name:?} on type {type_name:?}: cacheTag can only apply on root fields"
    )]
    RootField { type_name: Name, field_name: Name },
    #[error("cacheTag applied on root fields can only reference arguments in format using $args")]
    FormatFieldArgument,
    #[error(
        "cacheTag can only apply on resolvable entities, object containing at least 1 @key directive"
    )]
    ResolvableEntity,
    #[error(
        "Each entity field referenced in a @cacheTag format (applied on entity type) must be a member of every @key field set. In other words, when there are multiple @key fields on the type, the referenced field(s) must be limited to their intersection"
    )]
    FormatEntityKey,
    #[error(
        "When there are multiple @key fields on a type, the referenced field(s) in @cacheTag format must be limited to their intersection"
    )]
    EntityKeyTagValue,
    #[error("@cacheTag format must always generate a valid string")]
    FormatString,
    #[error("cacheTag directive not found")]
    DirectiveNotFound,
    #[error("query root type not found")]
    QueryNotFound,
    #[error("federation error: {0}")]
    FederationError(#[from] FederationError),
}

pub fn validate_supergraph(supergraph_schema: &Schema) -> Result<(), Vec<ValidationError>> {
    let federation_schema = FederationSchema::new(supergraph_schema.clone())
        .map_err(|err| vec![ValidationError::FederationError(err)])?;
    field_definition::validate_root_field(&federation_schema)?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_valid_supergraph() {
        const SCHEMA: &str = include_str!("../test_data/valid_supergraph.graphql");
        let supergraph_schema = Schema::parse(SCHEMA, "valid_supergraph.graphql").unwrap();

        assert!(validate_supergraph(&supergraph_schema).is_ok());
    }
}
