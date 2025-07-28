use apollo_compiler::Schema;
use apollo_federation::merge::{MergeFailure, MergeSuccess, merge_federation_subgraphs};
use apollo_federation::{ValidFederationSubgraph, ValidFederationSubgraphs};
use apollo_federation::schema::ValidFederationSchema;

/// Extract schemas from a successful composition result
/// Returns (supergraph_schema, composition_hints)
pub fn extract_schemas(success: &MergeSuccess) -> (&Schema, &[String]) {
    (&success.schema, &success.composition_hints)
}

/// Compose subgraphs as Federation 2.0 subgraphs
pub fn compose_as_fed2_subgraphs(
    subgraphs: ValidFederationSubgraphs,
) -> Result<MergeSuccess, MergeFailure> {
    merge_federation_subgraphs(subgraphs)
}

/// Create a ValidFederationSubgraphs collection from a list of (name, schema_sdl) pairs
pub fn create_subgraphs_from_sdl(subgraphs: &[(&str, &str)]) -> ValidFederationSubgraphs {
    let mut federation_subgraphs = ValidFederationSubgraphs::new();
    
    for (name, schema_sdl) in subgraphs {
        let schema = Schema::parse_and_validate(schema_sdl, &format!("{}.graphql", name))
            .expect("Failed to parse schema");
        
        let federation_schema = ValidFederationSchema::new(schema)
            .expect("Failed to create valid federation schema");
        
        let subgraph = ValidFederationSubgraph {
            name: name.to_string(),
            url: format!("https://{}", name),
            schema: federation_schema,
        };
        
        federation_subgraphs.add(subgraph)
            .expect("Failed to add subgraph");
    }
    
    federation_subgraphs
}

/// Serialize a schema to SDL string for comparison
pub fn serialize_schema(schema: &Schema) -> String {
    let mut schema = schema.clone();
    schema.types.sort_keys();
    schema.directive_definitions.sort_keys();
    schema.to_string()
}

/// Create a basic Federation 2.0 schema template
pub fn fed2_schema_template(type_defs: &str) -> String {
    format!(
        r#"
extend schema
    @link(url: "https://specs.apollo.dev/federation/v2.0", import: ["@key", "@shareable", "@external", "@requires", "@provides", "@tag", "@inaccessible"])

{}
        "#,
        type_defs.trim()
    )
}

/// Create a basic subgraph with Query type
pub fn basic_subgraph_template(name: &str, type_defs: &str) -> String {
    fed2_schema_template(&format!(
        r#"
type Query {{
    _dummy: String
}}

{}
        "#,
        type_defs.trim()
    ))
}