pub(super) mod field_definition;
pub(super) mod object;

use std::str::FromStr;

use apollo_compiler::{Name, Schema, name, parser::SourceSpan};
use thiserror::Error;

use crate::{
    cache_tag::FEDERATION_CACHE_TAG_DIRECTIVE_NAME,
    connectors::{self, StringTemplate},
    error::FederationError,
    schema::FederationSchema,
    supergraph::JOIN_DIRECTIVE,
};

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
    #[error("join__directive for cacheTag contains invalid arguments")]
    InvalidArgs,
    #[error("cacheTag format argument not found")]
    FormatNotFound,
    #[error("cacheTag format is invalid: {0}")]
    InvalidFormat(#[from] connectors::StringTemplateError),
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
    let cache_tag_directives =
        get_all_federation_cache_tag_directives(&federation_schema).map_err(|err| vec![err])?;
    field_definition::validate_fields(&federation_schema, &cache_tag_directives)?;

    Ok(())
}

struct FederationCacheTagDirective {
    format: StringTemplate,
    subgraphs: Vec<Name>,
}

enum FederationCacheTagDirectiveLocation {
    Field {
        directive: FederationCacheTagDirective,
        type_name: Name,
        field_name: Name,
        location: Option<SourceSpan>,
    },
    Object {
        directive: FederationCacheTagDirective,
        type_name: Name,
        location: Option<SourceSpan>,
    },
}

fn get_all_federation_cache_tag_directives(
    federation_schema: &FederationSchema,
) -> Result<Vec<FederationCacheTagDirectiveLocation>, ValidationError> {
    let directive_referencers = federation_schema
        .referencers()
        .get_directive(JOIN_DIRECTIVE)
        .map_err(|_| ValidationError::DirectiveNotFound)?;

    let field_directives: Vec<FederationCacheTagDirectiveLocation> = directive_referencers
        .object_fields
        .iter()
        .filter_map(|object_field| {
            let type_name = object_field.type_name.clone();
            let field_name = object_field.field_name.clone();
            let join_directives = federation_schema
                .schema()
                .get_object(&object_field.type_name)?
                .fields
                .get(&object_field.field_name)?
                .directives
                .get_all(JOIN_DIRECTIVE)
                .filter(|d| {
                    d.argument_by_name("name", federation_schema.schema())
                        .ok()
                        .and_then(|v| v.as_str())
                        == Some(FEDERATION_CACHE_TAG_DIRECTIVE_NAME)
                })
                .map(|d| {
                    let args = d
                        .argument_by_name("args", federation_schema.schema())
                        .ok()
                        .and_then(|args| args.as_object())
                        .ok_or_else(|| ValidationError::InvalidArgs)?;
                    let format = args
                        .iter()
                        .find(|(arg_name, _)| arg_name == &name!("format"));
                    let format = match format {
                        Some(format) => format
                            .1
                            .as_str()
                            .ok_or_else(|| ValidationError::FormatString)?,
                        None => {
                            return Err(ValidationError::FormatNotFound);
                        }
                    };
                    let graphs = d
                        .argument_by_name("graphs", federation_schema.schema())
                        .ok()
                        .and_then(|graphs| {
                            graphs.as_list().map(|graphs| {
                                graphs
                                    .iter()
                                    .map(|g| {
                                        g.as_enum()
                                            .cloned()
                                            .ok_or_else(|| ValidationError::InvalidArgs)
                                    })
                                    .collect::<Result<Vec<Name>, ValidationError>>()
                            })
                        })
                        .ok_or_else(|| ValidationError::InvalidArgs)??;

                    let federation_cache_tag_dir = FederationCacheTagDirectiveLocation::Field {
                        directive: FederationCacheTagDirective {
                            format: StringTemplate::from_str(format)?,
                            subgraphs: graphs,
                        },
                        type_name: type_name.clone(),
                        field_name: field_name.clone(),
                        location: d.location(),
                    };

                    Ok(federation_cache_tag_dir)
                })
                .collect::<Result<Vec<FederationCacheTagDirectiveLocation>, ValidationError>>();

            Some(join_directives)
        })
        .collect::<Result<Vec<Vec<FederationCacheTagDirectiveLocation>>, ValidationError>>()?
        .into_iter()
        .flatten()
        .collect();

    Ok(field_directives)
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
