use apollo_compiler::Name;
use apollo_compiler::Schema;
use apollo_compiler::ast::Directive;
use apollo_compiler::ast::DirectiveDefinition;

use crate::error::FederationError;
use crate::link::DEFAULT_LINK_NAME;
use crate::link::Import;
use crate::link::Purpose;
use crate::link::federation_spec_definition::FEDERATION_VERSIONS;
use crate::link::link_spec_definition::LINK_VERSIONS;
use crate::link::spec::Identity;
use crate::link::spec::Url;
use crate::link::spec_definition::SpecDefinition;
use crate::schema::FederationSchema;
use crate::schema::compute_subgraph_metadata;

#[allow(dead_code)]
const FEDERATION_OPERATION_FIELDS: [&str; 2] = ["_entities", "_services"];

#[allow(dead_code)]
struct CoreFeature {
    url: Url,
    name_in_schema: Name,
    directive: Directive,
    imports: Vec<Import>,
    purpose: Option<Purpose>,
}

#[allow(dead_code)]
trait SchemaBlueprint {
    fn on_missing_directive_definition(
        &self,
        _schema: &mut FederationSchema,
        _directive: &Directive,
    ) -> Result<Option<DirectiveDefinition>, FederationError>;

    fn on_directive_definition_and_schema_parsed(_: &mut Schema) -> Result<(), FederationError>;

    fn ignore_parsed_field(schema: &FederationSchema, _field_name: &str) -> bool;

    fn on_constructed(&self, _schema: &mut FederationSchema) -> Result<(), FederationError>;

    fn on_added_core_feature(_schema: &mut FederationSchema, _feature: &CoreFeature);

    fn on_invalidation(_: &Schema);

    fn on_validation(_schema: &Schema) -> Result<(), FederationError>;

    fn on_apollo_rs_validation_error(
        _error: apollo_compiler::validation::WithErrors<Schema>,
    ) -> FederationError;

    fn on_unknown_directive_validation_error(
        _schema: &Schema,
        _unknown_directive_name: &str,
        _error: FederationError,
    ) -> FederationError;

    fn apply_directives_after_parsing() -> bool;
}

#[allow(dead_code)]
struct DefaultBlueprint {}

impl SchemaBlueprint for DefaultBlueprint {
    fn on_missing_directive_definition(
        &self,
        _schema: &mut FederationSchema,
        _directive: &Directive,
    ) -> Result<Option<DirectiveDefinition>, FederationError> {
        Ok(None)
    }

    fn on_directive_definition_and_schema_parsed(_: &mut Schema) -> Result<(), FederationError> {
        Ok(())
    }

    fn ignore_parsed_field(_schema: &FederationSchema, _field_name: &str) -> bool {
        false
    }

    fn on_constructed(&self, _schema: &mut FederationSchema) -> Result<(), FederationError> {
        // No-op by default, but used for federation.
        Ok(())
    }

    fn on_added_core_feature(_schema: &mut FederationSchema, _feature: &CoreFeature) {
        // No-op by default, but used for federation.
    }

    fn on_invalidation(_: &Schema) {
        todo!()
    }

    fn on_validation(_schema: &Schema) -> Result<(), FederationError> {
        Ok(())
    }

    fn on_apollo_rs_validation_error(
        _error: apollo_compiler::validation::WithErrors<Schema>,
    ) -> FederationError {
        todo!()
    }

    fn on_unknown_directive_validation_error(
        _schema: &Schema,
        _unknown_directive_name: &str,
        error: FederationError,
    ) -> FederationError {
        error
    }

    fn apply_directives_after_parsing() -> bool {
        false
    }
}

#[allow(dead_code)]
struct FederationBlueprint {
    with_root_type_renaming: bool,
}

#[allow(dead_code)]
impl FederationBlueprint {
    fn new(with_root_type_renaming: bool) -> Self {
        Self {
            with_root_type_renaming,
        }
    }
}

impl SchemaBlueprint for FederationBlueprint {
    fn on_missing_directive_definition(
        &self,
        schema: &mut FederationSchema,
        directive: &Directive,
    ) -> Result<Option<DirectiveDefinition>, FederationError> {
        // TODO: This needs to be generalized for different link names in the same way as bootstrapping process does it
        if directive.name == DEFAULT_LINK_NAME {
            let latest_version = LINK_VERSIONS.versions().last().unwrap();
            let link_spec = LINK_VERSIONS.find(latest_version).unwrap();
            link_spec.add_elements_to_schema(schema)?;
        }
        // TODO: Return the definition
        Ok(None)
    }

    fn on_directive_definition_and_schema_parsed(_: &mut Schema) -> Result<(), FederationError> {
        todo!()
    }

    fn ignore_parsed_field(schema: &FederationSchema, field_name: &str) -> bool {
        if !FEDERATION_OPERATION_FIELDS.contains(&field_name) {
            return false;
        }
        schema
            .subgraph_metadata
            .as_ref()
            .is_some_and(|meta| !meta.is_fed_2_schema())
    }

