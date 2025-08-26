use apollo_compiler::Name;
use apollo_compiler::Node;
use apollo_compiler::ast::Value;
use apollo_compiler::name;

use crate::connectors::ConnectSpec;
use crate::connectors::JSONSelection;
use crate::error::FederationError;

pub(crate) const ERRORS_NAME_IN_SPEC: Name = name!("ConnectorErrors");
pub(crate) const ERRORS_ARGUMENT_NAME: Name = name!("errors");
pub(crate) const ERRORS_MESSAGE_ARGUMENT_NAME: Name = name!("message");
pub(crate) const ERRORS_EXTENSIONS_ARGUMENT_NAME: Name = name!("extensions");

/// Configure the error mapping functionality for a source or connect
#[cfg_attr(test, derive(Debug))]
pub(crate) struct ErrorsArguments {
    /// Configure the mapping for the "message" portion of an error
    pub(crate) message: Option<JSONSelection>,

    /// Configure the mapping for the "extensions" portion of an error
    pub(crate) extensions: Option<JSONSelection>,
}

impl TryFrom<(&[(Name, Node<Value>)], &Name, ConnectSpec)> for ErrorsArguments {
    type Error = FederationError;

    fn try_from(
        (values, directive_name, spec): (&[(Name, Node<Value>)], &Name, ConnectSpec),
    ) -> Result<Self, FederationError> {
        let mut message = None;
        let mut extensions = None;
        for (name, value) in values {
            let name = name.as_str();

            if name == ERRORS_MESSAGE_ARGUMENT_NAME.as_str() {
                let message_value = value.as_str().ok_or_else(|| FederationError::internal(format!(
                    "`message` field in `@{directive_name}` directive's `errors` field is not a string")
                ))?;
                message = Some(
                    JSONSelection::parse_with_spec(message_value, spec)
                        .map_err(|e| FederationError::internal(e.message))?,
                );
            } else if name == ERRORS_EXTENSIONS_ARGUMENT_NAME.as_str() {
                let extensions_value = value.as_str().ok_or_else(|| FederationError::internal(format!(
                    "`extensions` field in `@{directive_name}` directive's `errors` field is not a string")
                ))?;
                extensions = Some(
                    JSONSelection::parse_with_spec(extensions_value, spec)
                        .map_err(|e| FederationError::internal(e.message))?,
                );
            } else {
                return Err(FederationError::internal(format!(
                    "unknown argument in `@{directive_name}` directive's `errors` field: {name}"
                )));
            }
        }

        Ok(Self {
            message,
            extensions,
        })
    }
}
