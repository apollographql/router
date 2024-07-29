use std::collections::HashMap;
use std::collections::HashSet;

use apollo_compiler::ast::Argument;
use apollo_compiler::ast::Directive;
use apollo_compiler::ast::DirectiveList;
use apollo_compiler::ast::FieldDefinition;
use apollo_compiler::ast::InputValueDefinition;
use apollo_compiler::ast::NamedType;
use apollo_compiler::executable::Field;
use apollo_compiler::executable::SelectionSet;
use apollo_compiler::parser::Parser;
use apollo_compiler::schema::ExtendedType;
use apollo_compiler::validation::Valid;
use apollo_compiler::Node;
use apollo_compiler::Schema;
use tower::BoxError;

use super::DemandControlError;

pub(in crate::plugins::demand_control) struct CostDirective {
    pub(in crate::plugins::demand_control) weight: i32,
}

impl CostDirective {
    pub(in crate::plugins::demand_control) fn weight(&self) -> f64 {
        self.weight as f64
    }

    pub(in crate::plugins::demand_control) fn from_argument(
        argument: &InputValueDefinition,
    ) -> Option<Self> {
        Self::from_directives(&argument.directives)
    }

    pub(in crate::plugins::demand_control) fn from_field(field: &FieldDefinition) -> Option<Self> {
        Self::from_directives(&field.directives)
    }

    pub(in crate::plugins::demand_control) fn from_type(ty: &ExtendedType) -> Option<Self> {
        Self::from_schema_directives(ty.directives())
    }

    fn from_directives(directives: &DirectiveList) -> Option<Self> {
        directives
            .get("cost")
            .and_then(|cost| cost.argument_by_name("weight"))
            .and_then(|weight| weight.to_i32())
            .map(|weight| Self { weight })
    }

    pub(in crate::plugins::demand_control) fn from_schema_directives(
        directives: &apollo_compiler::schema::DirectiveList,
    ) -> Option<Self> {
        directives
            .get("cost")
            .and_then(|cost| cost.argument_by_name("weight"))
            .and_then(|weight| weight.to_i32())
            .map(|weight| Self { weight })
    }
}

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

pub(in crate::plugins::demand_control) struct ListSizeDirective<'schema> {
    pub(in crate::plugins::demand_control) assumed_size: Option<usize>,
    pub(in crate::plugins::demand_control) slicing_arguments: Option<HashSet<&'schema str>>,
    pub(in crate::plugins::demand_control) sized_fields: Option<HashSet<&'schema str>>,
    pub(in crate::plugins::demand_control) require_one_slicing_argument: bool,
}

impl<'schema> ListSizeDirective<'schema> {
    pub(in crate::plugins::demand_control) fn from_field(
        field: &'schema FieldDefinition,
    ) -> Result<Option<Self>, DemandControlError> {
        let directive = field.directives.get("listSize");

        match directive {
            Some(dir) => Ok(Some(Self::from_directive(dir)?)),
            None => Ok(None),
        }
    }

    fn from_directive(directive: &'schema Directive) -> Result<Self, DemandControlError> {
        let assumed_size = directive
            .argument_by_name("assumedSize")
            .and_then(|arg| arg.to_i32())
            .map(|i| i as usize); // TODO: Validate this
        let slicing_arguments = directive
            .argument_by_name("slicingArguments")
            .and_then(|arg| arg.as_list())
            .map(|arg_list| arg_list.iter().flat_map(|arg| arg.as_str()).collect());
        let sized_fields = directive
            .argument_by_name("sizedFields")
            .and_then(|arg| arg.as_list())
            .map(|arg_list| arg_list.iter().flat_map(|arg| arg.as_str()).collect());
        let require_one_slicing_argument = directive
            .argument_by_name("requireOneSlicingArgument")
            .and_then(|arg| arg.to_bool())
            .unwrap_or(true);

        // TODO: Validation for argument combinations

        Ok(Self {
            assumed_size,
            slicing_arguments,
            sized_fields,
            require_one_slicing_argument,
        })
    }

    // TODO: Store a reference in from_field instead of passing field twice
    pub(in crate::plugins::demand_control) fn expected_size(
        &self,
        field: &Field,
    ) -> Result<f64, DemandControlError> {
        if let Some(assumed_size) = self.assumed_size {
            return Ok(assumed_size as f64);
        }

        if let Some(slicing_arguments) = &self.slicing_arguments {
            let used_slicing_arguments: Vec<&Node<Argument>> = field
                .arguments
                .iter()
                .filter(|arg| slicing_arguments.contains(arg.name.as_str()))
                .collect();

            if self.require_one_slicing_argument && used_slicing_arguments.len() != 1 {
                // TODO: Different error variant?
                return Err(DemandControlError::QueryParseFailure(format!(
                    "Exactly one slicing argument is required, but found {}",
                    used_slicing_arguments.len()
                )));
            }

            let mut size_from_slicing_arguments: f64 = 0.0;
            for arg in used_slicing_arguments.iter() {
                if let Some(v) = arg.value.to_f64() {
                    size_from_slicing_arguments = size_from_slicing_arguments.max(v);
                }
            }
            return Ok(size_from_slicing_arguments);
        }

        todo!("Probably an error here?")
    }

    pub(in crate::plugins::demand_control) fn sized_fields(
        &self,
        field: &Field,
    ) -> Result<HashMap<&str, f64>, DemandControlError> {
        let size = self.expected_size(field)?;
        let sized_fields_with_sizes = if let Some(sized_field_set) = &self.sized_fields {
            sized_field_set.iter().map(|f| (*f, size)).collect()
        } else {
            Default::default()
        };
        Ok(sized_fields_with_sizes)
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
