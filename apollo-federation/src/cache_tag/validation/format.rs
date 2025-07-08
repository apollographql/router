use std::ops::Deref;

use apollo_compiler::{Name, ast::Type, collections::IndexMap};

use crate::{
    cache_tag::validation::{FederationCacheTagDirectiveLocation, ValidationError},
    connectors::{JSONSelection, SelectionTrie},
    schema::FederationSchema,
};

pub(super) fn validate_format(
    federation_schema: &FederationSchema,
    federation_cache_tag_dirs: &[FederationCacheTagDirectiveLocation],
) -> Result<(), Vec<ValidationError>> {
    let errors: Vec<ValidationError> = federation_cache_tag_dirs
        .iter()
        .filter_map(|directive_location| {
            validate_format_string(federation_schema, directive_location)
                .err()
                .or_else(|| {
                    // Chain all validations
                    None
                })
        })
        .flatten()
        .collect();

    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}

pub(super) fn validate_format_string(
    federation_schema: &FederationSchema,
    federation_cache_tag_dir: &FederationCacheTagDirectiveLocation,
) -> Result<(), Vec<ValidationError>> {
    match federation_cache_tag_dir {
        FederationCacheTagDirectiveLocation::Object {
            type_name,
            directive,
            ..
        } => {
            let object_type = federation_schema
                .schema()
                .get_object(type_name.as_str())
                .ok_or_else(|| {
                    vec![ValidationError::Custom(format!(
                        "cannot find object {type_name:?}"
                    ))]
                })?;
            let res: Vec<Result<(), ValidationError>> = directive
                .format
                .expressions()
                .filter_map(|expr| match &expr.expression {
                    JSONSelection::Named(_) => Some(Err(ValidationError::FormatEntityKey {
                        type_name: type_name.clone(),
                        format: directive.format.to_string(),
                    })),
                    JSONSelection::Path(path_selection) => {
                        match path_selection.variable_reference::<String>() {
                            Some(var_ref) => {
                                let fields = object_type
                                    .fields
                                    .iter()
                                    .map(|(name, f)| (name, &f.ty))
                                    .collect::<IndexMap<&Name, &Type>>();
                                match is_valid_string(
                                    federation_schema,
                                    &fields,
                                    &var_ref.selection,
                                ) {
                                    Ok(_) => Some(Ok(())),
                                    Err(_err) => Some(Err(ValidationError::FormatString {
                                        type_name: type_name.clone(),
                                        format: directive.format.to_string(),
                                    })),
                                }
                            }
                            None => None,
                        }
                    }
                })
                .collect();
            let errors: Vec<ValidationError> = res.into_iter().filter_map(|r| r.err()).collect();
            if !errors.is_empty() {
                Err(errors)
            } else {
                Ok(())
            }
        }
        FederationCacheTagDirectiveLocation::Field {
            type_name,
            field_name,
            directive,
            ..
        } => {
            let object_type = federation_schema
                .schema()
                .get_object(type_name.as_str())
                .ok_or_else(|| {
                    vec![ValidationError::Custom(format!(
                        "cannot find object {type_name:?}"
                    ))]
                })?;
            let field_def = object_type.fields.get(field_name).ok_or_else(|| {
                vec![ValidationError::Custom(format!(
                    "cannot find field name {field_name:?} for type {type_name:?}"
                ))]
            })?;

            let res: Vec<Result<(), ValidationError>> = directive
                .format
                .expressions()
                .filter_map(|expr| match &expr.expression {
                    JSONSelection::Named(_) => Some(Err(ValidationError::FormatEntityKey {
                        type_name: type_name.clone(),
                        format: directive.format.to_string(),
                    })),
                    JSONSelection::Path(path_selection) => {
                        match path_selection.variable_reference::<String>() {
                            Some(var_ref) => {
                                let fields = field_def
                                    .arguments
                                    .iter()
                                    .map(|arg| (&arg.name, arg.ty.deref()))
                                    .collect::<IndexMap<&Name, &Type>>();
                                match is_valid_string(
                                    federation_schema,
                                    &fields,
                                    &var_ref.selection,
                                ) {
                                    Ok(_) => Some(Ok(())),
                                    Err(_err) => Some(Err(ValidationError::FormatString {
                                        type_name: type_name.clone(),
                                        format: directive.format.to_string(),
                                    })),
                                }
                            }
                            None => None,
                        }
                    }
                })
                .collect();
            let errors: Vec<ValidationError> = res.into_iter().filter_map(|r| r.err()).collect();
            if !errors.is_empty() {
                Err(errors)
            } else {
                Ok(())
            }
        }
    }
}

