use std::iter::once;
use std::ops::Range;

use apollo_compiler::ast::FieldDefinition;
use apollo_compiler::parser::LineColumn;
use apollo_compiler::parser::SourceMap;
use apollo_compiler::schema::Component;
use apollo_compiler::schema::Directive;
use apollo_compiler::schema::ObjectType;
use apollo_compiler::Node;
use apollo_compiler::Schema;
use itertools::Itertools;

use super::coordinates::connect_directive_selection_coordinate;
use super::require_value_is_str;
use super::Code;
use super::Message;
use super::Name;
use super::Value;
use crate::sources::connect::json_selection::JSONSelectionVisitor;
use crate::sources::connect::spec::schema::CONNECT_SELECTION_ARGUMENT_NAME;
use crate::sources::connect::validation::coordinates::connect_directive_http_body_coordinate;
use crate::sources::connect::JSONSelection;

pub(super) fn validate_selection(
    field: &Component<FieldDefinition>,
    connect_directive: &Node<Directive>,
    parent_type: &Node<ObjectType>,
    schema: &Schema,
) -> Option<Message> {
    let (selection_value, json_selection) =
        match get_json_selection(connect_directive, parent_type, &field.name, &schema.sources) {
            Ok(selection) => selection,
            Err(err) => return Some(err),
        };

    let Some(return_type) = schema.get_object(field.ty.inner_named_type()) else {
        // TODO: Validate scalars
        return None;
    };

    SelectionValidator {
        root: PathPart::Root(parent_type),
        schema,
        path: vec![PathPart::Field {
            definition: field,
            ty: return_type,
        }],
        selection_coordinate: connect_directive_selection_coordinate(
            &connect_directive.name,
            parent_type,
            &field.name,
        ),
        selection_location: selection_value.line_column_range(&schema.sources),
    }
    .walk(&json_selection)
    .err()
}

pub(super) fn validate_body_selection(
    connect_directive: &Node<Directive>,
    parent_type: &Node<ObjectType>,
    field: &Component<FieldDefinition>,
    schema: &Schema,
    selection_node: &Node<Value>,
) -> Result<(), Message> {
    let coordinate =
        connect_directive_http_body_coordinate(&connect_directive.name, parent_type, &field.name);

    let selection_str = require_value_is_str(&selection_node, &coordinate, &schema.sources)?;

    if selection_str.is_empty() {
        return Err(Message {
            code: Code::InvalidJsonSelection,
            message: format!("{coordinate} is empty"),
            locations: selection_node
                .line_column_range(&schema.sources)
                .into_iter()
                .collect(),
        });
    }

    // TODO: parse and validate JSONSelection
    Ok(())
}

fn get_json_selection<'a>(
    connect_directive: &'a Node<Directive>,
    object: &Node<ObjectType>,
    field_name: &Name,
    source_map: &SourceMap,
) -> Result<(&'a Node<Value>, JSONSelection), Message> {
    let selection_arg = connect_directive
        .arguments
        .iter()
        .find(|arg| arg.name == CONNECT_SELECTION_ARGUMENT_NAME)
        .ok_or_else(|| Message {
            code: Code::GraphQLError,
            message: format!(
                "{coordinate} is required.",
                coordinate = connect_directive_selection_coordinate(
                    &connect_directive.name,
                    object,
                    field_name
                ),
            ),
            locations: connect_directive
                .line_column_range(source_map)
                .into_iter()
                .collect(),
        })?;
    let selection_str = require_value_is_str(
        &selection_arg.value,
        &connect_directive_selection_coordinate(&connect_directive.name, object, field_name),
        source_map,
    )?;

    if selection_str.is_empty() {
        return Err(Message {
            code: Code::InvalidJsonSelection,
            message: format!(
                "{coordinate} is empty",
                coordinate = connect_directive_selection_coordinate(
                    &connect_directive.name,
                    object,
                    field_name
                ),
            ),
            locations: selection_arg
                .value
                .line_column_range(source_map)
                .into_iter()
                .collect(),
        });
    }

    let (_rest, selection) = JSONSelection::parse(selection_str).map_err(|err| Message {
        code: Code::InvalidJsonSelection,
        message: format!(
            "{coordinate} is not a valid JSONSelection: {err}",
            coordinate =
                connect_directive_selection_coordinate(&connect_directive.name, object, field_name),
        ),
        locations: selection_arg
            .value
            .line_column_range(source_map)
            .into_iter()
            .collect(),
    })?;
    Ok((&selection_arg.value, selection))
}

