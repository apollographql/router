use apollo_compiler::Node;
use apollo_compiler::Schema;
use apollo_compiler::executable;
use apollo_compiler::executable::FieldSet;
use apollo_compiler::schema::ExtendedType;
use apollo_compiler::schema::NamedType;
use apollo_compiler::validation::Valid;

use crate::error::FederationError;
use crate::error::MultipleFederationErrors;
use crate::error::SingleFederationError;
use crate::operation::Selection;
use crate::operation::SelectionSet;
use crate::schema::ValidFederationSchema;
use crate::schema::position::CompositeTypeDefinitionPosition;
use crate::schema::position::FieldDefinitionPosition;
use crate::schema::position::InterfaceTypeDefinitionPosition;
use crate::schema::position::ObjectTypeDefinitionPosition;
use crate::schema::position::UnionTypeDefinitionPosition;

// Federation spec does not allow the alias syntax in field set strings.
// However, since `parse_field_set` uses the standard GraphQL parser, which allows aliases,
// we need this secondary check to ensure that aliases are not used.
fn check_absence_of_aliases(selection_set: &SelectionSet) -> Result<(), FederationError> {
    fn visit_selection_set(
        errors: &mut MultipleFederationErrors,
        selection_set: &SelectionSet,
    ) -> Result<(), FederationError> {
        for selection in selection_set.iter() {
            match selection {
                Selection::InlineFragment(frag) => check_absence_of_aliases(&frag.selection_set)?,
                Selection::Field(field) => {
                    if let Some(alias) = &field.field.alias {
                        errors.push(SingleFederationError::UnsupportedFeature {
                            // PORT_NOTE: The JS version also quotes the directive name in the error message.
                            //            For example, "aliases are not currently supported in @requires".
                            message: format!(r#"Cannot use alias "{alias}" in "{}": aliases are not currently supported in the used directive"#, field.field),
                            kind: crate::error::UnsupportedFeatureKind::Alias
                        }.into());
                    }
                    if let Some(selection_set) = &field.selection_set {
                        visit_selection_set(errors, selection_set)?;
                    }
                }
            }
        }
        Ok(())
    }

    let mut errors = MultipleFederationErrors { errors: vec![] };
    visit_selection_set(&mut errors, selection_set)?;
    errors.into_result()
}

// TODO: In the JS codebase, this has some error-rewriting to help give the user better hints around
// non-existent fields.
pub(crate) fn parse_field_set(
    schema: &ValidFederationSchema,
    parent_type_name: NamedType,
    field_set: &str,
    validate: bool,
) -> Result<SelectionSet, FederationError> {
    // Note this parsing takes care of adding curly braces ("{" and "}") if they aren't in the
    // string.
    let field_set = if validate {
        FieldSet::parse_and_validate(
            schema.schema(),
            parent_type_name,
            field_set,
            "field_set.graphql",
        )?
    } else {
        Valid::assume_valid(FieldSet::parse(
            schema.schema(),
            parent_type_name,
            field_set,
            "field_set.graphql",
        )?)
    };

    // A field set should not contain any named fragments.
    let fragments = Default::default();
    let selection_set =
        SelectionSet::from_selection_set(&field_set.selection_set, &fragments, schema, &||
            // never cancel
            Ok(()))?;

    // Validate that the field set has no aliases.
    if validate {
        check_absence_of_aliases(&selection_set)?;
    }

    Ok(selection_set)
}

/// When we first see a field set in a GraphQL document, there are some constraints which can make
/// calling `parse_field_set()` on it difficult to do.
/// 1. We may not have a `ValidFederationSchema` yet, since we may be in the process of building
///    one. This is the case in `extract_subgraphs_from_supergraph()`.
///    TODO: As noted in the callsite in `extract_subgraphs_from_supergraph()`, ideally we could
///    move the parsing to after extraction, but it takes time to determine whether that impacts
///    correctness, so we're leaving it for later.
/// 2. GraphQL documents, due to historical reasons, may have field sets containing enum values that
///    are mistakenly written as strings, but which we still want to support for backwards
///    compatibility.
/// 3. We may prefer an apollo_compiler `SelectionSet` during tests.
///
/// This function exists for these purposes, and returns an apollo_compiler `SelectionSet`,
/// optionally trying to fix the aforementioned string/enum value issues. This function also returns
/// a boolean indicating whether the field set was modified to fix such string/enum value issues.
///
/// Outside these specific purposes, you should prefer to use `parse_field_set()` instead.
pub(crate) fn parse_field_set_without_normalization(
    schema: &Valid<Schema>,
    parent_type_name: NamedType,
    field_set: &str,
    fix_string_enum_values: bool,
) -> Result<(executable::SelectionSet, bool), FederationError> {
    // Note this parsing takes care of adding curly braces ("{" and "}") if they aren't in the
    // string.
    let (field_set, is_modified) = if fix_string_enum_values {
        let mut field_set = FieldSet::parse(
            schema,
            parent_type_name.clone(),
            field_set,
            "field_set.graphql",
        )?;
        let is_modified = fix_string_enum_values_in_field_set(schema, &mut field_set);
        field_set.validate(schema)?;
        // `FieldSet::validate()` strangely doesn't return `Valid<FieldSet>`, so we instead use
        // `Valid::assume_valid()` here.
        (Valid::assume_valid(field_set), is_modified)
    } else {
        (
            FieldSet::parse_and_validate(schema, parent_type_name, field_set, "field_set.graphql")?,
            false,
        )
    };
    Ok((field_set.into_inner().selection_set, is_modified))
}

/// In the past, arguments in field sets may have mistakenly provided strings when they meant to
/// provide enum values. This was erroneously permitted because the representation of a GraphQL
/// value in JS doesn't really distinguish between the two, and the JS GraphQL value validation code
/// strangely permits enum values when a string type is expected. This ends up being okay-ish in the
/// JS code, since it later ends up using the type of the argument to try inferring whether it
/// should be a string or enum value.
///
/// This doesn't occur in Rust, since the representation of a GraphQL value properly distinguishes
/// between strings and enum values, and validation checks this accordingly. However, to continue
/// accepting supergraph schemas that may have this mistake, the Rust code does this type inference
/// when extracting subgraphs instead. This inference is performed by this function, flipping the
/// type from string to enum value when relevant (if the field set is invalid in some other way,
/// this function skips the invalidity, assuming that later field set validation will catch this).
/// In a future major release, we may error on such string/enum value mistakes instead.
fn fix_string_enum_values_in_field_set(schema: &Valid<Schema>, field_set: &mut FieldSet) -> bool {
    fix_string_enum_values_in_selection_set(schema, &mut field_set.selection_set)
}

fn fix_string_enum_values_in_selection_set(
    schema: &Valid<Schema>,
    selection_set: &mut executable::SelectionSet,
) -> bool {
    let mut is_modified = false;
    for selection in selection_set.selections.iter_mut() {
        match selection {
            executable::Selection::Field(field) => {
                let field = field.make_mut();
                for argument in field.arguments.iter_mut() {
                    let Some(type_) = field
                        .definition
                        .argument_by_name(&argument.name)
                        .map(|def| &def.ty)
                    else {
                        continue;
                    };
                    let argument = argument.make_mut();
                    if fix_string_enum_values_in_input_value(schema, type_, &mut argument.value) {
                        is_modified = true;
                    }
                }
                if fix_string_enum_values_in_directives(schema, &mut field.directives) {
                    is_modified = true;
                }
                if fix_string_enum_values_in_selection_set(schema, &mut field.selection_set) {
                    is_modified = true;
                }
            }
            executable::Selection::FragmentSpread(fragment_spread) => {
                let fragment_spread = fragment_spread.make_mut();
                if fix_string_enum_values_in_directives(schema, &mut fragment_spread.directives) {
                    is_modified = true;
                }
            }
            executable::Selection::InlineFragment(inline_fragment) => {
                let inline_fragment = inline_fragment.make_mut();
                if fix_string_enum_values_in_directives(schema, &mut inline_fragment.directives) {
                    is_modified = true;
                }
                if fix_string_enum_values_in_selection_set(
                    schema,
                    &mut inline_fragment.selection_set,
                ) {
                    is_modified = true;
                }
            }
        }
    }
    is_modified
}

fn fix_string_enum_values_in_directives(
    schema: &Valid<Schema>,
    directives: &mut executable::DirectiveList,
) -> bool {
    let mut is_modified = false;
    for directive in directives.iter_mut() {
        let Some(directive_definition) = schema.directive_definitions.get(&directive.name) else {
            continue;
        };
        let directive = directive.make_mut();
        for argument in directive.arguments.iter_mut() {
            let Some(type_) = directive_definition
                .argument_by_name(&argument.name)
                .map(|def| &def.ty)
            else {
                continue;
            };
            let argument = argument.make_mut();
            if fix_string_enum_values_in_input_value(schema, type_, &mut argument.value) {
                is_modified = true;
            }
        }
    }
    is_modified
}

fn fix_string_enum_values_in_input_value(
    schema: &Valid<Schema>,
    type_: &executable::Type,
    value: &mut Node<executable::Value>,
) -> bool {
    let mut is_modified = false;
    let Some(type_definition) = schema.types.get(type_.inner_named_type()) else {
        return is_modified;
    };
    let value = value.make_mut();
    match value {
        executable::Value::Int(_)
        | executable::Value::Float(_)
        | executable::Value::Boolean(_)
        | executable::Value::Null
        | executable::Value::Variable(_)
        | executable::Value::Enum(_) => {}
        executable::Value::String(string_value) => {
            let ExtendedType::Enum(type_definition) = type_definition else {
                return is_modified;
            };
            let Ok(enum_value) = executable::Name::new(string_value) else {
                return is_modified;
            };
            if !type_definition.values.contains_key(&enum_value) {
                return is_modified;
            }
            *value = executable::Value::Enum(enum_value);
            is_modified = true;
        }
        executable::Value::List(values) => {
            if !type_.is_list() {
                return is_modified;
            }
            let type_ = type_.item_type();
            for value in values {
                if fix_string_enum_values_in_input_value(schema, type_, value) {
                    is_modified = true;
                }
            }
        }
        executable::Value::Object(values) => {
            let ExtendedType::InputObject(type_definition) = type_definition else {
                return is_modified;
            };
            for (key, value) in values {
                let Some(type_) = type_definition.fields.get(key).map(|def| &def.ty) else {
                    continue;
                };
                if fix_string_enum_values_in_input_value(schema, type_, value) {
                    is_modified = true;
                }
            }
        }
    }
    is_modified
}

// PORT_NOTE: The JS codebase called this `collectTargetFields()`, but this naming didn't make it
// apparent that this was collecting from a field set, so we've renamed it accordingly. Note that
// the JS function also optionally collected interface field implementations, but we've split that
// off into a separate function.
pub(crate) fn collect_target_fields_from_field_set(
    schema: &Valid<Schema>,
    parent_type_name: NamedType,
    field_set: &str,
    validate: bool,
) -> Result<Vec<FieldDefinitionPosition>, FederationError> {
    // Note this parsing takes care of adding curly braces ("{" and "}") if they aren't in the string.
    let field_set = if validate {
        FieldSet::parse_and_validate(schema, parent_type_name, field_set, "field_set.graphql")?
    } else {
        // This case exists for when a directive's field set uses an interface I with implementer O, and conditions
        // I on O, but the actual phrase "type O implements I" only exists in another subgraph. Ideally, this wouldn't
        // be allowed, but it would be a breaking change to remove it, thus it's supported for legacy reasons.
        Valid::assume_valid(
            FieldSet::parse(schema, parent_type_name, field_set, "field_set.graphql")
                // If we failed to parse, we want to continue collecting leaf fields from the partial result. This is
                // useful for when we are collecting used fields, for example, so we can avoid extra error messages
                // about fields that are used in otherwise invalid field sets.
                .unwrap_or_else(|e| e.partial),
        )
    };
    let mut stack = vec![&field_set.selection_set];
    let mut fields = vec![];
    while let Some(selection_set) = stack.pop() {
        let Some(parent_type) = schema.types.get(&selection_set.ty) else {
            if validate {
                return Err(FederationError::internal(
                    "Unexpectedly missing selection set type from schema.",
                ));
            } else {
                continue;
            }
        };
        let parent_type_position: CompositeTypeDefinitionPosition = match parent_type {
            ExtendedType::Object(_) => ObjectTypeDefinitionPosition {
                type_name: selection_set.ty.clone(),
            }
            .into(),
            ExtendedType::Interface(_) => InterfaceTypeDefinitionPosition {
                type_name: selection_set.ty.clone(),
            }
            .into(),
            ExtendedType::Union(_) => UnionTypeDefinitionPosition {
                type_name: selection_set.ty.clone(),
            }
            .into(),
            _ => {
                if validate {
                    return Err(FederationError::internal(
                        "Unexpectedly encountered non-composite type for selection set.",
                    ));
                } else {
                    continue;
                }
            }
        };
        // The stack iterates through what we push in reverse order, so we iterate through
        // selections in reverse order to fix it.
        for selection in selection_set.selections.iter().rev() {
            match selection {
                executable::Selection::Field(field) => {
                    fields.push(parent_type_position.field(field.name.clone())?);
                    if !field.selection_set.selections.is_empty() {
                        stack.push(&field.selection_set);
                    }
                }
                executable::Selection::FragmentSpread(_) => {
                    if validate {
                        return Err(FederationError::internal(
                            "Unexpectedly encountered fragment spread in FieldSet.",
                        ));
                    } else {
                        continue;
                    }
                }
                executable::Selection::InlineFragment(inline_fragment) => {
                    stack.push(&inline_fragment.selection_set);
                }
            }
        }
    }
    Ok(fields)
}

pub(crate) fn parse_field_value_without_validation(
    schema: &ValidFederationSchema,
    parent_type_name: NamedType,
    field_value: &str,
) -> Result<FieldSet, FederationError> {
    // Note this parsing takes care of adding curly braces ("{" and "}") if they aren't in the
    // string.
    Ok(FieldSet::parse(
        schema.schema(),
        parent_type_name,
        field_value,
        "field_set.graphql",
    )?)
}

// Similar to parse_field_set(), we explicitly forbid aliases for field values. In this case though,
// it's because field value evaluation semantics means aliases would be stripped out and have no
// effect.
pub(crate) fn validate_field_value(
    schema: &ValidFederationSchema,
    field_value: FieldSet,
) -> Result<SelectionSet, FederationError> {
    field_value.validate(schema.schema())?;

    // A field value should not contain any named fragments.
    let fragments = Default::default();
    let selection_set =
        SelectionSet::from_selection_set(&field_value.selection_set, &fragments, schema, &|| {
            // never cancel
            Ok(())
        })?;

    // Validate that the field value has no aliases.
    check_absence_of_aliases(&selection_set)?;

    Ok(selection_set)
}

#[cfg(test)]
mod tests {
    use apollo_compiler::Name;

    use crate::Supergraph;
    use crate::error::FederationError;
    use crate::query_graph::build_federated_query_graph;

    #[test]
    fn test_aliases_in_field_set() -> Result<(), FederationError> {
        // Note: `field-set-alias.graphqls` has multiple alias errors in the same field set.
        let schema_str = include_str!("fixtures/field-set-alias.graphqls");
        let supergraph = Supergraph::new(schema_str).expect("Expected supergraph schema to parse");
        // Note: `Supergraph::new` does not error out on aliases in field sets.
        // We call `parse_field_set` directly to test the alias error.
        let err =
            super::parse_field_set(&supergraph.schema, Name::new("T").unwrap(), "r1: r", true)
                .expect_err("Expected alias error");
        assert_eq!(
            err.to_string(),
            r#"Cannot use alias "r1" in "r1: r": aliases are not currently supported in the used directive"#
        );
        Ok(())
    }

    #[test]
    fn test_aliases_in_field_set_via_build_federated_query_graph() -> Result<(), FederationError> {
        // Note: `field-set-alias.graphqls` has multiple alias errors in the same field set.
        let schema_str = include_str!("fixtures/field-set-alias.graphqls");
        let supergraph = Supergraph::new(schema_str).expect("Expected supergraph schema to parse");
        let api_schema = supergraph.to_api_schema(Default::default())?;
        // Testing via `build_federated_query_graph` function, which validates the @requires directive.
        let err = build_federated_query_graph(supergraph.schema, api_schema, None, None)
            .expect_err("Expected alias error");
        assert_eq!(
            err.to_string(),
            r#"The following errors occurred:
  - Cannot use alias "r1" in "r1: r": aliases are not currently supported in the used directive
  - Cannot use alias "q1" in "q1: q": aliases are not currently supported in the used directive"#
        );
        Ok(())
    }
}
