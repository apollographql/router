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
        if let Some(directive) = field.directives.get("include") {
            if let Some(condition) = directive.argument_by_name("if") {
                Ok(Some(Self {
                    is_included: condition.to_bool().unwrap_or(true),
                }))
            } else {
                Err(anyhow!("Found @include directive with no if argument").into())
            }
        } else {
            Ok(None)
        }
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
        if let Some(directive) = field.definition.directives.get("requires") {
            if let Some(fields_str) = directive.argument_by_name("fields") {
                let doc = ExecutableDocument::parse(
                    schema,
                    format!("{{ {} }}", fields_str.as_str().unwrap_or("")),
                    "",
                )
                .map_err(|e| anyhow!("{}", e))?;

                Ok(Some(RequiresDirective {
                    fields: doc.anonymous_operation.unwrap().selection_set.clone(),
                }))
            } else {
                Err(anyhow!("Found @requires directive with no fields argument").into())
            }
        } else {
            Ok(None)
        }
    }
}

pub(super) struct SkipDirective {
    pub(super) is_skipped: bool,
}

impl SkipDirective {
    pub(super) fn from_field(field: &Field) -> Result<Option<Self>, BoxError> {
        if let Some(directive) = field.directives.get("skip") {
            if let Some(condition) = directive.argument_by_name("if") {
                Ok(Some(Self {
                    is_skipped: condition.to_bool().unwrap_or(false),
                }))
            } else {
                Err(anyhow!("Found @skip directive with no if argument").into())
            }
        } else {
            Ok(None)
        }
    }
}