struct SelectionValidator<'schema> {
    schema: &'schema Schema,
    root: PathPart<'schema>,
    path: Vec<PathPart<'schema>>,
    selection_location: Option<Range<LineColumn>>,
    selection_coordinate: String,
}

#[derive(Clone, Copy, Debug)]
enum PathPart<'a> {
    // Query, Mutation, Subscription OR an Entity type
    Root(&'a Node<ObjectType>),
    Field {
        definition: &'a Node<FieldDefinition>,
        ty: &'a Node<ObjectType>,
    },
}
// TODO: Once there is location data for JSONSelection, return multiple errors instead of stopping
//  at the first
impl<'schema> JSONSelectionVisitor for SelectionValidator<'schema> {
    type Error = Message;

    fn visit(&mut self, _name: &str) -> Result<(), Self::Error> {
        // TODO: Validate that the field exists
        Ok(())
    }

    fn enter_group(&mut self, field_name: &str) -> Result<(), Self::Error> {
        let parent = self.path.last().copied().unwrap_or(self.root);
        let parent_type = match parent {
            PathPart::Root(root) => root,
            PathPart::Field { ty, .. } => ty,
        };

        let definition = parent_type.fields.get(field_name).ok_or_else(||  {
            Message {
                code: Code::SelectedFieldNotFound,
                message: format!(
                    "{coordinate} contains field `{field_name}`, which does not exist on `{parent_type}`.",
                    coordinate = &self.selection_coordinate,
                    parent_type = parent_type.name
                ),
                locations: self.selection_location.iter().cloned().collect(),
            }})?;

        let ty = self.schema.get_object(definition.ty.inner_named_type()).ok_or_else(|| {
            Message {
                code: Code::GroupSelectionIsNotObject,
                message: format!(
                    "{coordinate} selects a group `{field_name}`, but `{parent_type}.{field_name}` is not an object.",
                    coordinate = &self.selection_coordinate,
                    parent_type = parent_type.name,
                ),
                locations: self.selection_location.iter().cloned().chain(definition.line_column_range(&self.schema.sources)).collect(),
            }})?;

        for seen_part in self.path_with_root() {
            let (seen_type, field_def) = match seen_part {
                PathPart::Root(root) => (root, None),
                PathPart::Field { ty, definition } => (ty, Some(definition)),
            };

            if seen_type == ty {
                return Err(Message {
                    code: Code::CircularReference,
                    message: format!(
                        "Circular reference detected in {coordinate}: type `{new_object_name}` appears more than once in `{selection_path}`. For more information, see https://go.apollo.dev/connectors/limitations#circular-references",
                        coordinate = &self.selection_coordinate,
                        selection_path = self.path_string(&definition.name),
                        new_object_name = ty.name,
                    ),
                    locations:
                        self.selection_location.iter().cloned()
                        // Root field includes the selection location, which duplicates the diagnostic
                            .chain(field_def.and_then(|def| def.line_column_range(&self.schema.sources)))
                            .chain(definition.line_column_range(&self.schema.sources))
                            .collect(),
                });
            }
        }
        self.path.push(PathPart::Field { definition, ty });
        Ok(())
    }

    fn exit_group(&mut self) -> Result<(), Self::Error> {
        self.path.pop();
        Ok(())
    }

    fn finish(self) -> Result<(), Self::Error> {
        Ok(())
    }
}

impl SelectionValidator<'_> {
    fn path_with_root(&self) -> impl Iterator<Item = PathPart> {
        once(self.root).chain(self.path.iter().copied())
    }

    fn path_string(&self, tail: &str) -> String {
        self.path_with_root()
            .map(|part| match part {
                PathPart::Root(ty) => ty.name.as_str(),
                PathPart::Field { definition, .. } => definition.name.as_str(),
            })
            .chain(once(tail))
            .join(".")
    }
}
