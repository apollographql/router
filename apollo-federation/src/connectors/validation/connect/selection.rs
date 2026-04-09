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
use crate::connectors::ConnectSpec;
use crate::connectors::JSONSelection;
use crate::connectors::Namespace;
use crate::connectors::PathSelection;
use crate::connectors::SubSelection;
use crate::connectors::expand::visitors::FieldVisitor;
use crate::connectors::expand::visitors::GroupVisitor;
use crate::connectors::id::ConnectedElement;
use crate::connectors::json_selection::NamedSelection;
use crate::connectors::json_selection::Ranged;
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

        // Fork based on ConnectSpec version:
        //
        // - v0.1/v0.2/v0.3: Use legacy visitor-based validation
        //   (behaviorally frozen for forwards/backwards compatibility)
        //   Note: in v0.1 and v0.2, all -> method result shapes were
        //   Unknown, so the shape-based validation would not work at
        //   all. In v0.3, -> method shape checking was fully enabled,
        //   but v0.3 shipped before the shape-based validation code was
        //   ready, so we still use the legacy path for v0.3, even
        //   though it could in principle swap over to shape-based
        //   validation at some point if need arises.
        //
        // - v0.4+: Use shape-driven validation (actively maintained)
        if schema.connect_link.spec < ConnectSpec::V0_4 {
            // Legacy path for v0.1/v0.2 compatibility
            self.type_check_legacy(schema)
        } else {
            // Modern path for v0.3+
            self.type_check_shape_based(schema)
        }
    }

    /// Shape-based type checking for v0.3+ (actively maintained)
    fn type_check_shape_based(self, schema: &SchemaInfo) -> Result<Vec<(Name, Name)>, Message> {
        let coordinate = self.coordinate.connect;
        // Add shape() method to the selection
        let shape = self.parsed.shape();

        match coordinate.element {
            ConnectedElement::Field {
                parent_type: _,
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
                    &self.node,
                    self.coordinate,
                );

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

    /// Legacy visitor-based type checking for v0.1/v0.2 (frozen, will be removed)
    fn type_check_legacy(self, schema: &SchemaInfo) -> Result<Vec<(Name, Name)>, Message> {
        let coordinate = self.coordinate.connect;

        match coordinate.element {
            ConnectedElement::Field {
                parent_type,
                field_def,
                ..
            } => {
                // Get the return type's object node
                let field_type_name = field_def.ty.inner_named_type();
                let Some(return_type) = schema.types.get(field_type_name) else {
                    // TODO: Validate scalar return types
                    return Ok(Vec::new());
                };
                let ExtendedType::Object(return_type_obj) = return_type else {
                    return Ok(Vec::new());
                };

                let Some(sub_selection) = self.parsed.next_subselection() else {
                    // TODO: Validate scalar selections
                    return Ok(Vec::new());
                };

                // Get parent type's object node
                let ExtendedType::Object(parent_type_obj) = parent_type.extended() else {
                    return Err(Message {
                        code: Code::GraphQLError,
                        message: "Parent type is not an object type".to_string(),
                        locations: vec![],
                    });
                };

                let group = LegacyGroup {
                    selection: sub_selection,
                    ty: return_type_obj,
                    definition: Some(field_def),
                };

                LegacySelectionValidator::new(
                    schema,
                    LegacyPathPart::Root(parent_type_obj),
                    &self.node,
                    self.coordinate,
                )
                .walk(group)
                .map(|validator| validator.seen_fields)
            }

            ConnectedElement::Type { type_ref } => {
                let Some(sub_selection) = self.parsed.next_subselection() else {
                    // TODO: Validate scalar selections
                    return Ok(Vec::new());
                };

                let ExtendedType::Object(type_obj) = type_ref.extended() else {
                    return Err(Message {
                        code: Code::GraphQLError,
                        message: "Type definition is not an object type".to_string(),
                        locations: vec![],
                    });
                };

                let group = LegacyGroup {
                    selection: sub_selection,
                    ty: type_obj,
                    definition: None,
                };

                LegacySelectionValidator::new(
                    schema,
                    LegacyPathPart::Root(type_obj),
                    &self.node,
                    self.coordinate,
                )
                .walk(group)
                .map(|validator| validator.seen_fields)
            }
        }
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
    node: &'schema Node<Value>,
    coordinate: SelectionCoordinate<'schema>,
    seen_fields: Vec<(Name, Name)>,
}

impl<'schema> SelectionValidator<'schema> {
    const fn new(
        schema: &'schema SchemaInfo<'schema>,
        node: &'schema Node<Value>,
        coordinate: SelectionCoordinate<'schema>,
    ) -> Self {
        Self {
            schema,
            node,
            coordinate,
            seen_fields: Vec::new(),
        }
    }
}

impl<'schema> SelectionValidator<'schema> {
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

                        let inner_type_name = field_def.ty.inner_named_type();

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
                    }
                }
                Ok(self.seen_fields.clone())
            }
            ShapeCase::One(shapes) => {
                // For union shapes, we need to collect seen fields from ALL branches
                // that succeed, not just the first one. This is because different
                // branches may resolve different fields (e.g., in a ->match expression
                // that returns different object types for interface implementations).
                let mut all_seen_fields = Vec::new();
                let mut all_errors = Vec::new();

                for (index, member_shape) in shapes.iter().enumerate() {
                    match self.walk_selection_with_shape(type_ref, member_shape) {
                        Ok(mut fields) => all_seen_fields.append(&mut fields),
                        Err(e) => all_errors.push((index, e)),
                    }
                }

                // If at least one branch succeeded, return all collected fields
                if !all_seen_fields.is_empty() || all_errors.is_empty() {
                    Ok(all_seen_fields)
                } else {
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
            }
            _ => Ok(Vec::new()), // Handle other shape cases
        }
    }
}

