use apollo_compiler::Schema;
use apollo_federation::{ValidFederationSubgraph, ValidFederationSubgraphs};
use apollo_federation::schema::ValidFederationSchema;

/// Builder for creating test subgraphs with fluent API
#[derive(Debug, Clone)]
pub struct SubgraphBuilder {
    name: String,
    url: Option<String>,
    schema_sdl: Option<String>,
}

impl SubgraphBuilder {
    /// Create a new subgraph builder with the given name
    pub fn new(name: &str) -> Self {
        Self {
            name: name.to_string(),
            url: None,
            schema_sdl: None,
        }
    }

    /// Set the URL for the subgraph
    pub fn url(mut self, url: &str) -> Self {
        self.url = Some(url.to_string());
        self
    }

    /// Set the schema SDL for the subgraph
    pub fn schema(mut self, schema_sdl: &str) -> Self {
        self.schema_sdl = Some(schema_sdl.to_string());
        self
    }

    /// Build the ValidFederationSubgraph
    pub fn build(self) -> ValidFederationSubgraph {
        let url = self.url.unwrap_or_else(|| format!("https://{}", self.name));
        let schema_sdl = self.schema_sdl.expect("Schema SDL is required");
        
        let schema = Schema::parse_and_validate(&schema_sdl, &format!("{}.graphql", self.name))
            .expect("Failed to parse schema");
        
        let federation_schema = ValidFederationSchema::new(schema)
            .expect("Failed to create valid federation schema");
        
        ValidFederationSubgraph {
            name: self.name,
            url,
            schema: federation_schema,
        }
    }
}

/// Macro for creating subgraphs with a more concise syntax
/// Similar to the existing subgraphs! macro but for the new test structure
#[macro_export]
macro_rules! test_subgraphs {
    ($($name:expr => $schema:expr),* $(,)?) => {{
        let mut subgraphs = apollo_federation::ValidFederationSubgraphs::new();
        $(
            let subgraph = $crate::test_helpers::SubgraphBuilder::new($name)
                .schema($schema)
                .build();
            subgraphs.add(subgraph).expect("Failed to add subgraph");
        )*
        subgraphs
    }};
}

/// Macro for creating subgraphs from files (similar to existing pattern)
#[macro_export]
macro_rules! test_subgraphs_from_files {
    ($($name:expr => $file:expr),* $(,)?) => {{
        let mut subgraphs = apollo_federation::ValidFederationSubgraphs::new();
        $(
            let schema_sdl = include_str!($file);
            let subgraph = $crate::test_helpers::SubgraphBuilder::new($name)
                .schema(schema_sdl)
                .build();
            subgraphs.add(subgraph).expect("Failed to add subgraph");
        )*
        subgraphs
    }};
}

/// Helper function to create a collection of subgraphs from builders
pub fn build_subgraphs(builders: Vec<SubgraphBuilder>) -> ValidFederationSubgraphs {
    let mut subgraphs = ValidFederationSubgraphs::new();
    
    for builder in builders {
        let subgraph = builder.build();
        subgraphs.add(subgraph).expect("Failed to add subgraph");
    }
    
    subgraphs
}