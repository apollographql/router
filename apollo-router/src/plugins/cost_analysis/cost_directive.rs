use anyhow::anyhow;
use apollo_compiler::ast;
use tower::BoxError;

pub(crate) struct CostDirective {
    weight: f64,
}

impl CostDirective {
    pub(crate) fn from_field(field: &ast::FieldDefinition) -> Result<Option<Self>, BoxError> {
        if let Some(directive) = field.directives.get("cost") {
            let weight = directive.argument_by_name("weight")
                .and_then(|weight| weight.to_f64())
                .ok_or(anyhow!("Expected @cost directive to have valid float value for weight argument"))?;

            Ok(Some(Self { weight }))
        } else {
            Ok(None)
        }
    }

    pub(crate) fn weight(&self) -> f64 {
        self.weight
    }
}