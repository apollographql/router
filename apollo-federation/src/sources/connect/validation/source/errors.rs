//! Parsing and validation for `@source(errors:)`

use std::fmt::Display;

use apollo_compiler::Name;
use apollo_compiler::Node;
use apollo_compiler::ast::Value;
use multi_try::MultiTry;
use shape::Shape;

use crate::sources::connect::JSONSelection;
use crate::sources::connect::spec::schema::ERRORS_ARGUMENT_NAME;
use crate::sources::connect::spec::schema::ERRORS_EXTENSIONS_ARGUMENT_NAME;
use crate::sources::connect::spec::schema::ERRORS_MESSAGE_ARGUMENT_NAME;
use crate::sources::connect::string_template::Expression;
use crate::sources::connect::validation::Code;
use crate::sources::connect::validation::Message;
use crate::sources::connect::validation::coordinates::SourceDirectiveCoordinate;
use crate::sources::connect::validation::expression;
use crate::sources::connect::validation::expression::Context;
use crate::sources::connect::validation::graphql::GraphQLString;
use crate::sources::connect::validation::graphql::SchemaInfo;

/// A valid, parsed (but not type-checked) `@source(errors:)`.
pub(super) struct Errors<'schema> {
    message: Option<ErrorsMessage<'schema>>,
    extensions: Option<ErrorsExtensions<'schema>>,
}

impl<'schema> Errors<'schema> {
    /// Parse the `@source(errors:)` argument and run just enough checks to be able to use the
    /// argument at runtime. More advanced checks are done in [`Self::type_check`].
    ///
    /// Two sub-pieces are always parsed, and the errors from _all_ of those pieces are returned
    /// together in the event of failure:
    /// 1. `errors.message` with [`ErrorsMessage::parse`]
    /// 2. `errors.extensions` with [`ErrorsExtensions::parse`]
    ///
    /// The order these pieces run in doesn't matter and shouldn't affect the output.
    pub(super) fn parse(
        coordinate: SourceDirectiveCoordinate<'schema>,
        schema: &'schema SchemaInfo,
    ) -> Result<Self, Vec<Message>> {
        let Some((errors_arg, _errors_arg_node)) = coordinate
            .directive
            .specified_argument_by_name(&ERRORS_ARGUMENT_NAME)
            .and_then(|arg| Some((arg.as_object()?, arg)))
        else {
            return Ok(Self {
                message: None,
                extensions: None,
            });
        };

        ErrorsMessage::parse(errors_arg, coordinate, schema)
            .map_err(|err| vec![err])
            .and_try(
                ErrorsExtensions::parse(errors_arg, coordinate, schema).map_err(|err| vec![err]),
            )
            .map_err(|nested| nested.into_iter().flatten().collect())
            .map(|(message, extensions)| Self {
                message,
                extensions,
            })
    }

    /// Type-check the `@source(errors:)` directive.
    ///
    /// Does things like ensuring that every accessed variable actually exists and that expressions
    /// used in the URL and headers result in scalars.
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
}

struct ErrorsMessage<'schema> {
    selection: JSONSelection,
    string: GraphQLString<'schema>,
    coordinate: ErrorsMessageCoordinate<'schema>,
}

impl<'schema> ErrorsMessage<'schema> {
    pub(super) fn parse(
        errors_arg: &'schema [(Name, Node<Value>)],
        connect: SourceDirectiveCoordinate<'schema>,
        schema: &'schema SchemaInfo,
    ) -> Result<Option<Self>, Message> {
        let Some((_, value)) = errors_arg
            .iter()
            .find(|(name, _)| name == &ERRORS_MESSAGE_ARGUMENT_NAME)
        else {
            return Ok(None);
        };
        let coordinate = ErrorsMessageCoordinate { connect };

        // Ensure that the message selection is a valid JSON selection string
        let string = match GraphQLString::new(value, &schema.sources) {
            Ok(selection_str) => selection_str,
            Err(_) => {
                return Err(Message {
                    code: Code::GraphQLError,
                    message: format!("{coordinate} must be a string."),
                    locations: value
                        .line_column_range(&schema.sources)
                        .into_iter()
                        .collect(),
                });
            }
        };
        let selection = match JSONSelection::parse(string.as_str()) {
            Ok(selection) => selection,
            Err(err) => {
                return Err(Message {
                    code: Code::InvalidErrorsMessage,
                    message: format!("{coordinate} is not valid: {err}"),
                    locations: value
                        .line_column_range(&schema.sources)
                        .into_iter()
                        .collect(),
                });
            }
        };
        if selection.is_empty() {
            return Err(Message {
                code: Code::InvalidErrorsMessage,
                message: format!("{coordinate} is empty"),
                locations: value
                    .line_column_range(&schema.sources)
                    .into_iter()
                    .collect(),
            });
        }

        Ok(Some(Self {
            selection,
            string,
            coordinate,
        }))
    }

