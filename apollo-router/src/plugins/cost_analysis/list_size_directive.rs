use anyhow::anyhow;
use apollo_compiler::ast;
use tower::BoxError;

pub(crate) struct ListSizeDirective {
    max_size: f64,
}

impl ListSizeDirective {
    pub(crate) fn from_field(
        field_def: &ast::FieldDefinition,
        field: &ast::Field,
    ) -> Result<ListSizeDirective, BoxError> {
        let list_size_directive = field_def
            .directives
            .get("listSize")
            .ok_or(anyhow!("Expected listSize directive"))?;

        // First, we check if there is an explicit assumed size
        let assumed_size = list_size_directive
            .argument_by_name("assumedSize")
            .and_then(|arg| arg.as_ref().to_f64());

        if let Some(assumed_size) = assumed_size {
            return Ok(Self {
                max_size: assumed_size,
            });
        }

        // Second, we check if the directive has specified slicing arguments. These arguments
        // limit the size of the returned list. Of those present, we take the maximum.
        let slicing_arguments = list_size_directive
            .argument_by_name("slicingArguments")
            .and_then(|arg| arg.as_list())
            .map(|args| args.iter().flat_map(|arg| arg.as_str()).collect::<Vec<_>>());

        if let Some(slicing_arguments) = slicing_arguments {
            let mut max_size: Option<f64> = None;

            let values = field
                .arguments
                .iter()
                .filter(|arg| slicing_arguments.contains(&arg.name.as_str()))
                .map(|arg| arg.value.to_f64());

            for value in values {
                max_size = match (value, max_size) {
                    (Some(v), Some(max)) => Some(max.max(v)),
                    (Some(v), None) => Some(v),
                    (None, existing) => existing,
                }
            }

            if let Some(v) = max_size {
                return Ok(Self { max_size: v });
            } else {
                return Err(anyhow!(
                    "Found slicingArguments, but no passed argument had valid f64"
                )
                .into());
            }
        }

        // TODO(tninesling): The spec also defines a sizedFields argument, which
        // direct the limiting effect to a list field within the returned object.
        // That implementation starts here, but needs to be tracked during the
        // traversal as well.

        Err(anyhow!("Expected assumedSize or slicingArguments").into())
    }

    pub(crate) fn max_list_size(&self) -> f64 {
        self.max_size
    }
}
