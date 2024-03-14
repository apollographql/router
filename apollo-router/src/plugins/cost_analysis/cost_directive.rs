use anyhow::anyhow;
use apollo_compiler::ast;
use apollo_compiler::executable;
use tower::BoxError;

pub(crate) struct CostDirective {
    weight: f64,
}

impl CostDirective {
    pub(crate) fn new(weight: f64) -> Self {
        Self { weight }
    }

    pub(crate) fn from_directives(
        directives: &ast::DirectiveList,
    ) -> Result<Option<Self>, BoxError> {
        if let Some(cost_directive) = directives.get("cost") {
            let weight = cost_directive
                .argument_by_name("weight")
                .and_then(|arg| arg.as_str());

            if let Some(w) = weight {
                Ok(Some(CostDirective::new(w.parse::<f64>()?)))
            } else {
                Err(anyhow!("Cost directive is missing required parameter: weight.").into())
            }
        } else {
            Ok(None)
        }
    }

    pub(crate) fn weight(&self) -> f64 {
        self.weight
    }
}
