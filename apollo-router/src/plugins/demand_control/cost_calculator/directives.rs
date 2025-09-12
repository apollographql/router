use ahash::HashMap;
use ahash::HashMapExt;
use ahash::HashSet;
use apollo_compiler::Schema;
use apollo_compiler::ast::FieldDefinition;
use apollo_compiler::ast::NamedType;
use apollo_compiler::executable::Field;
use apollo_compiler::executable::SelectionSet;
use apollo_compiler::parser::Parser;
use apollo_compiler::validation::Valid;
use apollo_federation::link::cost_spec_definition::ListSizeDirective as ParsedListSizeDirective;
use tower::BoxError;

use crate::json_ext::Object;
use crate::json_ext::ValueExt;
use crate::plugins::demand_control::DemandControlError;

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
            .and_then(|skip| skip.specified_argument_by_name("if"))
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
    pub(in crate::plugins::demand_control) fn new(
        parsed: &'schema ParsedListSizeDirective,
        field: &Field,
        variables: &Object,
    ) -> Result<Self, DemandControlError> {
        let mut slicing_arguments: HashMap<&str, i32> = HashMap::new();
        if let Some(slicing_argument_names) = parsed.slicing_argument_names.as_ref() {
            // First, collect the default values for each argument
            for argument in &field.definition.arguments {
                if slicing_argument_names.contains(argument.name.as_str())
                    && let Some(numeric_value) =
                        argument.default_value.as_ref().and_then(|v| v.to_i32())
                {
                    slicing_arguments.insert(&argument.name, numeric_value);
                }
            }
            // Then, overwrite any default values with the actual values passed in the query
            for argument in &field.arguments {
                if slicing_argument_names.contains(argument.name.as_str()) {
                    if let Some(numeric_value) = argument.value.to_i32() {
                        slicing_arguments.insert(&argument.name, numeric_value);
                    } else if let Some(numeric_value) = argument
                        .value
                        .as_variable()
                        .and_then(|variable_name| variables.get(variable_name.as_str()))
                        .and_then(|variable| variable.as_i32())
                    {
                        slicing_arguments.insert(&argument.name, numeric_value);
                    }
                }
            }

            if parsed.require_one_slicing_argument && slicing_arguments.len() != 1 {
                return Err(DemandControlError::QueryParseFailure(format!(
                    "Exactly one slicing argument is required, but found {}",
                    slicing_arguments.len()
                )));
            }
        }

        let expected_size = slicing_arguments
            .values()
            .max()
            .cloned()
            .or(parsed.assumed_size);

        Ok(Self {
            expected_size,
            sized_fields: parsed
                .sized_fields
                .as_ref()
                .map(|set| set.iter().map(|s| s.as_str()).collect()),
        })
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
    pub(in crate::plugins::demand_control) fn from_field_definition(
        definition: &FieldDefinition,
        parent_type_name: &NamedType,
        schema: &Valid<Schema>,
    ) -> Result<Option<Self>, DemandControlError> {
        let requires_arg = definition
            .directives
            .get("join__field")
            .and_then(|requires| requires.specified_argument_by_name("requires"))
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
            .and_then(|skip| skip.specified_argument_by_name("if"))
            .and_then(|arg| arg.to_bool())
            .map(|cond| Self { is_skipped: cond });

        Ok(directive)
    }
}
