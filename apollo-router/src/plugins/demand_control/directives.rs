use anyhow::anyhow;
use apollo_compiler::executable::Field;
use apollo_compiler::executable::SelectionSet;
use apollo_compiler::validation::Valid;
use apollo_compiler::ExecutableDocument;
use apollo_compiler::Schema;
use tower::BoxError;

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
        schema: &Valid<Schema>,
    ) -> Result<Option<Self>, BoxError> {
        // TODO(tninesling): This assumes the happy path of consumers using federation directives as-is.
        // However, unlike the built-ins, end users can rename these directives when they import the
        // federation spec via `@link`. We'll need to follow-up with some solution that accounts for this
        // potential renaming.
        let required_selection = field
            .definition
            .directives
            .get("requires")
            .and_then(|requires| requires.argument_by_name("fields"))
            .and_then(|arg| arg.as_str())
            .map(|selection_without_braces| format!("{{ {} }}", selection_without_braces))
            .map(|selection_str| {
                RequiresDirective::parse_top_level_selection_set(selection_str, schema)
            });

        match required_selection {
            Some(Ok(Some(selection))) => Ok(Some(RequiresDirective { fields: selection })),
            Some(Err(e)) => Err(e),
            None | Some(Ok(None)) => Ok(None),
        }
    }

    fn parse_top_level_selection_set(
        str: String,
        schema: &Valid<Schema>,
    ) -> Result<Option<SelectionSet>, BoxError> {
        let doc = ExecutableDocument::parse(schema, str, "").map_err(|e| anyhow!(e))?;

        Ok(doc.anonymous_operation.map(|op| op.selection_set.clone()))
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
