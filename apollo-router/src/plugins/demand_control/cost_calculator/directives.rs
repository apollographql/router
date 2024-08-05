use std::collections::HashSet;

use apollo_compiler::ast::Argument;
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
        // TODO: Make sure to handle renaming
        directives
            .get("cost")
            .or(directives.get("federation__cost"))
            .and_then(|cost| cost.argument_by_name("weight"))
            .and_then(|weight| weight.to_i32())
            .map(|weight| Self { weight })
    }

    pub(in crate::plugins::demand_control) fn from_schema_directives(
        directives: &apollo_compiler::schema::DirectiveList,
    ) -> Option<Self> {
        directives
            .get("cost")
            .or(directives.get("federation__cost"))
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
    pub(in crate::plugins::demand_control) expected_size: Option<i32>,
    pub(in crate::plugins::demand_control) sized_fields: Option<HashSet<&'schema str>>,
}

impl<'schema> ListSizeDirective<'schema> {
    pub(in crate::plugins::demand_control) fn from_field(
        field: &'schema Field,
        definition: &'schema FieldDefinition,
    ) -> Result<Option<Self>, DemandControlError> {
        let directive = definition
            .directives
            .get("listSize")
            .or(definition.directives.get("federation__listSize"));

        if let Some(directive) = directive {
            let assumed_size = directive
                .argument_by_name("assumedSize")
                .and_then(|arg| arg.to_i32());
            let slicing_arguments: Option<HashSet<&str>> = directive
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

            if let Some(slicing_arguments) = slicing_arguments.as_ref() {
                let used_slicing_arguments: Vec<&Node<Argument>> = field
                    .arguments
                    .iter()
                    .filter(|arg| slicing_arguments.contains(arg.name.as_str()))
                    .collect();
                if require_one_slicing_argument && used_slicing_arguments.len() != 1 {
                    // TODO: Different error variant?
                    return Err(DemandControlError::QueryParseFailure(format!(
                        "Exactly one slicing argument is required, but found {}",
                        used_slicing_arguments.len()
                    )));
                }
            }
            let expected_size = assumed_size.or(Self::size_from_slicing_arguments(
                field,
                slicing_arguments.as_ref(),
            ));

            Ok(Some(Self {
                expected_size,
                sized_fields,
            }))
        } else {
            Ok(None)
        }
    }

    fn size_from_slicing_arguments(
        field: &Field,
        slicing_arguments: Option<&HashSet<&str>>,
    ) -> Option<i32> {
        if let Some(slicing_arguments) = slicing_arguments {
            let mut size_from_slicing_arguments = 0;
            for arg in field
                .arguments
                .iter()
                .filter(|arg| slicing_arguments.contains(arg.name.as_str()))
            {
                if let Some(v) = arg.value.to_i32() {
                    size_from_slicing_arguments = size_from_slicing_arguments.max(v);
                }
            }
            Some(size_from_slicing_arguments)
        } else {
            None
        }
    }

    pub(in crate::plugins::demand_control) fn size_of(&self, field: &Field) -> Option<i32> {
        if self
            .sized_fields
            .as_ref()
            .is_some_and(|sf| sf.contains(field.name.as_str()))
        {
            self.expected_size
        } else {
            None
        }
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