    fn on_constructed(&self, schema: &mut FederationSchema) -> Result<(), FederationError> {
        if schema.subgraph_metadata.is_none() {
            schema.subgraph_metadata = compute_subgraph_metadata(schema)?.map(Box::new);
        }
        Ok(())
    }

    fn on_added_core_feature(schema: &mut FederationSchema, feature: &CoreFeature) {
        DefaultBlueprint::on_added_core_feature(schema, feature);
        if feature.url.identity == Identity::federation_identity() {
            if let Some(spec) = FEDERATION_VERSIONS.find(&feature.url.version) {
                let _ = spec.add_elements_to_schema(schema);
            }
        }
    }

    fn on_invalidation(_: &Schema) {
        todo!()
    }

    fn on_validation(_schema: &Schema) -> Result<(), FederationError> {
        todo!()
    }

    fn on_apollo_rs_validation_error(
        _error: apollo_compiler::validation::WithErrors<Schema>,
    ) -> FederationError {
        todo!()
    }

    fn on_unknown_directive_validation_error(
        _schema: &Schema,
        _unknown_directive_name: &str,
        _error: FederationError,
    ) -> FederationError {
        todo!()
    }

    fn apply_directives_after_parsing() -> bool {
        todo!()
    }
}

#[cfg(test)]
mod tests {
    use apollo_compiler::Node;
    use apollo_compiler::ast::Argument;
    use apollo_compiler::name;
    use apollo_compiler::schema::Component;

    use crate::error::FederationError;
    use crate::schema::FederationSchema;
    use crate::schema::ValidFederationSchema;

    use super::*;

    #[test]
    fn detects_federation_1_subgraphs_correctly() {
        let schema = Schema::parse(
            r#"
                type Query {
                    s: String
                }"#,
            "empty-fed1-schema.graphqls",
        )
        .expect("valid schema");
        let subgraph = build_subgraph(&schema, true).expect("builds subgraph");
        let metadata = subgraph.subgraph_metadata().expect("has metadata");

        assert!(!metadata.is_fed_2_schema());
    }

    #[test]
    fn detects_federation_2_subgraphs_correctly() {
        let schema = Schema::parse(
            r#"
                extend schema @link(url: "https://specs.apollo.dev/federation/v2.0")

                type Query {
                    s: String
                }"#,
            "empty-fed2-schema.graphqls",
        )
        .expect("valid schema");
        let subgraph = build_subgraph(&schema, true).expect("builds subgraph");
        let metadata = subgraph.subgraph_metadata().expect("has metadata");

        assert!(metadata.is_fed_2_schema());
    }

    fn build_subgraph(
        source: &Schema,
        with_root_type_renaming: bool,
    ) -> Result<ValidFederationSchema, FederationError> {
        let blueprint = FederationBlueprint::new(with_root_type_renaming);
        let subgraph = build_schema(source, &blueprint)?;
        subgraph.validate_or_return_self().map_err(|(_, err)| err)
    }

    fn build_schema<B: SchemaBlueprint>(
        schema: &Schema,
        blueprint: &B,
    ) -> Result<FederationSchema, FederationError> {
        let mut federation_schema = FederationSchema::new_uninitialized(schema.clone())?;

        // First, copy types over from the underlying schema AST to make sure we have built-ins that directives may reference
        federation_schema.collect_shallow_references();

        // Backfill missing directive definitions. This is primarily making sure we have a definition for `@link`.
        for directive in &schema.schema_definition.directives {
            if federation_schema
                .get_directive_definition(&directive.name)
                .is_none()
            {
                blueprint.on_missing_directive_definition(&mut federation_schema, directive)?;
            }
        }

        // If there's a use of `@link`, and we successfully added its definition, add the bootstrap directive
        // TODO: We may need to do the same for `@core` on Fed 1 schemas.
        if federation_schema
            .get_directive_definition(&name!("link"))
            .is_some()
        {
            federation_schema
                .schema
                .schema_definition
                .make_mut()
                .directives
                .insert(
                    0,
                    Component::new(Directive {
                        name: name!("link"),
                        arguments: vec![Node::new(Argument {
                            name: name!("url"),
                            value: "https://specs.apollo.dev/link/v1.0".into(),
                        })],
                    }),
                );
        }

        // Now that we have the definition for `@link` and an application, the bootstrap directive detection should work.
        federation_schema.collect_links_metadata()?;

        // Also, the backfilled definitions mean we can collect deep references.
        federation_schema.collect_deep_references()?;

        // TODO: In JS this happens inside the schema constructor; we should consider if that's the right thing to do here
        // Right now, this is down here because it eagerly evaluates directive usages for SubgraphMetadata, whereas the JS
        // code was lazy and we could call this hook to lazily use federation directives before actually adding their
        // definitions.
        blueprint.on_constructed(&mut federation_schema)?;

        Ok(federation_schema)
    }
}
