use apollo_compiler::collections::HashMap;
use apollo_compiler::schema::SchemaBuilder;
use apollo_compiler::Name;

use crate::error::FederationError;
use crate::link::spec::APOLLO_SPEC_DOMAIN;
use crate::link::Link;
use crate::schema::FederationSchema;

/// Builds a map of original name to new name for Apollo feature directives. This is
/// used to handle cases where a directive is renamed via an import statement. For
/// example, importing a directive with a custom name like
/// ```graphql
/// @link(url: "https://specs.apollo.dev/cost/v0.1", import: [{ name: "@cost", as: "@renamedCost" }])
/// ```
/// results in a map entry of `cost -> renamedCost` with the `@` prefix removed.
///
/// If the directive is imported under its default name, that also results in an entry. So,
/// ```graphql
/// @link(url: "https://specs.apollo.dev/cost/v0.1", import: ["@cost"])
/// ```
/// results in a map entry of `cost -> cost`. This duals as a way to check if a directive
/// is included in the supergraph schema.
///
/// **Important:** This map does _not_ include directives imported from identities other
/// than `specs.apollo.dev`. This helps us avoid extracting directives to subgraphs
/// when a custom directive's name conflicts with that of a default one.
pub(super) fn get_apollo_directive_names(
    supergraph_schema: &FederationSchema,
) -> Result<HashMap<Name, Name>, FederationError> {
    let mut hm: HashMap<Name, Name> = HashMap::default();
    for directive in &supergraph_schema.schema().schema_definition.directives {
        if directive.name.as_str() == "link" {
            if let Ok(link) = Link::from_directive_application(directive) {
                if link.url.identity.domain != APOLLO_SPEC_DOMAIN {
                    continue;
                }
                for import in link.imports {
                    hm.insert(import.element.clone(), import.imported_name().clone());
                }
            }
        }
    }
    Ok(hm)
}

/// TODO: Use the JS/programmatic approach instead of hard-coding definitions.
pub(crate) fn new_empty_fed_2_subgraph_schema() -> Result<FederationSchema, FederationError> {
    let builder = SchemaBuilder::new().adopt_orphan_extensions();
    let builder = builder.parse(
        r#"
    extend schema
        @link(url: "https://specs.apollo.dev/link/v1.0")
        @link(url: "https://specs.apollo.dev/federation/v2.9")

    directive @link(url: String, as: String, for: link__Purpose, import: [link__Import]) repeatable on SCHEMA

    scalar link__Import

    enum link__Purpose {
        """
        \`SECURITY\` features provide metadata necessary to securely resolve fields.
        """
        SECURITY

        """
        \`EXECUTION\` features provide metadata necessary for operation execution.
        """
        EXECUTION
    }

    directive @federation__key(fields: federation__FieldSet!, resolvable: Boolean = true) repeatable on OBJECT | INTERFACE

    directive @federation__requires(fields: federation__FieldSet!) on FIELD_DEFINITION

    directive @federation__provides(fields: federation__FieldSet!) on FIELD_DEFINITION

    directive @federation__external(reason: String) on OBJECT | FIELD_DEFINITION

    directive @federation__tag(name: String!) repeatable on FIELD_DEFINITION | OBJECT | INTERFACE | UNION | ARGUMENT_DEFINITION | SCALAR | ENUM | ENUM_VALUE | INPUT_OBJECT | INPUT_FIELD_DEFINITION | SCHEMA

    directive @federation__extends on OBJECT | INTERFACE

    directive @federation__shareable on OBJECT | FIELD_DEFINITION

    directive @federation__inaccessible on FIELD_DEFINITION | OBJECT | INTERFACE | UNION | ARGUMENT_DEFINITION | SCALAR | ENUM | ENUM_VALUE | INPUT_OBJECT | INPUT_FIELD_DEFINITION

    directive @federation__override(from: String!, label: String) on FIELD_DEFINITION

    directive @federation__composeDirective(name: String) repeatable on SCHEMA

    directive @federation__interfaceObject on OBJECT

    directive @federation__authenticated on FIELD_DEFINITION | OBJECT | INTERFACE | SCALAR | ENUM

    directive @federation__requiresScopes(scopes: [[federation__Scope!]!]!) on FIELD_DEFINITION | OBJECT | INTERFACE | SCALAR | ENUM

    directive @federation__cost(weight: Int!) on ARGUMENT_DEFINITION | ENUM | FIELD_DEFINITION | INPUT_FIELD_DEFINITION | OBJECT | SCALAR

    directive @federation__listSize(assumedSize: Int, slicingArguments: [String!], sizedFields: [String!], requireOneSlicingArgument: Boolean = true) on FIELD_DEFINITION

    scalar federation__FieldSet

    scalar federation__Scope
    "#,
        "subgraph.graphql",
    );
    FederationSchema::new(builder.build()?)
}
