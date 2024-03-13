use anyhow::anyhow;
use apollo_compiler::ast;
use apollo_compiler::executable;
use tower::BoxError;

pub(crate) enum ListSizeDirective {
    AssumedSize(f64),
    ArgumentLimited(Vec<String>),
}

impl ListSizeDirective {
    pub(crate) fn from_directives(
        directives: &ast::DirectiveList,
    ) -> Result<ListSizeDirective, BoxError> {
        let list_size_directive = directives
            .get("listSize")
            .ok_or(anyhow!("Expected listSize directive"))?;

        let assumed_size = list_size_directive
            .argument_by_name("assumedSize")
            .map(|arg| arg.as_ref());
        let slicing_arguments = list_size_directive
            .argument_by_name("slicingArguments")
            .map(|arg| arg.as_ref());
        let require_one_slicing_argument = list_size_directive
            .argument_by_name("requireOneSlicingArgument")
            .map(|arg| arg.as_ref());

        match (
            assumed_size,
            require_one_slicing_argument,
            slicing_arguments,
        ) {
            // Assumed size
            (Some(executable::Value::Float(f)), _, _) => {
                let size = f
                    .try_to_f64()
                    .map_err(|_| anyhow!("Expected valid float value"))?;
                Ok(ListSizeDirective::AssumedSize(size))
            }
            (Some(executable::Value::Int(i)), _, _) => {
                let size = i
                    .try_to_f64()
                    .map_err(|_| anyhow!("Expected valid float value"))?;
                Ok(ListSizeDirective::AssumedSize(size))
            }
            (Some(_), _, _) => {
                Err(anyhow!("Expected numeric value for assumed size argument").into())
            }
            // Slicing arguments
            (None, Some(executable::Value::Boolean(true)), None) => Err(anyhow!(
                "slicingArguments is required by requireOneSlicingArgument: true but not passed"
            )
            .into()),
            (None, _, Some(executable::Value::List(slicing_args))) => {
                let slicing_args = slicing_args
                    .iter()
                    .flat_map(|arg| arg.as_str())
                    .map(str::to_owned)
                    .collect();
                Ok(ListSizeDirective::ArgumentLimited(slicing_args))
            }
            (None, None, None) => {
                Err(anyhow!("One of assumedSize or slicingArguments is required").into())
            }
            (None, _, _) => Err(anyhow!(
                "Invalid argument format. slicingArguments must be of type [String]"
            )
            .into()),
        }
    }

    pub(crate) fn max_list_size(&self) -> f64 {
        match self {
            ListSizeDirective::AssumedSize(size) => *size,
            _ => todo!(),
        }
    }
}
