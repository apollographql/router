//! Validate and check semantics of the `@connect(selection:)` argument

use std::fmt::Display;
use std::iter::once;
use std::ops::Range;

use apollo_compiler::Node;
use apollo_compiler::ast::FieldDefinition;
use apollo_compiler::ast::Value;
use apollo_compiler::collections::IndexSet;
use apollo_compiler::parser::LineColumn;
use apollo_compiler::schema::ExtendedType;
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
use crate::connectors::id::ConnectedElement;
use crate::connectors::json_selection::VarPaths;
use crate::connectors::schema_type_ref::SchemaTypeRef;
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
            &self.parsed.local_var_names(),
        )?;

        // Add shape() method to the selection
        let shape = self.parsed.shape();

        match coordinate.element {
            ConnectedElement::Field {
                parent_type,
                field_def,
                ..
            } => {
                let return_type_name = field_def.ty.inner_named_type();
                let Some(return_type_ref) = SchemaTypeRef::new(schema, return_type_name) else {
                    // Scalar or unknown type - no fields to validate
                    return Ok(Vec::new());
                };

                let concrete_type_refs = match return_type_ref.extended() {
                    ExtendedType::Object(_) => vec![return_type_ref],

                    ExtendedType::Interface(interface_type) => schema
                        .types
                        .values()
                        .filter_map(|t| {
                            if let ExtendedType::Object(o) = t {
                                if o.implements_interfaces.contains(&interface_type.name) {
                                    SchemaTypeRef::new(schema, &o.name)
                                } else {
                                    None
                                }
                            } else {
                                None
                            }
                        })
                        .collect(),

                    ExtendedType::Union(union_type) => union_type
                        .members
                        .iter()
                        .filter_map(|member_name| SchemaTypeRef::new(schema, member_name))
                        .collect(),

                    _ => {
                        return Ok(Vec::new());
                    }
                };

                if concrete_type_refs.is_empty() {
                    return Ok(Vec::new());
                }

                let mut validator = SelectionValidator::new(
                    schema,
                    PathPart::Root(parent_type),
                    &self.node,
                    self.coordinate,
                );

                // Add the connector field to the initial path for field connectors
                validator.path.push(PathPart::Field {
                    definition: field_def,
                    ty: parent_type,
                });

                // Clear seen_fields for this connector
                validator.seen_fields.clear();

                // Collect fields resolved from all concrete types
                let mut all_resolved_fields = Vec::new();

                for concrete_type_ref in concrete_type_refs {
                    // Use the new shape-based walk method for each concrete type
                    match validator.walk_selection_with_shape(concrete_type_ref, &shape) {
                        Ok(mut fields) => all_resolved_fields.append(&mut fields),
                        Err(err) => return Err(err),
                    }
                }

                Ok(all_resolved_fields)
            }

            ConnectedElement::Type { type_ref } => {
                if self.parsed.next_subselection().is_none() {
                    // TODO: Validate scalar selections
                    return Ok(Vec::new());
                }

                if !type_ref.is_object() {
                    return Err(Message {
                        code: Code::GraphQLError,
                        message: format!(
                            "Root type definition {} is not an object type",
                            type_ref.name()
                        ),
                        locations: vec![],
                    });
                }

                let mut validator = SelectionValidator::new(
                    schema,
                    PathPart::Root(type_ref),
                    &self.node,
                    self.coordinate,
                );

                // Clear seen_fields for this connector
                validator.seen_fields.clear();

                // Use the new shape-based walk method
                validator.walk_selection_with_shape(type_ref, &shape)
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
    local_var_names: &IndexSet<String>,
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
            let locals_suffix = {
                let local_var_vec = local_var_names.iter().cloned().collect::<Vec<_>>();
                if local_var_vec.is_empty() {
                    "".to_string()
                } else {
                    format!(", {}", local_var_vec.join(", "))
                }
            };

            return Err(Message {
                code: context.error_code(),
                message: format!(
                    "In {coordinate}: unknown variable `{namespace}`, must be one of {available}{locals}",
                    namespace = reference.namespace.namespace.as_str(),
                    available = context.namespaces_joined(),
                    locals = locals_suffix,
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
        current_ty: SchemaTypeRef<'schema>,
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
                                PathPart::Root(ty) => ty.name().as_str(),
                                PathPart::Field { definition, .. } => definition.name.as_str(),
                            })
                            .join("."),
                        type_name = current_ty.name(),
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

    fn get_shape_locations<'a>(
        &self,
        shape_locations: impl IntoIterator<Item = &'a Location>,
    ) -> Vec<Range<LineColumn>> {
        shape_locations
            .into_iter()
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

    fn path_with_root(&self) -> impl Iterator<Item = PathPart<'_>> {
        once(self.root).chain(self.path.iter().copied())
    }

    fn walk_selection_with_shape(
        &mut self,
        type_ref: SchemaTypeRef<'schema>,
        shape: &Shape,
    ) -> Result<Vec<(Name, Name)>, Message> {
        match shape.case() {
            ShapeCase::Object { fields, .. } => {
                for (field_name, field_shape) in fields.iter() {
                    if field_name == "__typename" {
                        continue;
                    }

                    let fields_by_type_name = type_ref.get_fields(field_name.as_str());
                    if fields_by_type_name.is_empty() {
                        return Err(Message {
                            code: Code::SelectedFieldNotFound,
                            message: format!(
                                "{} contains field `{field_name}`, which does not exist on `{}`.",
                                self.coordinate,
                                type_ref.name(),
                            ),
                            locations: self.get_shape_locations(field_shape.locations()),
                        });
                    }

                    for (type_name, field_component) in fields_by_type_name.iter() {
                        let field_def = &field_component.node;

                        // Shadowing the type_ref parameter with a concrete type reference.
                        let Some(type_ref) = SchemaTypeRef::new(self.schema, type_name) else {
                            continue;
                        };

                        // Add current field to path for nested traversal
                        self.path.push(PathPart::Field {
                            definition: field_def,
                            ty: type_ref,
                        });

                        // Check for circular reference after adding field to path
                        let inner_type_name = field_def.ty.inner_named_type();
                        if let Some(field_type_ref) =
                            SchemaTypeRef::new(self.schema, inner_type_name)
                        {
                            self.check_for_circular_reference(field_def, field_type_ref)?;
                        }

                        // Validate field without arguments
                        if !field_def.arguments.is_empty() {
                            let mut locations = self.get_shape_locations(field_shape.locations());
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
                                    parent_type = type_ref.name(),
                                    field_name = field_name,
                                ),
                                locations,
                            });
                        }

                        // Mark the field as seen (shape only contains selected fields)
                        self.seen_fields
                            .push((type_ref.name().clone(), field_def.name.clone()));

                        // Check if this field has subselections (group selection)
                        // Object/Array shapes correspond to GraphQL object/list types with subselections
                        let has_subselection = match field_shape.case() {
                            ShapeCase::Object { .. } => true,
                            ShapeCase::Array { .. } => true,
                            _ => false, // ShapeCase::Name is a placeholder, don't assume subselections
                        };

                        if let Some(field_type_ref) =
                            SchemaTypeRef::new(self.schema, inner_type_name)
                        {
                            match (field_type_ref.extended(), has_subselection) {
                                (ExtendedType::Object(_), true) => {
                                    // Valid: object field with subselection
                                    self.walk_selection_with_shape(field_type_ref, field_shape)?;
                                }
                                (_, true) => {
                                    // Invalid: non-object field with subselection (group selection)
                                    return Err(Message {
                                        code: Code::GroupSelectionIsNotObject,
                                        message: format!(
                                            "{} selects a group `{} {{}}`, but `{}.{}` is of type `{}` which is not an object.",
                                            self.coordinate,
                                            field_name,
                                            type_ref.name(),
                                            field_name,
                                            inner_type_name,
                                        ),
                                        locations: self
                                            .get_shape_locations(field_shape.locations()),
                                    });
                                }
                                _ => {
                                    // Valid: scalar field without subselection, or object field without subselection
                                }
                            }
                        }

                        self.path.pop();
                    }
                }
                Ok(self.seen_fields.clone())
            }
            ShapeCase::One(shapes) => {
                // Try each shape in the union
                let mut all_errors = Vec::new();
                for (index, member_shape) in shapes.iter().enumerate() {
                    match self.walk_selection_with_shape(type_ref, member_shape) {
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

#[derive(Clone, Copy, Debug)]
enum PathPart<'a> {
    // Query, Mutation, Subscription OR an Entity type
    Root(SchemaTypeRef<'a>),
    Field {
        definition: &'a Node<FieldDefinition>,
        ty: SchemaTypeRef<'a>,
    },
}