/// Legacy validation structures for v0.1/v0.2 (frozen, will be removed)
/// These mirror the old visitor-based validation approach.
use apollo_compiler::schema::ObjectType;

struct LegacySelectionValidator<'schema> {
    schema: &'schema SchemaInfo<'schema>,
    root: LegacyPathPart<'schema>,
    path: Vec<LegacyPathPart<'schema>>,
    node: &'schema Node<Value>,
    coordinate: SelectionCoordinate<'schema>,
    seen_fields: Vec<(Name, Name)>,
}

impl<'schema> LegacySelectionValidator<'schema> {
    const fn new(
        schema: &'schema SchemaInfo<'schema>,
        root: LegacyPathPart<'schema>,
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

    fn check_for_circular_reference(
        &self,
        field: LegacyField,
        object: &Node<ObjectType>,
    ) -> Result<(), Message> {
        for (depth, seen_part) in self.path_with_root().enumerate() {
            let (seen_type, ancestor_field) = match seen_part {
                LegacyPathPart::Root(root) => (root, None),
                LegacyPathPart::Field { ty, definition } => (ty, Some(definition)),
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
                    locations: self
                        .get_range_location(field.inner_range())
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

    fn path_with_root(&self) -> impl Iterator<Item = LegacyPathPart<'_>> {
        once(self.root).chain(self.path.iter().copied())
    }

    fn path_string(&self, tail: &FieldDefinition) -> String {
        self.path_with_root()
            .map(|part| match part {
                LegacyPathPart::Root(ty) => ty.name.as_str(),
                LegacyPathPart::Field { definition, .. } => definition.name.as_str(),
            })
            .chain(once(tail.name.as_str()))
            .join(".")
    }

    fn last_field(&self) -> &LegacyPathPart<'_> {
        self.path.last().unwrap_or(&self.root)
    }
}

#[derive(Clone, Copy, Debug)]
struct LegacyField<'schema> {
    selection: &'schema NamedSelection,
    definition: &'schema Node<FieldDefinition>,
}

impl<'schema> LegacyField<'schema> {
    fn next_subselection(&self) -> Option<&'schema SubSelection> {
        self.selection.next_subselection()
    }

    fn inner_range(&self) -> Option<Range<usize>> {
        self.selection.range()
    }
}

#[derive(Clone, Copy, Debug)]
enum LegacyPathPart<'a> {
    Root(&'a Node<ObjectType>),
    Field {
        definition: &'a Node<FieldDefinition>,
        ty: &'a Node<ObjectType>,
    },
}

impl LegacyPathPart<'_> {
    const fn ty(&self) -> &Node<ObjectType> {
        match self {
            LegacyPathPart::Root(ty) => ty,
            LegacyPathPart::Field { ty, .. } => ty,
        }
    }
}

#[derive(Clone, Debug)]
struct LegacyGroup<'schema> {
    selection: &'schema SubSelection,
    ty: &'schema Node<ObjectType>,
    definition: Option<&'schema Node<FieldDefinition>>,
}

impl<'schema> GroupVisitor<LegacyGroup<'schema>, LegacyField<'schema>>
    for LegacySelectionValidator<'schema>
{
    fn try_get_group_for_field(
        &self,
        field: &LegacyField<'schema>,
    ) -> Result<Option<LegacyGroup<'schema>>, Self::Error> {
        let Some(selection) = field.next_subselection() else {
            return Ok(None);
        };
        let Some(ty) = self
            .schema
            .get_object(field.definition.ty.inner_named_type())
        else {
            return Ok(None);
        };
        Ok(Some(LegacyGroup {
            selection,
            ty,
            definition: Some(field.definition),
        }))
    }

    fn enter_group(
        &mut self,
        group: &LegacyGroup<'schema>,
    ) -> Result<Vec<LegacyField<'schema>>, Self::Error> {
        if let Some(definition) = group.definition {
            self.path.push(LegacyPathPart::Field {
                definition,
                ty: group.ty,
            });
        }

        group.selection.selections_iter().flat_map(|selection| {
            let mut results = Vec::new();
            for field_name in selection.names() {
                if let Some(definition) = group.ty.fields.get(field_name) {
                    results.push(Ok(LegacyField {
                        selection,
                        definition,
                    }));
                } else if field_name != "__typename" {
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

impl<'schema> FieldVisitor<LegacyField<'schema>> for LegacySelectionValidator<'schema> {
    type Error = Message;

    fn visit(&mut self, field: LegacyField<'schema>) -> Result<(), Self::Error> {
        let field_name = field.definition.name.as_str();
        let type_name = field.definition.ty.inner_named_type();
        let coordinate = self.coordinate;
        let field_type = self.schema.types.get(type_name).ok_or_else(|| Message {
            code: Code::GraphQLError,
            message: format!(
                "{coordinate} contains field `{field_name}`, which has undefined type `{type_name}`.",
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