    /// Check that the selection of the body matches the inputs at this location.
    ///
    /// TODO: check keys here?
    pub(super) fn type_check(self, schema: &SchemaInfo) -> Result<(), Message> {
        let Self {
            selection,
            string,
            coordinate,
        } = self;
        expression::validate(
            &Expression {
                expression: selection,
                location: 0..string.as_str().len(),
            },
            &Context::for_source_response(schema, &string, Code::InvalidErrorsMessage),
            &Shape::string([]),
        )
        .map_err(|mut message| {
            message.message = format!("In {coordinate}: {message}", message = message.message);
            message
        })
    }
}

#[derive(Clone, Copy)]
struct ErrorsMessageCoordinate<'schema> {
    connect: SourceDirectiveCoordinate<'schema>,
}

impl Display for ErrorsMessageCoordinate<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "`@{connect_directive_name}({ERRORS_ARGUMENT_NAME}: {{{ERRORS_MESSAGE_ARGUMENT_NAME}:}})`",
            connect_directive_name = self.connect.directive.name,
        )
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
        connect: SourceDirectiveCoordinate<'schema>,
        schema: &'schema SchemaInfo,
    ) -> Result<Option<Self>, Message> {
        let Some((_, value)) = errors_arg
            .iter()
            .find(|(name, _)| name == &ERRORS_EXTENSIONS_ARGUMENT_NAME)
        else {
            return Ok(None);
        };
        let coordinate = ErrorsExtensionsCoordinate { connect };

        // Ensure that the message selection is a valid JSON selection string
        let string = match GraphQLString::new(value, &schema.sources) {
            Ok(selection_str) => selection_str,
            Err(_) => {
                return Err(Message {
                    code: Code::GraphQLError,
                    message: format!("{coordinate} must be a string."),
                    locations: value
                        .line_column_range(&schema.sources)
                        .into_iter()
                        .collect(),
                });
            }
        };
        let selection = match JSONSelection::parse(string.as_str()) {
            Ok(selection) => selection,
            Err(err) => {
                return Err(Message {
                    code: Code::InvalidErrorsExtensions,
                    message: format!("{coordinate} is not valid: {err}"),
                    locations: value
                        .line_column_range(&schema.sources)
                        .into_iter()
                        .collect(),
                });
            }
        };
        if selection.is_empty() {
            return Err(Message {
                code: Code::InvalidErrorsExtensions,
                message: format!("{coordinate} is empty"),
                locations: value
                    .line_column_range(&schema.sources)
                    .into_iter()
                    .collect(),
            });
        }

        Ok(Some(Self {
            selection,
            string,
            coordinate,
        }))
    }

    /// Check that the selection of the body matches the inputs at this location.
    ///
    /// TODO: check keys here?
    pub(super) fn type_check(self, schema: &SchemaInfo) -> Result<(), Message> {
        let Self {
            selection,
            string,
            coordinate,
        } = self;
        expression::validate(
            &Expression {
                expression: selection,
                location: 0..string.as_str().len(),
            },
            &Context::for_source_response(schema, &string, Code::InvalidErrorsMessage),
            &Shape::empty_object([]),
        )
        .map_err(|mut message| {
            message.message = format!("In {coordinate}: {message}", message = message.message);
            message
        })
    }
}

#[derive(Clone, Copy)]
struct ErrorsExtensionsCoordinate<'schema> {
    connect: SourceDirectiveCoordinate<'schema>,
}

impl Display for ErrorsExtensionsCoordinate<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "`@{connect_directive_name}({ERRORS_ARGUMENT_NAME}: {{{ERRORS_EXTENSIONS_ARGUMENT_NAME}:}})`",
            connect_directive_name = self.connect.directive.name
        )
    }
}
