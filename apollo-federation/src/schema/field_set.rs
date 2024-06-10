use apollo_compiler::executable;
use apollo_compiler::executable::FieldSet;
use apollo_compiler::schema::ExtendedType;
use apollo_compiler::schema::NamedType;
use apollo_compiler::validation::Valid;
use apollo_compiler::NodeStr;
use apollo_compiler::Schema;
use indexmap::IndexMap;

use crate::error::FederationError;
use crate::error::MultipleFederationErrors;
use crate::error::SingleFederationError;
use crate::operation::NamedFragments;
use crate::operation::SelectionSet;
use crate::schema::position::CompositeTypeDefinitionPosition;
use crate::schema::position::FieldDefinitionPosition;
use crate::schema::position::InterfaceTypeDefinitionPosition;
use crate::schema::position::ObjectTypeDefinitionPosition;
use crate::schema::position::UnionTypeDefinitionPosition;
use crate::schema::FederationSchema;
use crate::schema::ValidFederationSchema;

// Federation spec does not allow the alias syntax in field set strings.
// However, since `parse_field_set` uses the standard GraphQL parser, which allows aliases,
// we need this secondary check to ensure that aliases are not used.
fn check_absence_of_aliases(
    field_set: &Valid<FieldSet>,
    code_str: &str,
) -> Result<(), FederationError> {
    let aliases = field_set.selection_set.fields().filter_map(|field| {
        field.alias.as_ref().map(|alias|
            SingleFederationError::UnsupportedFeature {
                // PORT_NOTE: The JS version also quotes the directive name in the error message.
                //            For example, "aliases are not currently supported in @requires".
                message: format!(
                    r#"Cannot use alias "{}" in "{}": aliases are not currently supported in the used directive"#,
                    alias, code_str)
            })
    });
    MultipleFederationErrors::from_iter(aliases).into_result()
}

// TODO: In the JS codebase, this has some error-rewriting to help give the user better hints around
// non-existent fields.
pub(crate) fn parse_field_set(
    schema: &ValidFederationSchema,
    parent_type_name: NamedType,
    value: &str,
) -> Result<SelectionSet, FederationError> {
    // Note this parsing takes care of adding curly braces ("{" and "}") if they aren't in the
    // string.
    let field_set = FieldSet::parse_and_validate(
        schema.schema(),
        parent_type_name,
        value,
        "field_set.graphql",
    )?;

    // Validate the field set has no aliases.
    check_absence_of_aliases(&field_set, value)?;

    // field set should not contain any named fragments
    let named_fragments = NamedFragments::new(&IndexMap::new(), schema);
    SelectionSet::from_selection_set(&field_set.selection_set, &named_fragments, schema)
}

/// This exists because there's a single callsite in extract_subgraphs_from_supergraph() that needs
/// to parse field sets before the schema has finished building. Outside that case, you should
/// always use `parse_field_set()` instead.
// TODO: As noted in the single callsite, ideally we could move the parsing to after extraction, but
// it takes time to determine whether that impacts correctness, so we're leaving it for later.
pub(crate) fn parse_field_set_without_normalization(
    schema: &Valid<Schema>,
    parent_type_name: NamedType,
    value: &str,
) -> Result<executable::SelectionSet, FederationError> {
    // Note this parsing takes care of adding curly braces ("{" and "}") if they aren't in the
    // string.
    let field_set =
        FieldSet::parse_and_validate(schema, parent_type_name, value, "field_set.graphql")?;
    Ok(field_set.into_inner().selection_set)
}

