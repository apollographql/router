use apollo_compiler::schema::ComponentName;

use crate::{
    cache_tag::validation::{FederationCacheTagDirectiveLocation, ValidationError},
    schema::FederationSchema,
};

pub(super) fn validate_root_field(
    query_type_name: &ComponentName,
    federation_cache_tag_dir: &FederationCacheTagDirectiveLocation,
) -> Option<ValidationError> {
    match federation_cache_tag_dir {
        FederationCacheTagDirectiveLocation::Field {
            type_name,
            field_name,
            ..
        } if type_name.as_str() != query_type_name.name.as_str() => {
            Some(ValidationError::RootField {
                type_name: type_name.clone(),
                field_name: field_name.clone(),
            })
        }
        _ => None,
    }
}

pub(super) fn validate_fields(
    federation_schema: &FederationSchema,
    federation_cache_tag_dirs: &[FederationCacheTagDirectiveLocation],
) -> Result<(), Vec<ValidationError>> {
    let query_type_name = federation_schema
        .schema()
        .schema_definition
        .query
        .as_ref()
        .ok_or_else(|| vec![ValidationError::QueryNotFound])?;

    let non_root_object_fields_errors: Vec<ValidationError> = federation_cache_tag_dirs
        .iter()
        .filter_map(|directive_location| {
            validate_root_field(query_type_name, directive_location).or_else(|| {
                // Chain all validations
                None
            })
        })
        .collect();

    if non_root_object_fields_errors.is_empty() {
        Ok(())
    } else {
        Err(non_root_object_fields_errors)
    }
}

#[cfg(test)]
mod tests {
    use apollo_compiler::Schema;
    use apollo_compiler::name;

    use crate::cache_tag::validation::get_all_federation_cache_tag_directives;

    use super::*;

    #[test]
    fn test_root_fields() {
        const SCHEMA: &str = include_str!("../test_data/invalid_supergraph_root_fields.graphql");
        let supergraph_schema =
            Schema::parse(SCHEMA, "invalid_supergraph_root_fields.graphql").unwrap();
        let federation_schema = FederationSchema::new(supergraph_schema).unwrap();
        let cache_tag_directives =
            get_all_federation_cache_tag_directives(&federation_schema).unwrap();
        assert_eq!(
            validate_fields(&federation_schema, &cache_tag_directives)
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
    }
}
