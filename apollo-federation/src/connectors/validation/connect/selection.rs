//! Validate and check semantics of the `@connect(selection:)` argument

use std::fmt::Display;
use std::iter::once;
use std::ops::Range;

use apollo_compiler::Node;
use apollo_compiler::ast::FieldDefinition;
use apollo_compiler::ast::Value;
use apollo_compiler::parser::LineColumn;
use apollo_compiler::schema::ExtendedType;
use apollo_compiler::schema::ObjectType;
use itertools::Itertools;

use self::variables::VariableResolver;
use super::Code;
use super::Message;
use super::Name;
use crate::connectors::JSONSelection;
use crate::connectors::Namespace;
use crate::connectors::PathSelection;
use crate::connectors::SubSelection;
use crate::connectors::expand::visitors::FieldVisitor;
use crate::connectors::expand::visitors::GroupVisitor;
use crate::connectors::id::ConnectedElement;
use crate::connectors::json_selection::ExternalVarPaths;
use crate::connectors::json_selection::NamedSelection;
use crate::connectors::json_selection::Ranged;
use crate::connectors::spec::connect::CONNECT_SELECTION_ARGUMENT_NAME;
use crate::connectors::validation::coordinates::ConnectDirectiveCoordinate;
use crate::connectors::validation::coordinates::SelectionCoordinate;
use crate::connectors::validation::expression::MappingArgument;
use crate::connectors::validation::expression::parse_mapping_argument;
use crate::connectors::validation::graphql::SchemaInfo;
use crate::connectors::validation::graphql::subslice_location;
use crate::connectors::variable::Phase;
use crate::connectors::variable::Target;
use crate::connectors::variable::VariableContext;

mod variables;

/// The `@connect(selection:)` argument
pub(super) struct Selection<'schema> {
    parsed: JSONSelection,
    node: Node<Value>,
    coordinate: SelectionCoordinate<'schema>,
}

impl<'schema> Selection<'schema> {
    pub(super) fn parse(
        connect_directive: ConnectDirectiveCoordinate<'schema>,
        schema: &'schema SchemaInfo<'schema>,
    ) -> Result<Self, Message> {
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
                    .line_column_range(&schema.sources)
                    .into_iter()
                    .collect(),
            })?;

        let MappingArgument { expression, node } = parse_mapping_argument(
            &selection_arg.value,
            coordinate,
            Code::InvalidSelection,
            schema,
        )?;

        Ok(Self {
            parsed: expression.expression,
            node,
            coordinate,
        })
    }

    /// Type check the selection using the visitor pattern, returning a list of seen fields as
    /// (ObjectName, FieldName) pairs.
    pub(super) fn type_check(self, schema: &SchemaInfo) -> Result<Vec<(Name, Name)>, Message> {
        let coordinate = self.coordinate.connect;
        let context = VariableContext::new(&coordinate.element, Phase::Response, Target::Body);
        validate_selection_variables(
            &VariableResolver::new(context.clone(), schema),
            self.coordinate,
            &self.node,
            schema,
            context,
            self.parsed.external_var_paths(),
        )?;

        match coordinate.element {
            ConnectedElement::Field {
                parent_type,
                field_def,
                ..
            } => {
                let Some(return_type) = schema.get_object(field_def.ty.inner_named_type()) else {
                    // TODO: Validate scalar return types
                    return Ok(Vec::new());
                };
                let Some(sub_selection) = self.parsed.next_subselection() else {
                    // TODO: Validate scalar selections
                    return Ok(Vec::new());
                };

                let group = Group {
                    selection: sub_selection,
                    ty: return_type,
                    definition: Some(field_def),
                };

                SelectionValidator::new(
                    schema,
                    PathPart::Root(parent_type),
                    &self.node,
                    self.coordinate,
                )
                .walk(group)
                .map(|validator| validator.seen_fields)
            }
            ConnectedElement::Type { type_def } => {
                let Some(sub_selection) = self.parsed.next_subselection() else {
                    // TODO: Validate scalar selections
                    return Ok(Vec::new());
                };

                let group = Group {
                    selection: sub_selection,
                    ty: type_def,
                    definition: None,
                };

                SelectionValidator::new(
                    schema,
                    PathPart::Root(type_def),
                    &self.node,
                    self.coordinate,
                )
                .walk(group)
                .map(|validator| validator.seen_fields)
            }
        }
    }

    pub(super) fn variables(&self) -> impl Iterator<Item = Namespace> + '_ {
        self.parsed
            .variable_references()
            .map(|var_ref| var_ref.namespace.namespace)
    }
}

/// Validate variable references in a JSON Selection
pub(super) fn validate_selection_variables<'a>(
    variable_resolver: &VariableResolver,
    coordinate: impl Display,
    selection_str: &Node<Value>,
    schema: &SchemaInfo,
    context: VariableContext,
    variable_paths: impl IntoIterator<Item = &'a PathSelection>,
) -> Result<(), Message> {
    for path in variable_paths {
        if let Some(reference) = path.variable_reference() {
            variable_resolver
                .resolve(&reference, selection_str)
                .map_err(|mut err| {
                    err.message = format!("In {coordinate}: {message}", message = err.message);
                    err
                })?;
        } else if let Some(reference) = path.variable_reference::<String>() {
            return Err(Message {
                code: context.error_code(),
                message: format!(
                    "In {coordinate}: unknown variable `{namespace}`, must be one of {available}",
                    namespace = reference.namespace.namespace.as_str(),
                    available = context.namespaces_joined(),
                ),
                locations: reference
                    .namespace
                    .location
                    .iter()
                    .flat_map(|range| subslice_location(selection_str, range.clone(), schema))
                    .collect(),
            });
        }
    }
    Ok(())
}

