use anyhow::anyhow;
use apollo_compiler::executable::Field;
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
