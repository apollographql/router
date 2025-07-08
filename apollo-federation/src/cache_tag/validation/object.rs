use std::collections::HashSet;

use apollo_compiler::{
    Name,
    executable::{FieldSet, SelectionSet},
    schema::ObjectType,
    validation::Valid,
};

use crate::{
    cache_tag::validation::{FederationCacheTagDirectiveLocation, ValidationError},
    connectors::{JSONSelection, SelectionTrie},
    schema::FederationSchema,
};

pub(super) fn validate_objects(
    federation_schema: &FederationSchema,
    federation_cache_tag_dirs: &[FederationCacheTagDirectiveLocation],
) -> Result<(), Vec<ValidationError>> {
    let non_root_object_fields_errors: Vec<ValidationError> = federation_cache_tag_dirs
        .iter()
        .filter_map(|directive_location| {
            validate_resolvable_entity(federation_schema, directive_location)
                .err()
                .or_else(|| validate_format_entity_key(federation_schema, directive_location).err())
                .or_else(|| {
                    // Chain all validations
                    None
                })
        })
        .flatten()
        .collect();

    if non_root_object_fields_errors.is_empty() {
        Ok(())
    } else {
        Err(non_root_object_fields_errors)
    }
}

pub(super) fn validate_resolvable_entity(
    federation_schema: &FederationSchema,
    federation_cache_tag_dir: &FederationCacheTagDirectiveLocation,
) -> Result<(), Vec<ValidationError>> {
    match federation_cache_tag_dir {
        FederationCacheTagDirectiveLocation::Object {
            type_name,
            directive,
            ..
        } => {
            // Check if it's an entity for the subgraph targeted by cacheTag directive
            let is_entity = federation_schema
                .schema()
                .get_object(type_name.as_str())
                .ok_or_else(|| {
                    vec![ValidationError::Custom(format!(
                        "cannot find object {type_name:?}"
                    ))]
                })?
                .directives
                .get_all("join__type")
                .any(|d| {
                    let target_graph = d
                        .specified_argument_by_name("graph")
                        .and_then(|d| d.as_enum())
                        .map(|name| directive.subgraphs.contains(name))
                        .unwrap_or_default();

                    target_graph
                        && d.specified_argument_by_name("resolvable")
                            .and_then(|r| r.to_bool())
                            .unwrap_or(true)
                });

            if is_entity {
                Ok(())
            } else {
                Err(vec![ValidationError::ResolvableEntity(type_name.clone())])
            }
        }
        _ => Ok(()),
    }
}