// PORT_NOTE: The JS codebase called this `collectTargetFields()`, but this naming didn't make it
// apparent that this was collecting from a field set, so we've renamed it accordingly. Note that
// the JS function also optionally collected interface field implementations, but we've split that
// off into a separate function.
pub(crate) fn collect_target_fields_from_field_set(
    schema: &Valid<Schema>,
    parent_type_name: NamedType,
    value: NodeStr,
) -> Result<Vec<FieldDefinitionPosition>, FederationError> {
    // Note this parsing takes care of adding curly braces ("{" and "}") if they aren't in the
    // string.
    let field_set = FieldSet::parse_and_validate(
        schema,
        parent_type_name,
        value.as_str(),
        "field_set.graphql",
    )?;
    let mut stack = vec![&field_set.selection_set];
    let mut fields = vec![];
    while let Some(selection_set) = stack.pop() {
        let Some(parent_type) = schema.types.get(&selection_set.ty) else {
            return Err(FederationError::internal(
                "Unexpectedly missing selection set type from schema.",
            ));
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
                return Err(FederationError::internal(
                    "Unexpectedly encountered non-composite type for selection set.",
                ));
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
                    return Err(FederationError::internal(
                        "Unexpectedly encountered fragment spread in FieldSet.",
                    ));
                }
                executable::Selection::InlineFragment(inline_fragment) => {
                    stack.push(&inline_fragment.selection_set);
                }
            }
        }
    }
    Ok(fields)
}

// PORT_NOTE: This is meant as a companion function for collect_target_fields_from_field_set(), as
// some callers will also want to include interface field implementations.
pub(crate) fn add_interface_field_implementations(
    fields: Vec<FieldDefinitionPosition>,
    schema: &FederationSchema,
) -> Result<Vec<FieldDefinitionPosition>, FederationError> {
    let mut new_fields = vec![];
    for field in fields {
        let interface_field = if let FieldDefinitionPosition::Interface(field) = &field {
            Some(field.clone())
        } else {
            None
        };
        new_fields.push(field);
        if let Some(interface_field) = interface_field {
            for implementing_type in &schema
                .referencers
                .get_interface_type(&interface_field.type_name)?
                .object_types
            {
                new_fields.push(
                    implementing_type
                        .field(interface_field.field_name.clone())
                        .into(),
                );
            }
        }
    }
    Ok(new_fields)
}

#[cfg(test)]
mod tests {
    use apollo_compiler::schema::Name;

    use crate::error::FederationError;
    use crate::query_graph::build_federated_query_graph;
    use crate::subgraph::Subgraph;
    use crate::Supergraph;

    #[test]
    fn test_aliases_in_field_set() -> Result<(), FederationError> {
        let sdl = r#"
        type Query {
            a: Int! @requires(fields: "r1: r")
            r: Int! @external
          }
        "#;

        let subgraph = Subgraph::parse_and_expand("S1", "http://S1", sdl).unwrap();
        let supergraph = Supergraph::compose([&subgraph].to_vec()).unwrap();
        let err = super::parse_field_set(&supergraph.schema, Name::new("Query").unwrap(), "r1: r")
            .map(|_| "Unexpected success") // ignore the Ok value
            .expect_err("Expected alias error");
        assert_eq!(
            err.to_string(),
            r#"Cannot use alias "r1" in "r1: r": aliases are not currently supported in the used directive"#
        );
        Ok(())
    }

    #[test]
    fn test_aliases_in_field_set_via_build_federated_query_graph() -> Result<(), FederationError> {
        // NB: This tests multiple alias errors in the same field set.
        let sdl = r#"
        type Query {
            a: Int! @requires(fields: "r1: r s q1: q")
            r: Int! @external
            s: String! @external
            q: String! @external
          }
        "#;

        let subgraph = Subgraph::parse_and_expand("S1", "http://S1", sdl).unwrap();
        let supergraph = Supergraph::compose([&subgraph].to_vec()).unwrap();
        let api_schema = supergraph.to_api_schema(Default::default())?;
        // Testing via `build_federated_query_graph` function, which validates the @requires directive.
        let err = build_federated_query_graph(supergraph.schema, api_schema, None, None)
            .map(|_| "Unexpected success") // ignore the Ok value
            .expect_err("Expected alias error");
        assert_eq!(
            err.to_string(),
            r#"The following errors occurred:

  - Cannot use alias "r1" in "r1: r s q1: q": aliases are not currently supported in the used directive

  - Cannot use alias "q1" in "r1: r s q1: q": aliases are not currently supported in the used directive"#
        );
        Ok(())
    }
}
