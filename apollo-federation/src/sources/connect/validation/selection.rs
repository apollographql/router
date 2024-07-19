use std::iter::once;
use std::ops::Range;

use apollo_compiler::ast::FieldDefinition;
use apollo_compiler::schema::Component;
use apollo_compiler::schema::Directive;
use apollo_compiler::schema::ObjectType;
use apollo_compiler::Node;
use apollo_compiler::Schema;
use apollo_compiler::SourceMap;
use itertools::Itertools;

use super::coordinates::connect_directive_selection_coordinate;
use super::require_value_is_str;
use super::Code;
use super::Location;
use super::Message;
use super::Name;
use super::Value;
use crate::sources::connect::json_selection::JSONSelectionVisitor;
use crate::sources::connect::spec::schema::CONNECT_SELECTION_ARGUMENT_NAME;
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
        root: Field {
            definition: field,
            ty: return_type,
        },
        schema,
        path: vec![],
        selection_coordinate: connect_directive_selection_coordinate(
            &connect_directive.name,
            parent_type,
            &field.name,
        ),
        selection_location: Location::from_node(selection_value.location(), &schema.sources),
    }
    .walk(&json_selection)
    .err()
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
            locations: Location::from_node(connect_directive.location(), source_map)
                .into_iter()
                .collect(),
        })?;
    let selection_str = require_value_is_str(
        &selection_arg.value,
        &connect_directive_selection_coordinate(&connect_directive.name, object, field_name),
        source_map,
    )?;

    let (_rest, selection) = JSONSelection::parse(selection_str).map_err(|err| Message {
        code: Code::InvalidJsonSelection,
        message: format!(
            "{coordinate} is not a valid JSONSelection: {err}",
            coordinate =
                connect_directive_selection_coordinate(&connect_directive.name, object, field_name),
        ),
        locations: Location::from_node(selection_arg.value.location(), source_map)
            .into_iter()
            .collect(),
    })?;
    Ok((&selection_arg.value, selection))
}

struct SelectionValidator<'schema> {
    schema: &'schema Schema,
    root: Field<'schema>,
    path: Vec<Field<'schema>>,
    selection_location: Option<Range<Location>>,
    selection_coordinate: String,
}

#[derive(Clone, Copy, Debug)]
struct Field<'a> {
    definition: &'a Node<FieldDefinition>,
    ty: &'a Node<ObjectType>,
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
        let definition = parent.ty.fields.get(field_name).ok_or_else(||  {
            Message {
                code: Code::SelectedFieldNotFound,
                message: format!(
                    "{coordinate} contains field `{field_name}`, which does not exist on `{parent_type}`.",
                    coordinate = &self.selection_coordinate,
                    parent_type = parent.ty.name
                ),
                locations: self.selection_location.iter().cloned().collect(),
            }})?;

        let ty = self.schema.get_object(definition.ty.inner_named_type()).ok_or_else(|| {
            Message {
                code: Code::GroupSelectionIsNotObject,
                message: format!(
                    "{coordinate} selects a group `{field_name}`, but `{parent_type}.{field_name}` is not an object.",
                    coordinate = &self.selection_coordinate,
                    parent_type = parent.ty.name,
                ),
                locations: self.selection_location.iter().cloned().chain(Location::from_node(definition.location(), &self.schema.sources)).collect(),
            }})?;

        for parent_field in self.path_with_root() {
            if parent_field.ty == ty {
                return Err(Message {
                    code: Code::CircularReference,
                    message: format!(
                        "{coordinate} path `{selection_path}` contains a circular reference to `{new_object_name}`.",
                        coordinate = &self.selection_coordinate,
                        selection_path = self.path_with_root().map(|field| field.definition.name.as_str()).join("."),
                        new_object_name = ty.name,
                    ),
                    locations:
                        self.selection_location.iter().cloned()
                            .chain((parent_field.definition != self.root.definition).then(|| {
                                // Root field includes the selection location, which duplicates the diagnostic
                                Location::from_node(parent_field.definition.location(), &self.schema.sources)
                            }).flatten())
                            .chain(Location::from_node(definition.location(), &self.schema.sources))
                            .collect(),
                });
            }
        }
        self.path.push(Field { definition, ty });
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
    fn path_with_root(&self) -> impl Iterator<Item = Field> {
        once(self.root).chain(self.path.iter().copied())
    }
}
