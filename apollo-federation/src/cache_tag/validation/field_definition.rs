use crate::{
    cache_tag::{FEDERATION_CACHE_TAG_DIRECTIVE_NAME, validation::ValidationError},
    schema::FederationSchema,
    supergraph::JOIN_DIRECTIVE,
};

pub(super) fn validate_root_field(
    federation_schema: &FederationSchema,
) -> Result<(), Vec<ValidationError>> {
    let query_type_name = federation_schema
        .schema()
        .schema_definition
        .query
        .as_ref()
        .ok_or_else(|| vec![ValidationError::QueryNotFound])?;
    let directive_referencers = federation_schema
        .referencers()
        .get_directive(JOIN_DIRECTIVE)
        .map_err(|_| vec![ValidationError::DirectiveNotFound])?;

    let non_root_object_fields_errors: Vec<ValidationError> = directive_referencers
        .object_fields
        .iter()
        .filter_map(|object_field| {
            let contains_cache_tag_directive = federation_schema
                .schema()
                .get_object(&object_field.type_name)?
                .fields
                .get(&object_field.field_name)?
                .directives
                .get_all(JOIN_DIRECTIVE)
                .any(|d| {
                    d.argument_by_name("name", federation_schema.schema())
                        .ok()
                        .and_then(|v| v.as_str())
                        == Some(FEDERATION_CACHE_TAG_DIRECTIVE_NAME)
                });
            if !contains_cache_tag_directive {
                return None;
            }

            if object_field.type_name.as_str() != query_type_name.name.as_str() {
                Some(ValidationError::RootField {
                    type_name: object_field.type_name.clone(),
                    field_name: object_field.field_name.clone(),
                })
            } else {
                None
            }
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

    use super::*;

    #[test]
    fn test_root_fields() {
        const SCHEMA: &str = include_str!("../test_data/invalid_supergraph_root_fields.graphql");
        let supergraph_schema =
            Schema::parse(SCHEMA, "invalid_supergraph_root_fields.graphql").unwrap();
        let federation_schema = FederationSchema::new(supergraph_schema).unwrap();

        assert_eq!(
            validate_root_field(&federation_schema)
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
