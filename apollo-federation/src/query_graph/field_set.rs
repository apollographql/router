use crate::error::{FederationError, MultipleFederationErrors, SingleFederationError};
use crate::query_plan::operation::{NamedFragments, NormalizedSelectionSet};
use crate::schema::ValidFederationSchema;
use apollo_compiler::executable::{FieldSet, SelectionSet};
use apollo_compiler::schema::NamedType;
use apollo_compiler::validation::Valid;
use apollo_compiler::{NodeStr, Schema};
use indexmap::IndexMap;

// Federation spec does not allow the alias syntax in field set strings.
// However, since `parse_field_set` uses the standard GraphQL parser, which allows aliases,
// we need this secondary check to ensure that aliases are not used.
fn check_absence_of_aliases(
    field_set: &Valid<FieldSet>,
    code_str: &NodeStr,
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
pub(super) fn parse_field_set(
    schema: &ValidFederationSchema,
    parent_type_name: NamedType,
    value: NodeStr,
) -> Result<NormalizedSelectionSet, FederationError> {
    // Note this parsing takes care of adding curly braces ("{" and "}") if they aren't in the
    // string.
    let field_set = FieldSet::parse_and_validate(
        schema.schema(),
        parent_type_name,
        value.as_str(),
        "field_set.graphql",
    )?;

    // Validate the field set has no aliases.
    check_absence_of_aliases(&field_set, &value)?;

    // field set should not contain any named fragments
    let named_fragments = NamedFragments::new(&IndexMap::new(), schema);
    NormalizedSelectionSet::from_selection_set(&field_set.selection_set, &named_fragments, schema)
}

/// This exists because there's a single callsite in extract_subgraphs_from_supergraph() that needs
/// to parse field sets before the schema has finished building. Outside that case, you should
/// always use `parse_field_set()` instead.
// TODO: As noted in the single callsite, ideally we could move the parsing to after extraction, but
// it takes time to determine whether that impacts correctness, so we're leaving it for later.
pub(super) fn parse_field_set_without_normalization(
    schema: &Valid<Schema>,
    parent_type_name: NamedType,
    value: NodeStr,
) -> Result<SelectionSet, FederationError> {
    // Note this parsing takes care of adding curly braces ("{" and "}") if they aren't in the
    // string.
    let field_set = FieldSet::parse_and_validate(
        schema,
        parent_type_name,
        value.as_str(),
        "field_set.graphql",
    )?;
    Ok(field_set.into_inner().selection_set)
}

// unit test
#[cfg(test)]
mod tests {
    use crate::{
        error::FederationError, query_graph::build_federated_query_graph, subgraph::Subgraph,
        Supergraph,
    };
    use apollo_compiler::schema::Name;

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
        let err = super::parse_field_set(
            &supergraph.schema,
            Name::new("Query").unwrap(),
            "r1: r".into(),
        )
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
