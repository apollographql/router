use std::iter::once;
use std::ops::Range;

use apollo_compiler::ast::FieldDefinition;
use apollo_compiler::collections::IndexSet;
use apollo_compiler::parser::LineColumn;
use apollo_compiler::parser::SourceMap;
use apollo_compiler::schema::Component;
use apollo_compiler::schema::Directive;
use apollo_compiler::schema::ExtendedType;
use apollo_compiler::schema::ObjectType;
use apollo_compiler::Node;
use apollo_compiler::Schema;
use itertools::Itertools;

use super::coordinates::ConnectDirectiveCoordinate;
use super::coordinates::SelectionCoordinate;
use super::require_value_is_str;
use super::Code;
use super::Message;
use super::Name;
use super::Value;
use crate::sources::connect::expand::visitors::FieldVisitor;
use crate::sources::connect::expand::visitors::GroupVisitor;
use crate::sources::connect::json_selection::NamedSelection;
use crate::sources::connect::json_selection::Ranged;
use crate::sources::connect::spec::schema::CONNECT_SELECTION_ARGUMENT_NAME;
use crate::sources::connect::validation::coordinates::connect_directive_http_body_coordinate;
use crate::sources::connect::validation::graphql::GraphQLString;
use crate::sources::connect::validation::graphql::SchemaInfo;
use crate::sources::connect::JSONSelection;
use crate::sources::connect::SubSelection;

pub(super) fn validate_selection(
    coordinate: ConnectDirectiveCoordinate,
    schema: &SchemaInfo,
    seen_fields: &mut IndexSet<(Name, Name)>,
) -> Result<(), Message> {
    let (selection_arg, json_selection) = get_json_selection(coordinate, &schema.sources)?;
    let field = coordinate.field_coordinate.field;

    let Some(return_type) = schema.get_object(field.ty.inner_named_type()) else {
        // TODO: Validate scalar return types
        return Ok(());
    };
    let Some(sub_selection) = json_selection.next_subselection() else {
        // TODO: Validate scalar selections
        return Ok(());
    };

    let group = Group {
        selection: sub_selection,
        ty: return_type,
        definition: field,
    };

    SelectionValidator {
        root: PathPart::Root(coordinate.field_coordinate.object),
        schema,
        path: Vec::new(),
        selection_arg,
        seen_fields,
    }
    .walk(group)
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

    let selection_str = require_value_is_str(selection_node, &coordinate, &schema.sources)?;

    let (_rest, selection) = JSONSelection::parse(selection_str).map_err(|err| Message {
        code: Code::InvalidJsonSelection,
        message: format!("{coordinate} is not a valid JSONSelection: {err}"),
        locations: selection_node
            .line_column_range(&schema.sources)
            .into_iter()
            .collect(),
    })?;

    if selection.is_empty() {
        return Err(Message {
            code: Code::InvalidJsonSelection,
            message: format!("{coordinate} is empty"),
            locations: selection_node
                .line_column_range(&schema.sources)
                .into_iter()
                .collect(),
        });
    }

    // TODO: validate JSONSelection
    Ok(())
}

fn get_json_selection<'a>(
    connect_directive: ConnectDirectiveCoordinate<'a>,
    source_map: &'a SourceMap,
) -> Result<(SelectionArg<'a>, JSONSelection), Message> {
    let coordinate = SelectionCoordinate::from(connect_directive);
    let selection_arg = connect_directive
        .directive
        .arguments
        .iter()
        .find(|arg| arg.name == CONNECT_SELECTION_ARGUMENT_NAME)
        .ok_or_else(|| Message {
            code: Code::GraphQLError,
            message: format!("{coordinate} is required."),
            locations: connect_directive
                .directive
                .line_column_range(source_map)
                .into_iter()
                .collect(),
        })?;
    let selection_str =
        GraphQLString::new(&selection_arg.value, source_map).map_err(|_| Message {
            code: Code::GraphQLError,
            message: format!("{coordinate} must be a string."),
            locations: selection_arg
                .line_column_range(source_map)
                .into_iter()
                .collect(),
        })?;

    let (_rest, selection) =
        JSONSelection::parse(selection_str.as_str()).map_err(|err| Message {
            code: Code::InvalidJsonSelection,
            message: format!("{coordinate} is not a valid JSONSelection: {err}",),
            locations: selection_arg
                .value
                .line_column_range(source_map)
                .into_iter()
                .collect(),
        })?;

    if selection.is_empty() {
        return Err(Message {
            code: Code::InvalidJsonSelection,
            message: format!("{coordinate} is empty",),
            locations: selection_arg
                .value
                .line_column_range(source_map)
                .into_iter()
                .collect(),
        });
    }

    Ok((
        SelectionArg {
            value: selection_str,
            coordinate,
        },
        selection,
    ))
}

