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
use super::coordinates::SourceDirectiveCoordinate;
use super::expression::MappingArgument;
use super::expression::parse_mapping_argument;
use crate::sources::connect::JSONSelection;
use crate::sources::connect::Namespace;
use crate::sources::connect::spec::schema::ERRORS_ARGUMENT_NAME;
use crate::sources::connect::spec::schema::ERRORS_EXTENSIONS_ARGUMENT_NAME;
use crate::sources::connect::spec::schema::ERRORS_MESSAGE_ARGUMENT_NAME;
use crate::sources::connect::string_template::Expression;
use crate::sources::connect::validation::Code;
use crate::sources::connect::validation::Message;
use crate::sources::connect::validation::expression;
use crate::sources::connect::validation::expression::Context;
use crate::sources::connect::validation::graphql::GraphQLString;
use crate::sources::connect::validation::graphql::SchemaInfo;

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
        let directive = match coordinate {
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
            ErrorsMessage::parse(errors_arg, coordinate, schema)
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
                m.selection
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
    selection: JSONSelection,
    string: GraphQLString<'schema>,
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

        let MappingArgument { expression, string } = parse_mapping_argument(
            value,
            &coordinate.to_string(),
            None,
            Code::InvalidErrorsMessage,
            &schema.sources,
        )?;

        Ok(Some(Self {
            selection: expression.expression,
            string,
            coordinate,
        }))
    }

    /// Check that only available variables are used, and the expression results in a string
    pub(super) fn type_check(self, schema: &SchemaInfo) -> Result<(), Message> {
        let Self {
            selection,
            string,
            coordinate,
        } = self;
        let context = match coordinate.coordinate {
            ErrorsCoordinate::Source { .. } => {
                &Context::for_source_response(schema, &string, Code::InvalidErrorsMessage)
            }
            ErrorsCoordinate::Connect { connect } => {
                &Context::for_connect_response(schema, connect, &string, Code::InvalidErrorsMessage)
            }
        };

        expression::validate(
            &Expression {
                expression: selection,
                location: 0..string.as_str().len(),
            },
            context,
            &Shape::string([]),
        )
        .map_err(|mut message| {
            message.message = format!("In {coordinate}: {message}", message = message.message);
            message
        })
    }
}

/// Coordinate for an `errors` argument in `@source` or `@connect`.
#[derive(Clone, Copy)]
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

#[derive(Clone, Copy)]
struct ErrorsMessageCoordinate<'schema> {
    coordinate: ErrorsCoordinate<'schema>,
}

impl Display for ErrorsMessageCoordinate<'_> {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        match self.coordinate {
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
    string: GraphQLString<'schema>,
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

        let MappingArgument { expression, string } = parse_mapping_argument(
            value,
            &coordinate.to_string(),
            None,
            Code::InvalidErrorsMessage,
            &schema.sources,
        )?;

        Ok(Some(Self {
            selection: expression.expression,
            string,
            coordinate,
        }))
    }

    /// Check that the selection only uses allowed variables and evaluates to an object
    pub(super) fn type_check(self, schema: &SchemaInfo) -> Result<(), Message> {
        let Self {
            selection,
            string,
            coordinate,
        } = self;
        let context = match coordinate.coordinate {
            ErrorsCoordinate::Source { .. } => {
                &Context::for_source_response(schema, &string, Code::InvalidErrorsMessage)
            }
            ErrorsCoordinate::Connect { connect } => {
                &Context::for_connect_response(schema, connect, &string, Code::InvalidErrorsMessage)
            }
        };

        expression::validate(
            &Expression {
                expression: selection,
                location: 0..string.as_str().len(),
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

#[derive(Clone, Copy)]
struct ErrorsExtensionsCoordinate<'schema> {
    coordinate: ErrorsCoordinate<'schema>,
}

impl Display for ErrorsExtensionsCoordinate<'_> {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        match self.coordinate {
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
