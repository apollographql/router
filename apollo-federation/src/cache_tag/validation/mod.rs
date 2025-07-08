pub(super) mod field_definition;
pub(super) mod format;
pub(super) mod object;

use std::{collections::HashSet, str::FromStr};

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
        "object {0:?} is not an entity. cacheTag can only apply on resolvable entities, object containing at least 1 @key directive"
    )]
    ResolvableEntity(Name),
    #[error(
        "Each entity field referenced in a @cacheTag format (applied on entity type) must be a member of every @key field set. In other words, when there are multiple @key fields on the type, the referenced field(s) must be limited to their intersection. Bad cacheTag format {format:?} on type {type_name:?}"
    )]
    FormatEntityKey { type_name: Name, format: String },
    #[error(
        "When there are multiple @key fields on a type, the referenced field(s) in @cacheTag format must be limited to their intersection.  Bad cacheTag format {format:?} on type {type_name:?}"
    )]
    FormatString { type_name: Name, format: String },
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
    #[error("cacheTag validation error: {0}")]
    Custom(String),
}

pub fn validate_supergraph(supergraph_schema: &Schema) -> Result<(), Vec<ValidationError>> {
    let federation_schema = FederationSchema::new(supergraph_schema.clone())
        .map_err(|err| vec![ValidationError::FederationError(err)])?;
    let cache_tag_directives =
        get_all_federation_cache_tag_directives(&federation_schema).map_err(|err| vec![err])?;
    object::validate_objects(&federation_schema, &cache_tag_directives)?;
    field_definition::validate_fields(&federation_schema, &cache_tag_directives)?;
    format::validate_format(&federation_schema, &cache_tag_directives)?;

    Ok(())
}

#[allow(dead_code)]
#[derive(Debug)]
struct FederationCacheTagDirective {
    format: StringTemplate,
    subgraphs: HashSet<Name>,
}

#[allow(dead_code)]
#[derive(Debug)]
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

    let mut directives: Vec<FederationCacheTagDirectiveLocation> = directive_referencers
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
                            .ok_or_else(|| ValidationError::InvalidArgs)?,
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
                                    .collect::<Result<HashSet<Name>, ValidationError>>()
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

    let object_directives = directive_referencers
        .object_types
        .iter()
        .filter_map(|object_type| {
            let type_name = object_type.type_name.clone();
            let join_directives = federation_schema
                .schema()
                .get_object(&object_type.type_name)?
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
                            .ok_or_else(|| ValidationError::InvalidArgs)?,
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
                                    .collect::<Result<HashSet<Name>, ValidationError>>()
                            })
                        })
                        .ok_or_else(|| ValidationError::InvalidArgs)??;

                    let federation_cache_tag_dir = FederationCacheTagDirectiveLocation::Object {
                        directive: FederationCacheTagDirective {
                            format: StringTemplate::from_str(format)?,
                            subgraphs: graphs,
                        },
                        type_name: type_name.clone(),
                        location: d.location(),
                    };

                    Ok(federation_cache_tag_dir)
                })
                .collect::<Result<Vec<FederationCacheTagDirectiveLocation>, ValidationError>>();

            Some(join_directives)
        })
        .collect::<Result<Vec<Vec<FederationCacheTagDirectiveLocation>>, ValidationError>>()?
        .into_iter()
        .flatten();
    directives.extend(object_directives);

    Ok(directives)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_valid_supergraph() {
        let schema: &str = include_str!("../test_data/valid_supergraph.graphql");
        let supergraph_schema = Schema::parse(schema, "valid_supergraph.graphql").unwrap();

        assert!(validate_supergraph(&supergraph_schema).is_ok());

        let schema: &str = include_str!("../test_data/valid_supergraph_format_entity_key.graphql");
        let supergraph_schema =
            Schema::parse(schema, "valid_supergraph_format_entity_key.graphql").unwrap();

        assert!(validate_supergraph(&supergraph_schema).is_ok());
    }

    #[test]
    fn test_invalid_supergraph() {
        let schema: &str = include_str!("../test_data/invalid_supergraph_root_fields.graphql");
        let supergraph_schema =
            Schema::parse(schema, "invalid_supergraph_root_fields.graphql").unwrap();

        assert_eq!(
            validate_supergraph(&supergraph_schema)
                .unwrap_err()
                .into_iter()
                .map(|err| err.to_string())
                .collect::<Vec<String>>(),
            vec![
                ValidationError::RootField {
                    type_name: name!("Product"),
                    field_name: name!("upc")
                }
                .to_string(),
                ValidationError::RootField {
                    type_name: name!("User"),
                    field_name: name!("name")
                }
                .to_string()
            ]
        );
        let schema: &str =
            include_str!("../test_data/invalid_supergraph_resolvable_entity.graphql");
        let supergraph_schema =
            Schema::parse(schema, "invalid_supergraph_resolvable_entity.graphql").unwrap();

        assert_eq!(
            validate_supergraph(&supergraph_schema)
                .unwrap_err()
                .into_iter()
                .map(|err| err.to_string())
                .collect::<Vec<String>>(),
            vec![ValidationError::ResolvableEntity(name!("Name")).to_string(),]
        );
        let schema: &str = include_str!("../test_data/invalid_supergraph_format_string.graphql");
        let supergraph_schema =
            Schema::parse(schema, "invalid_supergraph_format_string.graphql").unwrap();

        assert_eq!(
            validate_supergraph(&supergraph_schema)
                .unwrap_err()
                .into_iter()
                .map(|err| err.to_string())
                .collect::<Vec<String>>(),
            vec![
                ValidationError::FormatString {
                    type_name: name!("Product"),
                    format: "product-{$key.test}".to_string()
                }
                .to_string(),
            ]
        );
        let schema: &str =
            include_str!("../test_data/invalid_supergraph_format_entity_key.graphql");
        let supergraph_schema =
            Schema::parse(schema, "invalid_supergraph_format_entity_key.graphql").unwrap();

        assert_eq!(
            validate_supergraph(&supergraph_schema)
                .unwrap_err()
                .into_iter()
                .map(|err| err.to_string())
                .collect::<Vec<String>>(),
            vec![
                ValidationError::FormatEntityKey {
                    type_name: name!("Product"),
                    format: "product-{upc}".to_string()
                }
                .to_string(),
                ValidationError::FormatEntityKey {
                    type_name: name!("Product"),
                    format: "product-{$key.unknown}".to_string()
                }
                .to_string(),
                ValidationError::FormatEntityKey {
                    type_name: name!("Product"),
                    format: "product-{$key.upc.unknown}".to_string()
                }
                .to_string(),
                ValidationError::FormatEntityKey {
                    type_name: name!("Product"),
                    format: "product-{$key.test.a}".to_string()
                }
                .to_string(),
            ]
        );
    }
}