struct SelectionArg<'schema> {
    value: GraphQLString<'schema>,
    coordinate: SelectionCoordinate<'schema>,
}

struct SelectionValidator<'schema, 'a> {
    schema: &'schema SchemaInfo<'schema>,
    root: PathPart<'schema>,
    path: Vec<PathPart<'schema>>,
    selection_arg: SelectionArg<'schema>,
    seen_fields: &'a mut IndexSet<(Name, Name)>,
}

impl SelectionValidator<'_, '_> {
    fn check_for_circular_reference(
        &self,
        field: Field,
        object: &Node<ObjectType>,
    ) -> Result<(), Message> {
        for (depth, seen_part) in self.path_with_root().enumerate() {
            let (seen_type, ancestor_field) = match seen_part {
                PathPart::Root(root) => (root, None),
                PathPart::Field { ty, definition } => (ty, Some(definition)),
            };

            if seen_type == object {
                return Err(Message {
                    code: Code::CircularReference,
                    message: format!(
                        "Circular reference detected in {coordinate}: type `{new_object_name}` appears more than once in `{selection_path}`. For more information, see https://go.apollo.dev/connectors/limitations#circular-references",
                        coordinate = &self.selection_arg.coordinate,
                        selection_path = self.path_string(field.definition),
                        new_object_name = object.name,
                    ),
                    // TODO: make a helper function for easier range collection
                    locations: self.get_range_location(field.inner_range())
                        // Skip over fields which duplicate the location of the selection
                        .chain(if depth > 1 {ancestor_field.and_then(|def| def.line_column_range(&self.schema.sources))} else {None})
                        .chain(field.definition.line_column_range(&self.schema.sources))
                        .collect(),
                });
            }
        }
        Ok(())
    }

    fn get_selection_location<T>(
        &self,
        selection: &impl Ranged<T>,
    ) -> impl Iterator<Item = Range<LineColumn>> {
        selection
            .range()
            .and_then(|range| {
                self.selection_arg
                    .value
                    .line_col_for_subslice(range, self.schema)
            })
            .into_iter()
    }

    fn get_range_location(
        &self,
        selection: Option<Range<usize>>,
    ) -> impl Iterator<Item = Range<LineColumn>> {
        selection
            .as_ref()
            .and_then(|range| {
                self.selection_arg
                    .value
                    .line_col_for_subslice(range.clone(), self.schema)
            })
            .into_iter()
    }
}

#[derive(Clone, Copy, Debug)]
struct Field<'schema> {
    selection: &'schema NamedSelection,
    definition: &'schema Node<FieldDefinition>,
}

impl<'schema> Field<'schema> {
    fn next_subselection(&self) -> Option<&'schema SubSelection> {
        self.selection.next_subselection()
    }

    fn inner_range(&self) -> Option<Range<usize>> {
        self.selection.range()
    }
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

impl<'a> PathPart<'a> {
    fn ty(&self) -> &Node<ObjectType> {
        match self {
            PathPart::Root(ty) => ty,
            PathPart::Field { ty, .. } => ty,
        }
    }
}

#[derive(Clone, Debug)]
struct Group<'schema> {
    selection: &'schema SubSelection,
    ty: &'schema Node<ObjectType>,
    definition: &'schema Node<FieldDefinition>,
}

