use std::collections::HashMap;
use std::collections::HashSet;

use apollo_compiler::ast::DirectiveList;
use apollo_compiler::ast::FieldDefinition;
use apollo_compiler::ast::InputValueDefinition;
use apollo_compiler::ast::NamedType;
use apollo_compiler::executable::Field;
use apollo_compiler::executable::SelectionSet;
use apollo_compiler::name;
use apollo_compiler::parser::Parser;
use apollo_compiler::schema::ExtendedType;
use apollo_compiler::validation::Valid;
use apollo_compiler::Name;
use apollo_compiler::Schema;
use apollo_federation::link::spec::APOLLO_SPEC_DOMAIN;
use apollo_federation::link::Link;
use tower::BoxError;

use super::DemandControlError;

const COST_DIRECTIVE_NAME: Name = name!("cost");
const COST_DIRECTIVE_DEFAULT_NAME: Name = name!("federation__cost");
const COST_DIRECTIVE_WEIGHT_ARGUMENT_NAME: Name = name!("weight");

const LIST_SIZE_DIRECTIVE_NAME: Name = name!("listSize");
const LIST_SIZE_DIRECTIVE_DEFAULT_NAME: Name = name!("federation__listSize");
const LIST_SIZE_DIRECTIVE_ASSUMED_SIZE_ARGUMENT_NAME: Name = name!("assumedSize");
const LIST_SIZE_DIRECTIVE_SLICING_ARGUMENTS_ARGUMENT_NAME: Name = name!("slicingArguments");
const LIST_SIZE_DIRECTIVE_SIZED_FIELDS_ARGUMENT_NAME: Name = name!("sizedFields");
const LIST_SIZE_DIRECTIVE_REQUIRE_ONE_SLICING_ARGUMENT_ARGUMENT_NAME: Name =
    name!("requireOneSlicingArgument");

pub(in crate::plugins::demand_control) fn get_apollo_directive_names(
    schema: &Schema,
) -> HashMap<Name, Name> {
    let mut hm: HashMap<Name, Name> = HashMap::new();
    for directive in &schema.schema_definition.directives {
        if directive.name.as_str() == "link" {
            if let Ok(link) = Link::from_directive_application(directive) {
                if link.url.identity.domain != APOLLO_SPEC_DOMAIN {
                    continue;
                }
                for import in link.imports {
                    hm.insert(import.element.clone(), import.imported_name().clone());
                }
            }
        }
    }
    hm
}

pub(in crate::plugins::demand_control) struct CostDirective {
    weight: i32,
}

impl CostDirective {
    pub(in crate::plugins::demand_control) fn weight(&self) -> f64 {
        self.weight as f64
    }

    pub(in crate::plugins::demand_control) fn from_argument(
        directive_name_map: &HashMap<Name, Name>,
        argument: &InputValueDefinition,
    ) -> Option<Self> {
        Self::from_directives(directive_name_map, &argument.directives)
    }

    pub(in crate::plugins::demand_control) fn from_field(
        directive_name_map: &HashMap<Name, Name>,
        field: &FieldDefinition,
    ) -> Option<Self> {
        Self::from_directives(directive_name_map, &field.directives)
    }

    pub(in crate::plugins::demand_control) fn from_type(
        directive_name_map: &HashMap<Name, Name>,
        ty: &ExtendedType,
    ) -> Option<Self> {
        Self::from_schema_directives(directive_name_map, ty.directives())
    }

    fn from_directives(
        directive_name_map: &HashMap<Name, Name>,
        directives: &DirectiveList,
    ) -> Option<Self> {
        directive_name_map
            .get(&COST_DIRECTIVE_NAME)
            .and_then(|name| directives.get(name))
            .or(directives.get(&COST_DIRECTIVE_DEFAULT_NAME))
            .and_then(|cost| cost.argument_by_name(&COST_DIRECTIVE_WEIGHT_ARGUMENT_NAME))
            .and_then(|weight| weight.to_i32())
            .map(|weight| Self { weight })
    }

    pub(in crate::plugins::demand_control) fn from_schema_directives(
        directive_name_map: &HashMap<Name, Name>,
        directives: &apollo_compiler::schema::DirectiveList,
    ) -> Option<Self> {
        directive_name_map
            .get(&COST_DIRECTIVE_NAME)
            .and_then(|name| directives.get(name))
            .or(directives.get(&COST_DIRECTIVE_DEFAULT_NAME))
            .and_then(|cost| cost.argument_by_name(&COST_DIRECTIVE_WEIGHT_ARGUMENT_NAME))
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
        directive_name_map: &HashMap<Name, Name>,
        field: &'schema Field,
        definition: &'schema FieldDefinition,
    ) -> Result<Option<Self>, DemandControlError> {
        let directive = directive_name_map
            .get(&LIST_SIZE_DIRECTIVE_NAME)
            .and_then(|name| definition.directives.get(name))
            .or(definition.directives.get(&LIST_SIZE_DIRECTIVE_DEFAULT_NAME));
        if let Some(directive) = directive {
            let assumed_size = directive
                .argument_by_name(&LIST_SIZE_DIRECTIVE_ASSUMED_SIZE_ARGUMENT_NAME)
                .and_then(|arg| arg.to_i32());
            let slicing_argument_names: Option<HashSet<&str>> = directive
                .argument_by_name(&LIST_SIZE_DIRECTIVE_SLICING_ARGUMENTS_ARGUMENT_NAME)
                .and_then(|arg| arg.as_list())
                .map(|arg_list| arg_list.iter().flat_map(|arg| arg.as_str()).collect());
            let sized_fields = directive
                .argument_by_name(&LIST_SIZE_DIRECTIVE_SIZED_FIELDS_ARGUMENT_NAME)
                .and_then(|arg| arg.as_list())
                .map(|arg_list| arg_list.iter().flat_map(|arg| arg.as_str()).collect());
            let require_one_slicing_argument = directive
                .argument_by_name(&LIST_SIZE_DIRECTIVE_REQUIRE_ONE_SLICING_ARGUMENT_ARGUMENT_NAME)
                .and_then(|arg| arg.to_bool())
                .unwrap_or(true);

            let mut slicing_arguments: HashMap<&str, i32> = HashMap::new();
            if let Some(slicing_argument_names) = slicing_argument_names.as_ref() {
                // First, collect the default values for each argument
                for argument in &definition.arguments {
                    if slicing_argument_names.contains(argument.name.as_str()) {
                        if let Some(numeric_value) =
                            argument.default_value.as_ref().and_then(|v| v.to_i32())
                        {
                            slicing_arguments.insert(&argument.name, numeric_value);
                        }
                    }
                }
                // Then, overwrite any default values with the actual values passed in the query
                for argument in &field.arguments {
                    if slicing_argument_names.contains(argument.name.as_str()) {
                        if let Some(numeric_value) = argument.value.to_i32() {
                            slicing_arguments.insert(&argument.name, numeric_value);
                        }
                    }
                }

                if require_one_slicing_argument && slicing_arguments.len() != 1 {
                    return Err(DemandControlError::QueryParseFailure(format!(
                        "Exactly one slicing argument is required, but found {}",
                        slicing_arguments.len()
                    )));
                }
            }

            let expected_size = slicing_arguments.values().max().cloned().or(assumed_size);

            Ok(Some(Self {
                expected_size,
                sized_fields,
            }))
        } else {
            Ok(None)
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
