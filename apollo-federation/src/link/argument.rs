use std::ops::Deref;

use apollo_compiler::ast::Value;
use apollo_compiler::schema::Directive;
use apollo_compiler::schema::Name;
use apollo_compiler::Node;
use apollo_compiler::NodeStr;

use crate::error::FederationError;
use crate::error::SingleFederationError;
use crate::link::graphql_definition::BooleanOrVariable;

pub(crate) fn directive_optional_enum_argument(
    application: &Node<Directive>,
    name: &Name,
) -> Result<Option<Name>, FederationError> {
    match application.argument_by_name(name) {
        Some(value) => match value.deref() {
            Value::Enum(name) => Ok(Some(name.clone())),
            Value::Null => Ok(None),
            _ => Err(SingleFederationError::Internal {
                message: format!(
                    "Argument \"{}\" of directive \"@{}\" must be an enum value.",
                    name, application.name
                ),
            }
            .into()),
        },
        None => Ok(None),
    }
}

pub(crate) fn directive_required_enum_argument(
    application: &Node<Directive>,
    name: &Name,
) -> Result<Name, FederationError> {
    directive_optional_enum_argument(application, name)?.ok_or_else(|| {
        SingleFederationError::Internal {
            message: format!(
                "Required argument \"{}\" of directive \"@{}\" was not present.",
                name, application.name
            ),
        }
        .into()
    })
}

pub(crate) fn directive_optional_string_argument(
    application: &Node<Directive>,
    name: &Name,
) -> Result<Option<NodeStr>, FederationError> {
    match application.argument_by_name(name) {
        Some(value) => match value.deref() {
            Value::String(name) => Ok(Some(name.clone())),
            Value::Null => Ok(None),
            _ => Err(SingleFederationError::Internal {
                message: format!(
                    "Argument \"{}\" of directive \"@{}\" must be a string.",
                    name, application.name
                ),
            }
            .into()),
        },
        None => Ok(None),
    }
}

pub(crate) fn directive_required_string_argument(
    application: &Node<Directive>,
    name: &Name,
) -> Result<NodeStr, FederationError> {
    directive_optional_string_argument(application, name)?.ok_or_else(|| {
        SingleFederationError::Internal {
            message: format!(
                "Required argument \"{}\" of directive \"@{}\" was not present.",
                name, application.name
            ),
        }
        .into()
    })
}

pub(crate) fn directive_optional_fieldset_argument(
    application: &Node<Directive>,
    name: &Name,
) -> Result<Option<NodeStr>, FederationError> {
    match application.argument_by_name(name) {
        Some(value) => match value.deref() {
            Value::String(name) => Ok(Some(name.clone())),
            Value::Null => Ok(None),
            _ => Err(SingleFederationError::Internal {
                message: format!("Invalid value for argument \"{}\": must be a string.", name),
            }
            .into()),
        },
        None => Ok(None),
    }
}

pub(crate) fn directive_required_fieldset_argument(
    application: &Node<Directive>,
    name: &Name,
) -> Result<NodeStr, FederationError> {
    directive_optional_fieldset_argument(application, name)?.ok_or_else(|| {
        SingleFederationError::Internal {
            message: format!(
                "Required argument \"{}\" of directive \"@{}\" was not present.",
                name, application.name
            ),
        }
        .into()
    })
}

pub(crate) fn directive_optional_boolean_argument(
    application: &Node<Directive>,
    name: &Name,
) -> Result<Option<bool>, FederationError> {
    match application.argument_by_name(name) {
        Some(value) => match value.deref() {
            Value::Boolean(value) => Ok(Some(*value)),
            Value::Null => Ok(None),
            _ => Err(SingleFederationError::Internal {
                message: format!(
                    "Argument \"{}\" of directive \"@{}\" must be a boolean.",
                    name, application.name
                ),
            }
            .into()),
        },
        None => Ok(None),
    }
}

#[allow(dead_code)]
pub(crate) fn directive_required_boolean_argument(
    application: &Node<Directive>,
    name: &Name,
) -> Result<bool, FederationError> {
    directive_optional_boolean_argument(application, name)?.ok_or_else(|| {
        SingleFederationError::Internal {
            message: format!(
                "Required argument \"{}\" of directive \"@{}\" was not present.",
                name, application.name
            ),
        }
        .into()
    })
}

pub(crate) fn directive_optional_variable_boolean_argument(
    application: &Node<Directive>,
    name: &Name,
) -> Result<Option<BooleanOrVariable>, FederationError> {
    match application.argument_by_name(name) {
        Some(value) => match value.deref() {
            Value::Variable(name) => Ok(Some(BooleanOrVariable::Variable(name.clone()))),
            Value::Boolean(value) => Ok(Some(BooleanOrVariable::Boolean(*value))),
            Value::Null => Ok(None),
            _ => Err(FederationError::internal(format!(
                "Argument \"{}\" of directive \"@{}\" must be a boolean.",
                name, application.name
            ))),
        },
        None => Ok(None),
    }
}