struct SelectionValidator<'schema> {
    schema: &'schema SchemaInfo<'schema>,
    root: PathPart<'schema>,
    path: Vec<PathPart<'schema>>,
    node: &'schema Node<Value>,
    coordinate: SelectionCoordinate<'schema>,
    seen_fields: Vec<(Name, Name)>,
}

impl<'schema> SelectionValidator<'schema> {
    const fn new(
        schema: &'schema SchemaInfo<'schema>,
        root: PathPart<'schema>,
        node: &'schema Node<Value>,
        coordinate: SelectionCoordinate<'schema>,
    ) -> Self {
        Self {
            schema,
            root,
            path: Vec::new(),
            node,
            coordinate,
            seen_fields: Vec::new(),
        }
    }
}

impl SelectionValidator<'_> {
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
                        coordinate = &self.coordinate,
                        selection_path = self.path_string(field.definition),
                        new_object_name = object.name,
                    ),
                    // TODO: make a helper function for easier range collection
                    locations: self
                        .get_range_location(field.inner_range())
                        // Skip over fields which duplicate the location of the selection
                        .chain(if depth > 1 {
                            ancestor_field
                                .and_then(|def| def.line_column_range(&self.schema.sources))
                        } else {
                            None
                        })
                        .chain(field.definition.line_column_range(&self.schema.sources))
                        .collect(),
                });
            }
        }
        Ok(())
    }

    fn get_selection_location(
        &self,
        selection: &impl Ranged,
    ) -> impl Iterator<Item = Range<LineColumn>> {
        selection
            .range()
            .and_then(|range| subslice_location(self.node, range, self.schema))
            .into_iter()
    }

    fn get_range_location(
        &self,
        selection: Option<Range<usize>>,
    ) -> impl Iterator<Item = Range<LineColumn>> {
        selection
            .as_ref()
            .and_then(|range| subslice_location(self.node, range.clone(), self.schema))
            .into_iter()
    }

    fn path_with_root(&self) -> impl Iterator<Item = PathPart<'_>> {
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

    fn last_field(&self) -> &PathPart<'_> {
        self.path.last().unwrap_or(&self.root)
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

impl PathPart<'_> {
    const fn ty(&self) -> &Node<ObjectType> {
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
    definition: Option<&'schema Node<FieldDefinition>>,
}

// TODO: Once there is location data for JSONSelection, return multiple errors instead of stopping
//  at the first
impl<'schema> GroupVisitor<Group<'schema>, Field<'schema>> for SelectionValidator<'schema> {
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
            definition: Some(field.definition),
        }))
    }

    /// Get all the fields for an object type / selection.
    /// Returns an error if a selection points at a field which does not exist on the schema.
    fn enter_group(&mut self, group: &Group<'schema>) -> Result<Vec<Field<'schema>>, Self::Error> {
        // This is `None` at the root of a connector on a type, and we've already added the root path part
        if let Some(definition) = group.definition {
            self.path.push(PathPart::Field {
                definition,
                ty: group.ty,
            });
        }

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
                            coordinate = &self.coordinate,
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

impl<'schema> FieldVisitor<Field<'schema>> for SelectionValidator<'schema> {
    type Error = Message;

    fn visit(&mut self, field: Field<'schema>) -> Result<(), Self::Error> {
        let field_name = field.definition.name.as_str();
        let type_name = field.definition.ty.inner_named_type();
        let coordinate = self.coordinate;
        let field_type = self.schema.types.get(type_name).ok_or_else(|| Message {
            code: Code::GraphQLError,
            message: format!(
                "{coordinate} contains field `{field_name}`, which has undefined type `{type_name}.",
            ),
            locations: self.get_range_location(field.inner_range()).collect(),
        })?;
        let is_group = field.next_subselection().is_some();

        self.seen_fields.push((
            self.last_field().ty().name.clone(),
            field.definition.name.clone(),
        ));

        if !field.definition.arguments.is_empty() {
            return Err(Message {
                code: Code::ConnectorsFieldWithArguments,
                message: format!(
                    "{coordinate} selects field `{parent_type}.{field_name}`, which has arguments. Only fields with a connector can have arguments.",
                    parent_type = self.last_field().ty().name,
                ),
                locations: self
                    .get_range_location(field.inner_range())
                    .chain(field.definition.line_column_range(&self.schema.sources))
                    .collect(),
            });
        }

        match (field_type, is_group) {
            (ExtendedType::Object(object), true) => {
                self.check_for_circular_reference(field, object)
            }
            (_, true) => Err(Message {
                code: Code::GroupSelectionIsNotObject,
                message: format!(
                    "{coordinate} selects a group `{field_name}{{}}`, but `{parent_type}.{field_name}` is of type `{type_name}` which is not an object.",
                    parent_type = self.last_field().ty().name,
                ),
                locations: self
                    .get_range_location(field.inner_range())
                    .chain(field.definition.line_column_range(&self.schema.sources))
                    .collect(),
            }),
            (ExtendedType::Object(_), false) => Err(Message {
                code: Code::GroupSelectionRequiredForObject,
                message: format!(
                    "`{parent_type}.{field_name}` is an object, so {coordinate} must select a group `{field_name}{{}}`.",
                    parent_type = self.last_field().ty().name,
                ),
                locations: self
                    .get_range_location(field.inner_range())
                    .chain(field.definition.line_column_range(&self.schema.sources))
                    .collect(),
            }),
            (_, false) => Ok(()),
        }
    }
}
