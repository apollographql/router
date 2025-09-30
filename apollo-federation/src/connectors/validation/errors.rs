//! Parsing and validation for `@connect(errors:)` or `@source(errors:)`

use std::fmt;
use std::fmt::Display;
use std::fmt::Formatter;

use apollo_compiler::Name;
use apollo_compiler::Node;
use apollo_compiler::ast::Value;
use multi_try::MultiTry;
use shape::Shape;

use super::coordinates::ConnectDirectiveCoordinate;
use super::coordinates::IsSuccessCoordinate;
use super::coordinates::SourceDirectiveCoordinate;
use super::expression::MappingArgument;
use super::expression::parse_mapping_argument;
use crate::connectors::JSONSelection;
use crate::connectors::Namespace;
use crate::connectors::spec::connect::IS_SUCCESS_ARGUMENT_NAME;
use crate::connectors::spec::errors::ERRORS_ARGUMENT_NAME;
use crate::connectors::spec::errors::ERRORS_EXTENSIONS_ARGUMENT_NAME;
use crate::connectors::spec::errors::ERRORS_MESSAGE_ARGUMENT_NAME;
use crate::connectors::string_template::Expression;
use crate::connectors::validation::Code;
use crate::connectors::validation::Message;
use crate::connectors::validation::expression;
use crate::connectors::validation::expression::Context;
use crate::connectors::validation::graphql::SchemaInfo;

/// A valid, parsed (but not type-checked) `@connect(errors:)` or `@source(errors:)`.
pub(super) struct Errors<'schema> {
    message: Option<ErrorsMessage<'schema>>,
    extensions: Option<ErrorsExtensions<'schema>>,
}

impl<'schema> Errors<'schema> {
    /// Parse the `@connect(errors:)` or `@source(errors:)` argument and run just enough checks to be able to use the
    /// argument at runtime. More advanced checks are done in [`Self::type_check`].
    ///
    /// Two sub-pieces are always parsed, and the errors from _all_ of those pieces are returned
    /// together in the event of failure:
    /// 1. `errors.message` with [`ErrorsMessage::parse`]
    /// 2. `errors.extensions` with [`ErrorsExtensions::parse`]
    ///
    /// The order these pieces run in doesn't matter and shouldn't affect the output.
    pub(super) fn parse(
        coordinate: ErrorsCoordinate<'schema>,
        schema: &'schema SchemaInfo,
    ) -> Result<Self, Vec<Message>> {
        let directive = match &coordinate {
            ErrorsCoordinate::Source { source } => source.directive,
            ErrorsCoordinate::Connect { connect } => connect.directive,
        };
        let Some(arg) = directive.specified_argument_by_name(&ERRORS_ARGUMENT_NAME) else {
            return Ok(Self {
                message: None,
                extensions: None,
            });
        };

        if let Some(errors_arg) = arg.as_object() {
            ErrorsMessage::parse(errors_arg, coordinate.clone(), schema)
                .and_try(ErrorsExtensions::parse(errors_arg, coordinate, schema))
                .map(|(message, extensions)| Self {
                    message,
                    extensions,
                })
        } else {
            Err(vec![Message {
                code: Code::GraphQLError,
                message: format!(
                    "{coordinate} `{ERRORS_ARGUMENT_NAME}` argument must be an object."
                ),
                locations: arg.line_column_range(&schema.sources).into_iter().collect(),
            }])
        }
    }

    /// Type-check the `@connect(errors:)` or `@source(errors:)` directive.
    ///
    /// Runs [`ErrorsMessage::type_check`] and [`ErrorsExtensions::type_check`]
    ///
    /// TODO: Return some type checking results, like extracted keys?
    pub(super) fn type_check(self, schema: &SchemaInfo) -> Result<(), Vec<Message>> {
        let Self {
            message,
            extensions,
        } = self;

        let mut errors = Vec::new();
        if let Some(message) = message {
            errors.extend(message.type_check(schema).err());
        }

        if let Some(extensions) = extensions {
            errors.extend(extensions.type_check(schema).err());
        }

        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors)
        }
    }

    pub(super) fn variables(&self) -> impl Iterator<Item = Namespace> + '_ {
        self.message
            .as_ref()
            .into_iter()
            .flat_map(|m| {
                m.mapping
                    .variable_references()
                    .map(|var_ref| var_ref.namespace.namespace)
            })
            .chain(self.extensions.as_ref().into_iter().flat_map(|e| {
                e.selection
                    .variable_references()
                    .map(|var_ref| var_ref.namespace.namespace)
            }))
    }
}