pub(super) fn validate_format_entity_key(
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
            let res: Vec<Result<SelectionSet, ValidationError>> = directive
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
                                let mut selection_set = SelectionSet::new(type_name.clone());
                                // Build the selection set based on what's in the variable, so if it's $key.a.b it will generate { a { b } }
                                match build_selection_set(
                                    &mut selection_set,
                                    federation_schema,
                                    &var_ref.selection,
                                ) {
                                    Ok(_) => Some(Ok(selection_set)),
                                    Err(_err) => Some(Err(ValidationError::FormatEntityKey {
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
            let (field_sets, errors) =
                res.into_iter()
                    .fold((vec![], vec![]), |(mut field_sets, mut errors), current| {
                        match current {
                            Ok(res) => {
                                field_sets.push(FieldSet {
                                    selection_set: res,
                                    sources: Default::default(),
                                });
                            }
                            Err(errs) => {
                                errors.push(errs);
                            }
                        }

                        (field_sets, errors)
                    });
            if !errors.is_empty() {
                return Err(errors);
            }
            let entity_keys =
                get_entity_keys_field_set(federation_schema, object_type, &directive.subgraphs)
                    .map_err(|err| vec![err])?;

            // Check if all field sets coming from all collected StringTemplate ($key.a.b) from cacheTag directives are each a subset of each entity keys
            let is_correct = field_sets.into_iter().all(|sel| {
                entity_keys.iter().all(|entity_key_field_set| {
                    crate::connectors::field_set_is_subset(&sel, entity_key_field_set)
                })
            });

            if is_correct {
                Ok(())
            } else {
                Err(vec![ValidationError::FormatEntityKey {
                    type_name: type_name.clone(),
                    format: directive.format.to_string(),
                }])
            }
        }
        _ => Ok(()),
    }
}

fn get_entity_keys_field_set(
    federation_schema: &FederationSchema,
    object_type: &ObjectType,
    subgraph_names: &HashSet<Name>,
) -> Result<Vec<FieldSet>, ValidationError> {
    // Get entity keys from join__type directive for given subgraph names
    let keys = object_type
        .directives
        .get_all("join__type")
        .filter_map(|dir| {
            let graph = match dir
                .argument_by_name("graph", federation_schema.schema())
                .ok()
                .and_then(|d| d.as_enum())
                .ok_or_else(|| ValidationError::Custom("invalid join__type directive".to_string()))
            {
                Ok(graph) => graph,
                Err(err) => return Some(Err(err)),
            };
            if !subgraph_names.contains(graph) {
                return None;
            }
            Some(
                dir.argument_by_name("key", federation_schema.schema())
                    .ok()
                    .and_then(|d| d.as_str())
                    .ok_or_else(|| {
                        ValidationError::Custom("invalid join__type directive".to_string())
                    }),
            )
        })
        .collect::<Result<Vec<&str>, ValidationError>>()?;
    let field_sets = keys
        .into_iter()
        .map(|k| {
            FieldSet::parse(
                Valid::assume_valid_ref(federation_schema.schema()),
                object_type.name.clone(),
                k,
                "",
            )
            .map_err(|err| {
                ValidationError::Custom(format!("cannot parse field set for entity keys: {err}"))
            })
        })
        .collect::<Result<Vec<FieldSet>, ValidationError>>()?;

    Ok(field_sets)
}

/// Build the selection set based on what's in the variable, so if it's $key.a.b it will generate { a { b } }
fn build_selection_set(
    selection_set: &mut SelectionSet,
    federation_schema: &FederationSchema,
    selection: &SelectionTrie,
) -> Result<(), ValidationError> {
    for (key, sel) in selection.iter() {
        let name = Name::new(key)
            .map_err(|_| ValidationError::Custom(format!("cannot create name {key:?}")))?;
        let mut new_field = selection_set
            .new_field(federation_schema.schema(), name)
            .map_err(|_| {
                ValidationError::Custom(format!("cannot create selection set with {key:?}"))
            })?;

        if !sel.is_leaf() {
            build_selection_set(&mut new_field.selection_set, federation_schema, sel)?;
        }
        selection_set.push(new_field);
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
    fn test_resolvable_entity() {
        const SCHEMA: &str =
            include_str!("../test_data/invalid_supergraph_resolvable_entity.graphql");
        let supergraph_schema =
            Schema::parse(SCHEMA, "invalid_supergraph_resolvable_entity.graphql").unwrap();
        let federation_schema = FederationSchema::new(supergraph_schema).unwrap();
        let cache_tag_directives =
            get_all_federation_cache_tag_directives(&federation_schema).unwrap();
        assert_eq!(
            validate_objects(&federation_schema, &cache_tag_directives)
                .unwrap_err()
                .into_iter()
                .map(|err| err.to_string())
                .collect::<Vec<String>>(),
            vec![ValidationError::ResolvableEntity(name!("Name")).to_string(),]
        );
    }

    #[test]
    fn test_format_entity_key() {
        const SCHEMA: &str =
            include_str!("../test_data/invalid_supergraph_format_entity_key.graphql");
        let supergraph_schema =
            Schema::parse(SCHEMA, "invalid_supergraph_format_entity_key.graphql").unwrap();
        let federation_schema = FederationSchema::new(supergraph_schema).unwrap();
        let cache_tag_directives =
            get_all_federation_cache_tag_directives(&federation_schema).unwrap();
        assert_eq!(
            validate_objects(&federation_schema, &cache_tag_directives)
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

    #[test]
    fn test_valid_format_entity_key() {
        const SCHEMA: &str =
            include_str!("../test_data/valid_supergraph_format_entity_key.graphql");
        let supergraph_schema =
            Schema::parse(SCHEMA, "valid_supergraph_format_entity_key.graphql").unwrap();
        let federation_schema = FederationSchema::new(supergraph_schema).unwrap();
        let cache_tag_directives =
            get_all_federation_cache_tag_directives(&federation_schema).unwrap();
        validate_objects(&federation_schema, &cache_tag_directives).unwrap();
    }
}
