use apollo_compiler::ast::Directive;
use apollo_compiler::ast::DirectiveDefinition;
use apollo_compiler::ast::NamedType;
use apollo_compiler::Name;
use apollo_compiler::Schema;

use crate::error::FederationError;
use crate::link::spec::Url;
use crate::link::Import;
use crate::link::Purpose;

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
        _schema: &Schema,
        _directive: &Directive,
    ) -> Result<Option<DirectiveDefinition>, FederationError>;

    fn on_directive_definition_and_schema_parsed(_: &Schema) -> Option<FederationError>;

    fn ignore_parsed_field(_type: NamedType, _field_name: &str) -> bool;

    fn on_constructed(_: &Schema);

    fn on_added_core_feature(_schema: &Schema, _feature: &CoreFeature);

    fn on_invalidation(_: &Schema);

    fn on_validation(_schema: &Schema) -> Option<FederationError>;

    fn on_unknown_directive_validation_error(
        _schema: &Schema,
        _unknown_directive_name: &str,
        _error: FederationError,
    ) -> FederationError;

    fn apply_directives_after_parsing() -> bool;
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
        _schema: &Schema,
        _directive: &Directive,
    ) -> Result<Option<DirectiveDefinition>, FederationError> {
        todo!()
    }

    fn on_directive_definition_and_schema_parsed(_: &Schema) -> Option<FederationError> {
        todo!()
    }

    fn ignore_parsed_field(_type: NamedType, _field_name: &str) -> bool {
        todo!()
    }

    fn on_constructed(_: &Schema) {
        todo!()
    }

    fn on_added_core_feature(_schema: &Schema, _feature: &CoreFeature) {
        todo!()
    }

    fn on_invalidation(_: &Schema) {
        todo!()
    }

    fn on_validation(_schema: &Schema) -> Option<FederationError> {
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
