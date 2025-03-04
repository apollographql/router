use apollo_compiler::Name;
use apollo_compiler::Schema;
use apollo_compiler::ast::Directive;
use apollo_compiler::ast::DirectiveDefinition;
use apollo_compiler::ast::NamedType;

use crate::error::FederationError;
use crate::link::Import;
use crate::link::Purpose;
use crate::link::federation_spec_definition::FEDERATION_VERSIONS;
use crate::link::spec::Identity;
use crate::link::spec::Url;
use crate::link::spec_definition::SpecDefinition;
use crate::schema::FederationSchema;
use crate::schema::compute_subgraph_metadata;

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

    fn ignore_parsed_field(_type: NamedType, _field_name: &str) -> bool;

    fn on_constructed(_schema: &mut FederationSchema) -> Result<(), FederationError>;

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

    fn ignore_parsed_field(_type: NamedType, _field_name: &str) -> bool {
        false
    }

    fn on_constructed(_schema: &mut FederationSchema) -> Result<(), FederationError> {
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

    fn ignore_parsed_field(_type: NamedType, _field_name: &str) -> bool {
        todo!()
    }

    fn on_constructed(schema: &mut FederationSchema) -> Result<(), FederationError> {
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
