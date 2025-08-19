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
use shape::Shape;
use shape::ShapeCase;
use shape::location::Location;
use shape::location::SourceId;

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

        // Add shape() method to the selection
        let shape = self.parsed.shape();

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

                let mut validator = SelectionValidator::new(
                    schema,
                    PathPart::Root(parent_type.as_object_node().ok_or_else(|| Message {
                        code: Code::GraphQLError,
                        message: "Parent type is not an object type".to_string(),
                        locations: vec![],
                    })?),
                    &self.node,
                    self.coordinate,
                );

                // Add the connector field to the initial path for field connectors
                validator.path.push(PathPart::Field {
                    definition: field_def,
                    ty: parent_type.as_object_node().unwrap(),
                });

                // Clear seen_fields for this connector
                validator.seen_fields.clear();

                // Use the new shape-based walk method
                validator.walk_selection_with_shape(sub_selection, &shape, return_type)
            }
            ConnectedElement::Type { type_def } => {
                let Some(sub_selection) = self.parsed.next_subselection() else {
                    // TODO: Validate scalar selections
                    return Ok(Vec::new());
                };

                let mut validator = SelectionValidator::new(
                    schema,
                    PathPart::Root(type_def.as_object_node().ok_or_else(|| Message {
                        code: Code::GraphQLError,
                        message: "Type definition is not an object type".to_string(),
                        locations: vec![],
                    })?),
                    &self.node,
                    self.coordinate,
                );

                // Clear seen_fields for this connector
                validator.seen_fields.clear();

                // Use the new shape-based walk method
                validator.walk_selection_with_shape(
                    sub_selection,
                    &shape,
                    type_def.as_object_node().unwrap(),
                )
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

impl<'schema> SelectionValidator<'schema> {
    fn check_for_circular_reference(
        &self,
        field_def: &Node<FieldDefinition>,
        current_ty: &Node<ObjectType>,
    ) -> Result<(), Message> {
        for (depth, seen_part) in self.path_with_root().enumerate() {
            let (seen_type, ancestor_field) = match seen_part {
                PathPart::Root(root) => (root, None),
                PathPart::Field { ty, definition } => (ty, Some(definition)),
            };

            if seen_type == current_ty {
                return Err(Message {
                    code: Code::CircularReference,
                    message: format!(
                        "Circular reference detected in {coordinate}: type `{type_name}` appears more than once in `{selection_path}`. For more information, see https://go.apollo.dev/connectors/limitations#circular-references",
                        coordinate = &self.coordinate,
                        selection_path = self
                            .path_with_root()
                            .map(|part| match part {
                                PathPart::Root(ty) => ty.name.as_str(),
                                PathPart::Field { definition, .. } => definition.name.as_str(),
                            })
                            .join("."),
                        type_name = current_ty.name,
                    ),
                    // TODO: make a helper function for easier range collection
                    locations: if depth > 1 {
                        ancestor_field
                            .and_then(|def| def.line_column_range(&self.schema.sources))
                            .into_iter()
                            .chain(field_def.line_column_range(&self.schema.sources))
                            .collect()
                    } else {
                        field_def
                            .line_column_range(&self.schema.sources)
                            .into_iter()
                            .collect()
                    },
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

    fn get_shape_locations(&self, shape_locations: &[Location]) -> Vec<Range<LineColumn>> {
        shape_locations
            .iter()
            .filter_map(|location| match &location.source_id {
                SourceId::GraphQL(file_id) => self
                    .schema
                    .sources
                    .get(file_id)
                    .and_then(|source| source.get_line_column_range(location.span.clone())),
                SourceId::Other(_) => {
                    // JSONSelection location - convert to range in the selection string
                    subslice_location(self.node, location.span.clone(), self.schema)
                }
            })
            .collect()
    }

    fn path_with_root(&self) -> impl Iterator<Item = PathPart> {
        once(self.root).chain(self.path.iter().copied())
    }

    fn last_field(&self) -> &PathPart<'_> {
        self.path.last().unwrap_or(&self.root)
    }

    fn walk_selection_with_shape(
        &mut self,
        selection: &SubSelection,
        shape: &Shape,
        ty: &'schema Node<ObjectType>,
    ) -> Result<Vec<(Name, Name)>, Message> {
        // Don't clear seen_fields - accumulate across all levels

        match shape.case() {
            ShapeCase::Object { fields, .. } => {
                // Shape-driven approach: iterate through shape fields only
                for (field_name, field_shape) in fields.iter() {
                    // Skip __typename
                    if field_name == "__typename" {
                        continue;
                    }

                    let field_def = ty.fields.get(field_name.as_str()).ok_or_else(|| Message {
                        code: Code::SelectedFieldNotFound,
                        message: format!(
                            "{} contains field `{field_name}`, which does not exist on `{}`.",
                            self.coordinate, ty.name
                        ),
                        locations: self.get_shape_locations(&field_shape.locations),
                    })?;

                    // Add current field to path for nested traversal
                    self.path.push(PathPart::Field {
                        definition: field_def,
                        ty,
                    });

                    // Check for circular reference after adding field to path
                    let inner_type_name = field_def.ty.inner_named_type();
                    if let Some(field_type) = self.schema.types.get(inner_type_name) {
                        if let ExtendedType::Object(nested_ty) = field_type {
                            self.check_for_circular_reference(field_def, nested_ty)?;
                        }
                    }

                    // Validate field without arguments
                    if !field_def.arguments.is_empty() {
                        let mut locations = self.get_shape_locations(&field_shape.locations);
                        // Also include field definition location from schema
                        if let Some(def_location) =
                            field_def.line_column_range(&self.schema.sources)
                        {
                            locations.push(def_location);
                        }
                        return Err(Message {
                            code: Code::ConnectorsFieldWithArguments,
                            message: format!(
                                "{coordinate} selects field `{parent_type}.{field_name}`, which has arguments. Only fields with a connector can have arguments.",
                                coordinate = &self.coordinate,
                                parent_type = ty.name,
                                field_name = field_name,
                            ),
                            locations,
                        });
                    }

                    // Mark the field as seen (shape only contains selected fields)
                    self.seen_fields
                        .push((ty.name.clone(), field_def.name.clone()));

                    // Check if this field has subselections (group selection)
                    // Object/Array shapes correspond to GraphQL object/list types with subselections
                    let has_subselection = match field_shape.case() {
                        ShapeCase::Object { .. } => true,
                        ShapeCase::Array { .. } => true,
                        _ => false, // ShapeCase::Name is a placeholder, don't assume subselections
                    };
                    let inner_type_name = field_def.ty.inner_named_type();

                    if let Some(field_type) = self.schema.types.get(inner_type_name) {
                        match (field_type, has_subselection) {
                            (ExtendedType::Object(nested_ty), true) => {
                                // Valid: object field with subselection
                                self.walk_selection_with_shape(
                                    selection, // Pass the same selection down - shape drives the traversal
                                    field_shape,
                                    nested_ty,
                                )?;
                            }
                            (_, true) => {
                                // Invalid: non-object field with subselection (group selection)
                                return Err(Message {
                                    code: Code::GroupSelectionIsNotObject,
                                    message: format!(
                                        "{} selects a group `{}{{}}`, but `{}.{}` is of type `{}` which is not an object.",
                                        self.coordinate,
                                        field_name,
                                        ty.name,
                                        field_name,
                                        inner_type_name,
                                    ),
                                    locations: self.get_shape_locations(&field_shape.locations),
                                });
                            }
                            _ => {
                                // Valid: scalar field without subselection, or object field without subselection
                            }
                        }
                    }

                    self.path.pop();
                }
                Ok(self.seen_fields.clone())
            }
            ShapeCase::One(shapes) => {
                // Try each shape in the union
                let mut all_errors = Vec::new();
                for (index, member_shape) in shapes.iter().enumerate() {
                    match self.walk_selection_with_shape(selection, member_shape, ty) {
                        Ok(result) => return Ok(result),
                        Err(e) => all_errors.push((index, e)),
                    }
                }

                // If no shape worked, provide a comprehensive error
                Err(Message {
                    code: Code::GraphQLError,
                    message: format!(
                        "No matching shape found for selection. Attempted {} different shape variations and all failed.",
                        all_errors.len()
                    ),
                    // Optionally, include details from the first error
                    locations: all_errors
                        .first()
                        .map(|(_, e)| e.locations.clone())
                        .unwrap_or_default(),
                })
            }
            _ => Ok(Vec::new()), // Handle other shape cases
        }
    }
}

#[derive(Clone, Debug)]
struct Field<'a> {
    selection: &'a NamedSelection,
    definition: &'a Node<FieldDefinition>,
}

impl<'a> Field<'a> {
    fn next_subselection(&self) -> Option<&'a SubSelection> {
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
                } else if field_name != "__typename" {
                    let mut locations: Vec<_> = self.get_selection_location(selection).collect();
                    // Fallback to directive node location if selection location fails
                    if locations.is_empty() {
                        if let Some(directive_location) = self.node.line_column_range(&self.schema.sources) {
                            locations.push(directive_location);
                        }
                    }
                    results.push(Err(Message {
                        code: Code::SelectedFieldNotFound,
                        message: format!(
                            "{coordinate} contains field `{field_name}`, which does not exist on `{parent_type}`.",
                            coordinate = &self.coordinate,
                            parent_type = group.ty.name,
                        ),
                        locations,
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
                self.check_for_circular_reference(field.definition, object)
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
