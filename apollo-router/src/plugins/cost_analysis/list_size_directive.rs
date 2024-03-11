use anyhow::anyhow;
use apollo_compiler::ast;
use tower::BoxError;

type DirectiveArgument<'a> = Option<&'a apollo_compiler::Node<apollo_compiler::ast::Value>>;

pub(crate) struct ListSizeDirective {
    assumed_size: Option<f64>,
    slicing_arguments: Vec<String>,
    sized_fields: Vec<String>,
    require_one_slicing_argument: bool,
}

impl ListSizeDirective {
    pub(crate) fn from_field(field: &ast::FieldDefinition) -> Result<Self, BoxError> {
        let list_size_directive = field.directives.get("listSize")
            .ok_or(anyhow!("Expected listSize directive"))?;
        let assumed_size = ListSizeDirective::parse_arg(list_size_directive.argument_by_name("assumedSize"));
        let slicing_arguments = ListSizeDirective::parse_args(list_size_directive.argument_by_name("slicingArguments"));
        let sized_fields = ListSizeDirective::parse_args( list_size_directive.argument_by_name("sizedFields"));
        let require_one_slicing_argument = ListSizeDirective::parse_arg(list_size_directive.argument_by_name("requireOneSlicingArgument"))
            .unwrap_or(false);

        println!("Assumed size: {:?}\nSlicing arguments: {:?}\nSizedFields: {:?}\nRequire slicing arguments?: {}", assumed_size, slicing_arguments, sized_fields, require_one_slicing_argument);

        Ok(Self {
            assumed_size,
            slicing_arguments,
            sized_fields,
            require_one_slicing_argument,
        })
    }

    fn parse_arg<T: std::str::FromStr>(arg: DirectiveArgument) -> Option<T> {
        println!("Arg: {:?}", arg);
        arg.and_then(|arg| arg.as_str())
            .and_then(|arg| {
                println!("{}", arg);
                arg.parse().ok()
            })
    }

    fn parse_args(arg: DirectiveArgument) -> Vec<String> {
        arg.and_then(|arg| arg.as_list())
            .map(|args| args.iter()
                .flat_map(|arg| arg.as_str())
                .map(|str| str.to_string())
                .collect::<Vec<String>>()
            )
            .unwrap_or_default()
    }

    pub(crate) fn max_list_size(&self) -> Result<f64, BoxError> {
        if let Some(assumed_size) = self.assumed_size {
            return Ok(assumed_size);
        }
        if self.require_one_slicing_argument && self.slicing_arguments.len() < 1 {
            return Err(anyhow!("Slicing arguments are required, but none were provided").into());
        }
        // TODO: Handle slicing args
        // TODO: Handle sized fields

        Err(anyhow!("Invalid argument configuration for listSize").into())
    }
}
