use apollo_compiler::ast::NamedType;
use apollo_compiler::executable::Field;
use apollo_compiler::executable::SelectionSet;
use apollo_compiler::validation::Valid;
use apollo_compiler::Parser;
use apollo_compiler::Schema;
use tower::BoxError;

use super::DemandControlError;

pub(in crate::plugins::demand_control) struct IncludeDirective {
    pub(in crate::plugins::demand_control) is_included: bool,
}

impl IncludeDirective {
    pub(in crate::plugins::demand_control) fn from_field(
        field: &Field,
    ) -> Result<Option<Self>, BoxError> {
        let directive = field
            .directives
            .get("include")
            .and_then(|skip| skip.argument_by_name("if"))
            .and_then(|arg| arg.to_bool())
            .map(|cond| Self { is_included: cond });

        Ok(directive)
    }
}

pub(in crate::plugins::demand_control) struct RequiresDirective {
    pub(in crate::plugins::demand_control) fields: SelectionSet,
}

impl RequiresDirective {
    pub(in crate::plugins::demand_control) fn from_field(
        field: &Field,
        parent_type_name: &NamedType,
        schema: &Valid<Schema>,
    ) -> Result<Option<Self>, DemandControlError> {
        // When a user marks a subgraph schema field with `@requires`, the composition process
        // replaces `@requires(field: "<selection>")` with `@join__field(requires: "<selection>")`.
        //
        // Note we cannot use `field.definition` in this case: The operation executes against the
        // API schema, so its definition pointers point into the API schema. To find the
        // `@join__field()` directive, we must instead look up the field on the type with the same
        // name in the supergraph.
        let definition = schema
            .type_field(parent_type_name, &field.name)
            .map_err(|_err| {
                DemandControlError::QueryParseFailure(format!(
                    "Could not find the API schema type {}.{} in the supergraph. This looks like a bug",
                    parent_type_name, &field.name
                ))
            })?;
        let requires_arg = definition
            .directives
            .get("join__field")
            .and_then(|requires| requires.argument_by_name("requires"))
            .and_then(|arg| arg.as_str());

        if let Some(arg) = requires_arg {
            let field_set =
                Parser::new().parse_field_set(schema, parent_type_name.clone(), arg, "")?;

            Ok(Some(RequiresDirective {
                fields: field_set.selection_set.clone(),
            }))
        } else {
            Ok(None)
        }
    }
}

pub(in crate::plugins::demand_control) struct SkipDirective {
    pub(in crate::plugins::demand_control) is_skipped: bool,
}

impl SkipDirective {
    pub(in crate::plugins::demand_control) fn from_field(
        field: &Field,
    ) -> Result<Option<Self>, BoxError> {
        let directive = field
            .directives
            .get("skip")
            .and_then(|skip| skip.argument_by_name("if"))
            .and_then(|arg| arg.to_bool())
            .map(|cond| Self { is_skipped: cond });

        Ok(directive)
    }
}