fn is_valid_string(
    federation_schema: &FederationSchema,
    fields: &IndexMap<&Name, &Type>,
    selection: &SelectionTrie,
) -> Result<(), ValidationError> {
    for (key, sel) in selection.iter() {
        let name = Name::new(key)
            .map_err(|_| ValidationError::Custom(format!("cannot create name {key:?}")))?;
        let field = fields
            .get(&name)
            .ok_or_else(|| ValidationError::Custom(format!("cannot get field {name:?}")))?;
        let type_name = match &field {
            Type::Named(name) | Type::NonNullNamed(name) => name,
            _ => return Err(ValidationError::Custom("error on schema".to_string())),
        };
        if !sel.is_leaf() {
            // Check in referencers if it's an object or interface ?
            // THen pass it directly to the recursive call
            let fields = if federation_schema
                .referencers()
                .object_types
                .contains_key(type_name)
            {
                let object = federation_schema
                    .schema()
                    .get_object(type_name.as_str())
                    .ok_or_else(|| ValidationError::Custom("error on schema".to_string()))?;
                object
                    .fields
                    .iter()
                    .map(|(name, f)| (name, &f.ty))
                    .collect::<IndexMap<&Name, &Type>>()
            } else if federation_schema
                .referencers()
                .interface_types
                .contains_key(type_name)
            {
                let interface = federation_schema
                    .schema()
                    .get_interface(type_name.as_str())
                    .ok_or_else(|| ValidationError::Custom("error on schema".to_string()))?;
                interface
                    .fields
                    .iter()
                    .map(|(name, f)| (name, &f.ty))
                    .collect::<IndexMap<&Name, &Type>>()
            } else {
                // Probably not a valid string
                return Err(ValidationError::Custom("invalid string".to_string()));
            };

            is_valid_string(federation_schema, &fields, sel)?;
        } else {
            // Check if it's not an object or interface
            if federation_schema
                .referencers()
                .object_types
                .contains_key(type_name)
                || federation_schema
                    .referencers()
                    .interface_types
                    .contains_key(type_name)
            {
                return Err(ValidationError::Custom("invalid string".to_string()));
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use apollo_compiler::Schema;
    use apollo_compiler::name;

    use crate::cache_tag::validation::get_all_federation_cache_tag_directives;

    use super::*;

    #[test]
    fn test_valid_format_string() {
        const SCHEMA: &str =
            include_str!("../test_data/valid_supergraph_format_entity_key.graphql");
        let supergraph_schema =
            Schema::parse(SCHEMA, "valid_supergraph_format_entity_key.graphql").unwrap();
        let federation_schema = FederationSchema::new(supergraph_schema).unwrap();
        let cache_tag_directives =
            get_all_federation_cache_tag_directives(&federation_schema).unwrap();
        validate_format(&federation_schema, &cache_tag_directives).unwrap();
    }

    #[test]
    fn test_invvalid_format_string() {
        const SCHEMA: &str = include_str!("../test_data/invalid_supergraph_format_string.graphql");
        let supergraph_schema =
            Schema::parse(SCHEMA, "invalid_supergraph_format_string.graphql").unwrap();
        let federation_schema = FederationSchema::new(supergraph_schema).unwrap();
        let cache_tag_directives =
            get_all_federation_cache_tag_directives(&federation_schema).unwrap();
        assert_eq!(
            validate_format(&federation_schema, &cache_tag_directives)
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
    }
}