struct ErrorsMessage<'schema> {
    mapping: MappingArgument,
    coordinate: ErrorsMessageCoordinate<'schema>,
}

impl<'schema> ErrorsMessage<'schema> {
    pub(super) fn parse(
        errors_arg: &'schema [(Name, Node<Value>)],
        coordinate: ErrorsCoordinate<'schema>,
        schema: &'schema SchemaInfo,
    ) -> Result<Option<Self>, Message> {
        let Some((_, value)) = errors_arg
            .iter()
            .find(|(name, _)| name == &ERRORS_MESSAGE_ARGUMENT_NAME)
        else {
            return Ok(None);
        };
        let coordinate = ErrorsMessageCoordinate { coordinate };

        let mapping = parse_mapping_argument(
            value,
            coordinate.clone(),
            Code::InvalidErrorsMessage,
            schema,
        )?;

        Ok(Some(Self {
            mapping,
            coordinate,
        }))
    }

    /// Check that only available variables are used, and the expression results in a string
    pub(super) fn type_check(self, schema: &SchemaInfo) -> Result<(), Message> {
        let Self {
            mapping,
            coordinate,
        } = self;
        let context = match coordinate.coordinate {
            ErrorsCoordinate::Source { .. } => {
                &Context::for_source_response(schema, &mapping.node, Code::InvalidErrorsMessage)
            }
            ErrorsCoordinate::Connect { connect } => &Context::for_connect_response(
                schema,
                connect,
                &mapping.node,
                Code::InvalidErrorsMessage,
            ),
        };

        expression::validate(&mapping.expression, context, &Shape::string([])).map_err(
            |mut message| {
                message.message = format!("In {coordinate}: {message}", message = message.message);
                message
            },
        )
    }
}

/// Coordinate for an `errors` argument in `@source` or `@connect`.
#[derive(Clone)]
pub(super) enum ErrorsCoordinate<'a> {
    Source {
        source: SourceDirectiveCoordinate<'a>,
    },
    Connect {
        connect: ConnectDirectiveCoordinate<'a>,
    },
}

impl Display for ErrorsCoordinate<'_> {
    fn fmt(&self, f: &mut Formatter) -> fmt::Result {
        match self {
            Self::Connect { connect } => {
                write!(f, "{connect}")
            }
            Self::Source { source } => {
                write!(f, "{source}")
            }
        }
    }
}

#[derive(Clone)]
struct ErrorsMessageCoordinate<'schema> {
    coordinate: ErrorsCoordinate<'schema>,
}

impl Display for ErrorsMessageCoordinate<'_> {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        match &self.coordinate {
            ErrorsCoordinate::Source { source } => {
                write!(
                    f,
                    "`@{directive_name}(name: \"{source_name}\" {ERRORS_ARGUMENT_NAME}.{ERRORS_MESSAGE_ARGUMENT_NAME}:)`",
                    directive_name = source.directive.name,
                    source_name = source.name
                )
            }
            ErrorsCoordinate::Connect { connect } => {
                write!(
                    f,
                    "`@{directive_name}({ERRORS_ARGUMENT_NAME}.{ERRORS_MESSAGE_ARGUMENT_NAME}:)` on `{element}`",
                    directive_name = connect.directive.name,
                    element = connect.element
                )
            }
        }
    }
}

struct ErrorsExtensions<'schema> {
    selection: JSONSelection,
    node: Node<Value>,
    coordinate: ErrorsExtensionsCoordinate<'schema>,
}

impl<'schema> ErrorsExtensions<'schema> {
    pub(super) fn parse(
        errors_arg: &'schema [(Name, Node<Value>)],
        coordinate: ErrorsCoordinate<'schema>,
        schema: &'schema SchemaInfo,
    ) -> Result<Option<Self>, Message> {
        let Some((_, value)) = errors_arg
            .iter()
            .find(|(name, _)| name == &ERRORS_EXTENSIONS_ARGUMENT_NAME)
        else {
            return Ok(None);
        };
        let coordinate = ErrorsExtensionsCoordinate { coordinate };

        let MappingArgument { expression, node } = parse_mapping_argument(
            value,
            coordinate.clone(),
            Code::InvalidErrorsMessage,
            schema,
        )?;

        Ok(Some(Self {
            selection: expression.expression,
            node,
            coordinate,
        }))
    }

