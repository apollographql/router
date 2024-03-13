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
        let weight = directives
            .get("cost")
            .and_then(|cost| cost.argument_by_name("weight"))
            .map(|arg| arg.as_ref());

        match weight {
            Some(executable::Value::Float(f)) => f
                .try_to_f64()
                .map(|weight| Some(CostDirective::new(weight)))
                .map_err(|_| anyhow!("Argument weight cannot be parsed as a valid f64.").into()),
            Some(executable::Value::Int(i)) => i
                .try_to_f64()
                .map(|weight| Some(CostDirective::new(weight)))
                .map_err(|_| anyhow!("Argument weight cannot be parsed as a valid f64.").into()),
            Some(executable::Value::String(s)) => {
                // This is the expected branch, since the spec defines weight as a String.
                // The spec mentions the String could be either a serialized float, as
                // parsed here, or an expression that evaluates to a float (which is omitted
                // for now).
                s.parse()
                    .map(|weight| Some(CostDirective::new(weight)))
                    .map_err(|_| anyhow!("Argument weight cannot be parsed as a valid f64.").into())
            }
            Some(_) => Err(anyhow!("Argument weight must be a valid float").into()),
            None => Ok(None),
        }
    }

    pub(crate) fn weight(&self) -> f64 {
        self.weight
    }
}
