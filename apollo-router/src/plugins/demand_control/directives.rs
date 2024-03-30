use apollo_compiler::ast::NamedType;
use apollo_compiler::executable::Field;
use apollo_compiler::executable::SelectionSet;
use apollo_compiler::validation::Valid;
use apollo_compiler::Parser;
use apollo_compiler::Schema;
use tower::BoxError;

use super::DemandControlError;

pub(super) struct CostDirective {
    pub(super) weight: f64,
}

impl CostDirective {
    pub(super) fn from_field(field: &Field) -> Result<Option<Self>, DemandControlError> {
        let directive = field
            .definition
            .directives
            .get("cost")
            .and_then(|cost| cost.argument_by_name("weight"))
            .and_then(|arg| arg.to_f64())
            .map(|weight| Self { weight });

        Ok(directive)
    }

    pub(super) fn weight(&self) -> f64 {
        self.weight
    }
}

pub(super) struct IncludeDirective {
    pub(super) is_included: bool,
}

impl IncludeDirective {
    pub(super) fn from_field(field: &Field) -> Result<Option<Self>, BoxError> {
        let directive = field
            .directives
            .get("include")
            .and_then(|skip| skip.argument_by_name("if"))
            .and_then(|arg| arg.to_bool())
            .map(|cond| Self { is_included: cond });

        Ok(directive)
    }
}

pub(super) struct RequiresDirective {
    pub(super) fields: SelectionSet,
}

impl RequiresDirective {
    pub(super) fn from_field(
        field: &Field,
        parent_type_name: Option<&NamedType>,
        schema: &Valid<Schema>,
    ) -> Result<Option<Self>, DemandControlError> {
        // When a user marks a subgraph schema field with `@requires`, the composition process
        // replaces `@requires(field: "<selection>")` with `@join__field(requires: "<selection>")`.
        let requires_arg = field
            .definition
            .directives
            .get("join__field")
            .and_then(|requires| requires.argument_by_name("requires"))
            .and_then(|arg| arg.as_str());

        match (requires_arg, parent_type_name) {
            (Some(arg), Some(type_name)) => {
                let field_set = Parser::new()
                    .parse_field_set(schema, type_name.clone(), arg, "")
                    .map_err(|e| DemandControlError::QueryParseFailure(format!("{}", e)))?;

                Ok(Some(RequiresDirective {
                    fields: field_set.selection_set.clone(),
                }))
            }
            (Some(_), None) => Err(DemandControlError::QueryParseFailure("Parent type name is required to parse fields argument of @requires but none was provided. This is likely because @requires was placed on an anonymous query.".to_string())),
            (None, _) => Ok(None)
        }
    }
}

pub(super) struct SkipDirective {
    pub(super) is_skipped: bool,
}

impl SkipDirective {
    pub(super) fn from_field(field: &Field) -> Result<Option<Self>, BoxError> {
        let directive = field
            .directives
            .get("skip")
            .and_then(|skip| skip.argument_by_name("if"))
            .and_then(|arg| arg.to_bool())
            .map(|cond| Self { is_skipped: cond });

        Ok(directive)
    }
}