// TODO: Once there is location data for JSONSelection, return multiple errors instead of stopping
//  at the first
impl<'schema> GroupVisitor<Group<'schema>, Field<'schema>> for SelectionValidator<'schema, '_> {
    /// If the both the selection and the schema agree that this field is an object, then we
    /// provide it back to the visitor to be walked.
    ///
    /// This does no validation, as we have to do that on the field level anyway.
    fn try_get_group_for_field(
        &self,
        field: &Field<'schema>,
    ) -> Result<Option<Group<'schema>>, Self::Error> {
        let Some(selection) = field.next_subselection() else {
            return Ok(None);
        };
        let Some(ty) = self
            .schema
            .get_object(field.definition.ty.inner_named_type())
        else {
            return Ok(None);
        };
        Ok(Some(Group {
            selection,
            ty,
            definition: field.definition,
        }))
    }

    /// Get all the fields for an object type / selection.
    /// Returns an error if a selection points at a field which does not exist on the schema.
    fn enter_group(&mut self, group: &Group<'schema>) -> Result<Vec<Field<'schema>>, Self::Error> {
        self.path.push(PathPart::Field {
            definition: group.definition,
            ty: group.ty,
        });
        group.selection.selections_iter().flat_map(|selection| {
            let mut results = Vec::new();
            for field_name in selection.names() {
                if let Some(definition) = group.ty.fields.get(field_name) {
                    results.push(Ok(Field {
                        selection,
                        definition,
                    }));
                } else {
                    results.push(Err(Message {
                        code: Code::SelectedFieldNotFound,
                        message: format!(
                            "{coordinate} contains field `{field_name}`, which does not exist on `{parent_type}`.",
                            coordinate = &self.selection_arg.coordinate,
                            parent_type = group.ty.name,
                        ),
                        locations: self.get_selection_location(selection).collect(),
                    }));
                }
            }
            results
        }).collect()
    }

    fn exit_group(&mut self) -> Result<(), Self::Error> {
        self.path.pop();
        Ok(())
    }
}

impl<'schema> FieldVisitor<Field<'schema>> for SelectionValidator<'schema, '_> {
    type Error = Message;

    fn visit(&mut self, field: Field<'schema>) -> Result<(), Self::Error> {
        let field_name = field.definition.name.as_str();
        let type_name = field.definition.ty.inner_named_type();
        let coordinate = self.selection_arg.coordinate;
        let field_type = self.schema.types.get(type_name).ok_or_else(|| Message {
            code: Code::GraphQLError,
            message: format!(
                "{coordinate} contains field `{field_name}`, which has undefined type `{type_name}.",
            ),
            locations: self.get_range_location(field.inner_range()).collect(),
        })?;
        let is_group = field.next_subselection().is_some();

        self.seen_fields.insert((
            self.last_field().ty().name.clone(),
            field.definition.name.clone(),
        ));

        if !field.definition.arguments.is_empty() {
            return Err(Message {
                code: Code::FieldWithArguments,
                message: format!(
                    "{coordinate} selects field `{parent_type}.{field_name}`, which has arguments. Only fields with a connector can have arguments.",
                    parent_type = self.last_field().ty().name,
                ),
                locations: self.get_range_location(field.inner_range()).chain(field.definition.line_column_range(&self.schema.sources)).collect(),
            });
        }

        match (field_type, is_group) {
            (ExtendedType::Object(object), true) => {
                self.check_for_circular_reference(field, object)
            },
            (_, true) => {
                Err(Message {
                    code: Code::GroupSelectionIsNotObject,
                    message: format!(
                        "{coordinate} selects a group `{field_name}{{}}`, but `{parent_type}.{field_name}` is of type `{type_name}` which is not an object.",
                        parent_type = self.last_field().ty().name,
                    ),
                    locations: self.get_range_location(field.inner_range()).chain(field.definition.line_column_range(&self.schema.sources)).collect(),
                })
            },
            (ExtendedType::Object(_), false) => {
                Err(Message {
                    code: Code::GroupSelectionRequiredForObject,
                    message: format!(
                        "`{parent_type}.{field_name}` is an object, so {coordinate} must select a group `{field_name}{{}}`.",
                        parent_type = self.last_field().ty().name,
                    ),
                    locations: self.get_range_location(field.inner_range()).chain(field.definition.line_column_range(&self.schema.sources)).collect(),
                })
            },
            (_, false) => Ok(()),
        }
    }
}

impl<'schema, 'a> SelectionValidator<'schema, 'a> {
    fn path_with_root(&self) -> impl Iterator<Item = PathPart> {
        once(self.root).chain(self.path.iter().copied())
    }

    fn path_string(&self, tail: &FieldDefinition) -> String {
        self.path_with_root()
            .map(|part| match part {
                PathPart::Root(ty) => ty.name.as_str(),
                PathPart::Field { definition, .. } => definition.name.as_str(),
            })
            .chain(once(tail.name.as_str()))
            .join(".")
    }

    fn last_field(&self) -> &PathPart {
        self.path.last().unwrap_or(&self.root)
    }
}
