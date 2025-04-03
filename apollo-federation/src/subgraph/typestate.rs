use apollo_compiler::Schema;
use apollo_compiler::name;

use crate::LinkSpecDefinition;
use crate::ValidFederationSchema;
use crate::error::FederationError;
use crate::internal_error;
use crate::link::federation_spec_definition::add_fed1_link_to_schema;
use crate::schema::FederationSchema;
use crate::schema::blueprint::FederationBlueprint;
use crate::schema::compute_subgraph_metadata;
use crate::schema::subgraph_metadata::SubgraphMetadata;

pub trait SubgraphState {}

pub struct Raw {
    schema: Schema,
}
impl SubgraphState for Raw {}

pub struct Expanded {
    schema: FederationSchema,
    metadata: SubgraphMetadata,
}
impl SubgraphState for Expanded {}

pub struct Validated {
    schema: ValidFederationSchema,
    metadata: SubgraphMetadata,
}
impl SubgraphState for Validated {}

pub struct Subgraph<S: SubgraphState> {
    pub name: String,
    pub url: String,
    pub state: S,
}

impl Subgraph<Raw> {
    pub fn new(name: &str, url: &str, schema: Schema) -> Subgraph<Raw> {
        Subgraph {
            name: name.to_string(),
            url: url.to_string(),
            state: Raw { schema },
        }
    }

    pub fn parse(
        name: &str,
        url: &str,
        schema_str: &str,
    ) -> Result<Subgraph<Raw>, FederationError> {
        let schema = Schema::builder()
            .adopt_orphan_extensions()
            .parse(schema_str, name)
            .build()?;

        Ok(Self::new(name, url, schema))
    }

    pub fn assume_expanded(self) -> Result<Subgraph<Expanded>, FederationError> {
        let schema = FederationSchema::new(self.state.schema)?;
        let metadata = compute_subgraph_metadata(&schema)?.ok_or_else(|| {
            internal_error!(
                "Unable to detect federation version used in subgraph '{}'",
                self.name
            )
        })?;

        Ok(Subgraph {
            name: self.name,
            url: self.url,
            state: Expanded { schema, metadata },
        })
    }

    pub fn expand(self) -> Result<Subgraph<Expanded>, FederationError> {
        let mut schema = FederationSchema::new_uninitialized(self.state.schema)?;
        // First, copy types over from the underlying schema AST to make sure we have built-ins that directives may reference
        schema.collect_shallow_references();

        // Backfill missing directive definitions. This is primarily making sure we have a definition for `@link`.
        for directive in &schema.schema().schema_definition.directives.clone() {
            if schema.get_directive_definition(&directive.name).is_none() {
                FederationBlueprint::on_missing_directive_definition(&mut schema, directive)?;
            }
        }

        // If there's a use of `@link`, and we successfully added its definition, add the bootstrap directive
        if schema.get_directive_definition(&name!("link")).is_some() {
            LinkSpecDefinition::latest().add_to_schema(&mut schema, /*alias*/ None)?;
        } else {
            // This must be a Fed 1 schema.
            LinkSpecDefinition::fed1_latest().add_to_schema(&mut schema, /*alias*/ None)?;

            // PORT_NOTE: JS doesn't actually add the 1.0 federation spec link to the schema. In
            //            Rust, we add it, so that fed 1 and fed 2 can be processed the same way.
            add_fed1_link_to_schema(&mut schema)?;
        }

        // Now that we have the definition for `@link` and an application, the bootstrap directive detection should work.
        schema.collect_links_metadata()?;

        FederationBlueprint::on_directive_definition_and_schema_parsed(&mut schema)?;

        // Also, the backfilled definitions mean we can collect deep references.
        schema.collect_deep_references()?;

        let metadata = compute_subgraph_metadata(&schema)?.ok_or_else(|| {
            internal_error!(
                "Unable to detect federation version used in subgraph '{}'",
                self.name
            )
        })?;

        Ok(Subgraph {
            name: self.name,
            url: self.url,
            state: Expanded { schema, metadata },
        })
    }
}

impl Subgraph<Expanded> {
    pub fn upgrade(&mut self) -> Result<Self, FederationError> {
        todo!("Implement upgrade logic for expanded subgraphs");
    }

    pub fn validate(mut self) -> Result<Subgraph<Validated>, FederationError> {
        let blueprint = FederationBlueprint::new(true); // TODO: Check if there are paths with this as false
        blueprint.on_validation(&mut self.state.schema)?;
        let schema = self
            .state
            .schema
            .validate_or_return_self()
            .map_err(|t| t.1)?;

        Ok(Subgraph {
            name: self.name,
            url: self.url,
            state: Validated {
                schema,
                metadata: self.state.metadata,
            },
        })
    }
}

impl Subgraph<Validated> {
    pub fn invalidate(self) -> Result<Subgraph<Expanded>, FederationError> {
        Ok(Subgraph {
            name: self.name,
            url: self.url,
            state: Expanded {
                // Other holders may still need the data in the `Arc`, so we clone the contents to allow mutation later
                schema: (*self.state.schema).clone(),
                metadata: self.state.metadata,
            },
        })
    }
}
