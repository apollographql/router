use std::ops::Range;

use apollo_compiler::ast::Value;
use apollo_compiler::parser::LineColumn;
use apollo_compiler::Name;
use apollo_compiler::Node;

use crate::sources::connect::json_selection::KnownVariable;
use crate::sources::connect::json_selection::PathList;
use crate::sources::connect::json_selection::Ranged;
use crate::sources::connect::json_selection::WithRange;
use crate::sources::connect::spec::schema::CONNECT_BODY_ARGUMENT_NAME;
use crate::sources::connect::validation::coordinates::BodyCoordinate;
use crate::sources::connect::validation::coordinates::ConnectDirectiveCoordinate;
use crate::sources::connect::validation::graphql::GraphQLString;
use crate::sources::connect::validation::graphql::SchemaInfo;
use crate::sources::connect::validation::selection::get_json_selection;
use crate::sources::connect::validation::Code;
use crate::sources::connect::validation::Message;
use crate::sources::connect::JSONSelection;
use crate::sources::connect::PathSelection;

pub(crate) fn validate_body(
    http_arg: &[(Name, Node<Value>)],
    connect_coordinate: ConnectDirectiveCoordinate,
    schema: &SchemaInfo,
) -> Result<(), Message> {
    let Some(body) = http_arg
        .iter()
        .find_map(|(name, value)| (name == &CONNECT_BODY_ARGUMENT_NAME).then_some(value))
    else {
        return Ok(());
    };
    let coordinate = BodyCoordinate::from(connect_coordinate);
    let (value, json_selection) = get_json_selection(coordinate, &schema.sources, body)?;
    let arg = Arg {
        value,
        coordinate,
        schema,
    };

    match json_selection {
        JSONSelection::Named(_) => Ok(()), // TODO: validate_sub_selection(sub_selection, arg),
        JSONSelection::Path(path_selection) => validate_path_selection(path_selection, arg),
    }
}

fn validate_path_selection(path_selection: PathSelection, arg: Arg) -> Result<(), Message> {
    let PathSelection { path } = path_selection;
    match path.as_ref() {
        PathList::Var(variable, trailing) => {
            match variable.as_ref() {
                KnownVariable::This => {
                    // TODO: Verify that the name & shape matches arg.coordinate.field_coordinate.object
                    Ok(())
                }
                KnownVariable::Args => validate_args_path(trailing, arg)
                    .map_err(|err| err.with_fallback_locations(arg.get_selection_location(&path))),
                KnownVariable::Config => Ok(()), // We have no way of knowing is this is valid, yet
                KnownVariable::Dollar => {
                    // TODO: Validate that this is followed only by a method
                    Ok(())
                }
                KnownVariable::AtSign => {
                    // TODO: This is probably just not allowed?
                    Ok(())
                }
            }
        }
        PathList::Key(_, _) => {
            // TODO: Make sure this is aliased & built from data we have
            Ok(())
        }
        PathList::Expr(_, _) => {
            Ok(()) // We don't know what shape to expect, so any is fine
        }
        PathList::Method(_, _, _) => {
            // TODO: This is a parse error, but return an error here just in case
            Ok(())
        }
        PathList::Selection(_) => {
            // TODO: This is a parse error, but return an error here just in case
            Ok(())
        }
        PathList::Empty => {
            // TODO: This is a parse error, but return an error here just in case
            Ok(())
        }
    }
}

// Validate a reference to `$args`
fn validate_args_path(path: &WithRange<PathList>, arg: Arg) -> Result<(), Message> {
    match path.as_ref() {
        PathList::Var(var_type, _) => {
            // This is probably caught by the parser, but we can't type-safely guarantee that yet
            Err(Message {
                code: Code::InvalidJsonSelection,
                message: format!(
                    "Can't reference a path within another path. `$args.{var_type}` is invalid.",
                    var_type = var_type.as_str()
                ),
                locations: arg.get_selection_location(var_type).collect(),
            })
        }
        PathList::Key(_, _) => {
            // TODO: Make sure that the path matches an argument, then validate the shape of that path
            Ok(())
        }
        PathList::Expr(_, _) => Err(Message {
            code: Code::InvalidJsonSelection,
            message: "Can't use a literal expression after `$args`.".to_string(),
            locations: arg.get_selection_location(path).collect(),
        }),
        PathList::Method(_, _, _) => {
            // TODO: Validate that the method can be called directly on `$args`
            Ok(())
        }
        PathList::Selection(_) => {
            // TODO: Validate that the `SubSelection` is valid for `$args`
            Ok(())
        }
        PathList::Empty => {
            // They're selecting the entirety of `$args`, this is okay as long as there are any args!
            if arg.coordinate.field_coordinate.field.arguments.is_empty() {
                Err(Message {
                    code: Code::InvalidJsonSelection,
                    message: "Can't use `$args` when there are no arguments.".to_string(),
                    locations: vec![],
                })
            } else {
                Ok(())
            }
        }
    }
}

/// The `@connect(http.body:)` argument.
#[derive(Clone, Copy)]
struct Arg<'schema> {
    value: GraphQLString<'schema>,
    coordinate: BodyCoordinate<'schema>,
    schema: &'schema SchemaInfo<'schema>,
}

impl Arg<'_> {
    fn get_selection_location<T>(
        &self,
        selection: &impl Ranged<T>,
    ) -> impl Iterator<Item = Range<LineColumn>> {
        selection
            .range()
            .and_then(|range| self.value.line_col_for_subslice(range, self.schema))
            .into_iter()
    }
}
