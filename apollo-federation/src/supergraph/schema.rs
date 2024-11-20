use apollo_compiler::schema::SchemaBuilder;

use crate::error::FederationError;
use crate::schema::FederationSchema;

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