    /// Check that the selection only uses allowed variables and evaluates to an object
    pub(super) fn type_check(self, schema: &SchemaInfo) -> Result<(), Message> {
        let Self {
            selection,
            node,
            coordinate,
        } = self;
        let context = match coordinate.coordinate {
            ErrorsCoordinate::Source { .. } => {
                &Context::for_source_response(schema, &node, Code::InvalidErrorsMessage)
            }
            ErrorsCoordinate::Connect { connect } => {
                &Context::for_connect_response(schema, connect, &node, Code::InvalidErrorsMessage)
            }
        };

        expression::validate(
            &Expression {
                expression: selection,
                location: 0..node
                    .location()
                    .map(|location| location.node_len())
                    .unwrap_or_default(),
            },
            context,
            &Shape::dict(Shape::unknown([]), []),
        )
        .map_err(|mut message| {
            message.message = format!("In {coordinate}: {message}", message = message.message);
            message
        })
    }
}

#[derive(Clone)]
struct ErrorsExtensionsCoordinate<'schema> {
    coordinate: ErrorsCoordinate<'schema>,
}

impl Display for ErrorsExtensionsCoordinate<'_> {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        match &self.coordinate {
            ErrorsCoordinate::Source { source } => {
                write!(
                    f,
                    "`@{directive_name}(name: \"{source_name}\" {ERRORS_ARGUMENT_NAME}.{ERRORS_EXTENSIONS_ARGUMENT_NAME}:)`",
                    directive_name = source.directive.name,
                    source_name = source.name
                )
            }
            ErrorsCoordinate::Connect { connect } => {
                write!(
                    f,
                    "`@{directive_name}({ERRORS_ARGUMENT_NAME}.{ERRORS_EXTENSIONS_ARGUMENT_NAME}:)` on `{element}`",
                    directive_name = connect.directive.name,
                    element = connect.element
                )
            }
        }
    }
}

/// The `@connect(isSuccess:)` or `@source(isSuccess:)` argument
pub(crate) struct IsSuccessArgument<'schema> {
    expression: Expression,
    node: Node<Value>,
    coordinate: IsSuccessCoordinate<'schema>,
}

impl<'schema> IsSuccessArgument<'schema> {
    pub(crate) fn parse_for_connector(
        connect: ConnectDirectiveCoordinate<'schema>,
        schema: &'schema SchemaInfo,
    ) -> Result<Option<Self>, Message> {
        Self::parse(
            IsSuccessCoordinate {
                coordinate: ErrorsCoordinate::Connect { connect },
            },
            schema,
        )
    }

    pub(crate) fn parse_for_source(
        source: SourceDirectiveCoordinate<'schema>,
        schema: &'schema SchemaInfo,
    ) -> Result<Option<Self>, Message> {
        Self::parse(
            IsSuccessCoordinate {
                coordinate: ErrorsCoordinate::Source { source },
            },
            schema,
        )
    }

    fn parse(
        coordinate: IsSuccessCoordinate<'schema>,
        schema: &'schema SchemaInfo,
    ) -> Result<Option<Self>, Message> {
        let directive = match &coordinate.coordinate {
            ErrorsCoordinate::Source { source } => source.directive,
            ErrorsCoordinate::Connect { connect } => connect.directive,
        };
        // If the `isSuccess` argument cannot be found in provided args, Error
        let Some(value) = directive.specified_argument_by_name(&IS_SUCCESS_ARGUMENT_NAME) else {
            return Ok(None);
        };

        let MappingArgument { expression, node } =
            parse_mapping_argument(value, coordinate.clone(), Code::InvalidIsSuccess, schema)?;

        Ok(Some(Self {
            expression,
            coordinate,
            node,
        }))
    }

    /// Check that only available variables are used, and the expression results in a boolean
    pub(crate) fn type_check(self, schema: &SchemaInfo<'_>) -> Result<(), Message> {
        let context = match self.coordinate.coordinate {
            ErrorsCoordinate::Source { .. } => {
                &Context::for_source_response(schema, &self.node, Code::InvalidIsSuccess)
            }
            ErrorsCoordinate::Connect { connect } => {
                &Context::for_connect_response(schema, connect, &self.node, Code::InvalidIsSuccess)
            }
        };
        expression::validate(&self.expression, context, &Shape::bool([])).map_err(|mut message| {
            message.message = format!(
                "In {coordinate}: {message}",
                coordinate = self.coordinate,
                message = message.message
            );
            message
        })
    }
}
