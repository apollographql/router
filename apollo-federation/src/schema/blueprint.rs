use apollo_compiler::Name;
use apollo_compiler::Schema;
use apollo_compiler::ast::Directive;
use apollo_compiler::ast::DirectiveDefinition;

use crate::error::FederationError;
use crate::link::Import;
use crate::link::Purpose;
use crate::link::federation_spec_definition::FEDERATION_VERSIONS;
use crate::link::spec::Identity;
use crate::link::spec::Url;
use crate::link::spec_definition::SpecDefinition;
use crate::schema::FederationSchema;
use crate::schema::compute_subgraph_metadata;

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
        _schema: &mut Schema,
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
        _schema: &mut Schema,
        _directive: &Directive,
    ) -> Result<Option<DirectiveDefinition>, FederationError> {
        Ok(None)
    }

    fn on_directive_definition_and_schema_parsed(_: &mut Schema) -> Result<(), FederationError> {
        Ok(())
    }

    fn ignore_parsed_field(schema: &FederationSchema, _field_name: &str) -> bool {
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
        _schema: &mut Schema,
        _directive: &Directive,
    ) -> Result<Option<DirectiveDefinition>, FederationError> {
        todo!()
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
    use crate::error::FederationError;
    use crate::link::federation_spec_definition::FederationSpecDefinition;
    use crate::link::spec::Version;
    use crate::schema::FederationSchema;
    use crate::schema::ValidFederationSchema;
    use crate::schema::subgraph_metadata::SubgraphMetadata;

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

    fn build_subgraph(
        source: &Schema,
        with_root_type_renaming: bool,
    ) -> Result<ValidFederationSchema, FederationError> {
        let blueprint = FederationBlueprint::new(with_root_type_renaming);
        let subgraph = build_schema(source, &blueprint)?;
        subgraph.validate_or_return_self().map_err(|(_, err)| err)
    }

    fn build_schema<B: SchemaBlueprint>(
        source: &Schema,
        blueprint: &B,
    ) -> Result<FederationSchema, FederationError> {
        let mut schema =
            FederationSchema::new(source.clone()).expect("constructs federation schema");
        blueprint.on_constructed(&mut schema)?;
        Ok(schema)
    }
}
