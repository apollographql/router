use std::fmt;
use std::fmt::Display;
use std::fmt::Formatter;
use std::hash::Hash;

use apollo_compiler::Name;
use apollo_compiler::Node;
use apollo_compiler::Schema;
use apollo_compiler::ast::FieldDefinition;
use apollo_compiler::ast::NamedType;
use apollo_compiler::collections::IndexMap;
use apollo_compiler::collections::IndexSet;
use apollo_compiler::schema::Component;
use apollo_compiler::schema::ExtendedType;
use apollo_compiler::schema::ObjectType;
use apollo_compiler::schema::Type;
use shape::Shape;

use crate::error::FederationError;
use crate::schema::position::ObjectOrInterfaceFieldDirectivePosition;

#[derive(Debug, Clone, Hash, PartialEq, Eq)]
pub(crate) struct ObjectTypeDefinitionDirectivePosition {
    pub(super) type_name: Name,
    pub(super) directive_name: Name,
    pub(super) directive_index: usize,
}

/// Stores information about the position of the @connect directive, either
/// on a field or on a type.
#[derive(Debug, Clone, Hash, PartialEq, Eq)]
pub(crate) enum ConnectorPosition {
    Field(ObjectOrInterfaceFieldDirectivePosition),
    Type(ObjectTypeDefinitionDirectivePosition),
}

impl ConnectorPosition {
    pub(crate) fn element<'s>(
        &self,
        schema: &'s Schema,
    ) -> Result<ConnectedElement<'s>, FederationError> {
        match self {
            Self::Field(pos) => Ok(ConnectedElement::Field {
                parent_type: SchemaTypeRef::new(schema, pos.field.parent().type_name())
                    .ok_or_else(|| {
                        FederationError::internal("Parent type for connector not found")
                    })?,
                field_def: pos.field.get(schema).map_err(|_| {
                    FederationError::internal("Field definition for connector not found")
                })?,
                parent_category: if self.on_query_type(schema) {
                    ObjectCategory::Query
                } else if self.on_mutation_type(schema) {
                    ObjectCategory::Mutation
                } else {
                    ObjectCategory::Other
                },
            }),
            Self::Type(pos) => Ok(ConnectedElement::Type {
                type_def: SchemaTypeRef::new(schema, &pos.type_name)
                    .ok_or_else(|| FederationError::internal("Type for connector not found"))?,
            }),
        }
    }

    // Only connectors on fields have a parent type (a root type or an entity type)
    pub(crate) fn parent_type_name(&self) -> Option<Name> {
        match self {
            ConnectorPosition::Field(pos) => Some(pos.field.type_name().clone()),
            ConnectorPosition::Type(_) => None,
        }
    }

    // The "base" type is the type returned by the connector. For connectors
    // on fields, this is the field return type. For connectors on types, this
    // is the type itself.
    pub(crate) fn base_type_name(&self, schema: &Schema) -> Option<NamedType> {
        match self {
            ConnectorPosition::Field(_) => self
                .field_definition(schema)
                .map(|field| field.ty.inner_named_type().clone()),
            ConnectorPosition::Type(pos) => Some(pos.type_name.clone()),
        }
    }

    pub(crate) fn field_definition<'s>(
        &self,
        schema: &'s Schema,
    ) -> Option<&'s Component<FieldDefinition>> {
        match self {
            ConnectorPosition::Field(pos) => pos.field.get(schema).ok(),
            ConnectorPosition::Type(_) => None,
        }
    }

    pub(crate) fn coordinate(&self) -> String {
        match self {
            ConnectorPosition::Field(pos) => format!(
                "{}.{}[{}]",
                pos.field.type_name(),
                pos.field.field_name(),
                pos.directive_index,
            ),
            ConnectorPosition::Type(pos) => format!("{}[{}]", pos.type_name, pos.directive_index,),
        }
    }

    pub(crate) fn synthetic_name(&self) -> String {
        match self {
            ConnectorPosition::Field(pos) => format!(
                "{}_{}_{}",
                pos.field.type_name(),
                pos.field.field_name(),
                pos.directive_index,
            ),
            ConnectorPosition::Type(pos) => format!("{}_{}", pos.type_name, pos.directive_index),
        }
    }

    /// The "simple" name of a Connector position without directive index included.
    /// This is useful for error messages where the index could be confusing to users.
    pub(crate) fn simple_name(&self) -> String {
        match self {
            ConnectorPosition::Field(pos) => {
                format!("{}.{}", pos.field.type_name(), pos.field.field_name(),)
            }
            ConnectorPosition::Type(pos) => format!("{}", pos.type_name),
        }
    }

    pub(super) fn on_root_type(&self, schema: &Schema) -> bool {
        self.on_query_type(schema) || self.on_mutation_type(schema)
    }

    fn on_query_type(&self, schema: &Schema) -> bool {
        schema
            .schema_definition
            .query
            .as_ref()
            .is_some_and(|query| match self {
                ConnectorPosition::Field(pos) => *pos.field.type_name() == query.name,
                ConnectorPosition::Type(_) => false,
            })
    }

    fn on_mutation_type(&self, schema: &Schema) -> bool {
        schema
            .schema_definition
            .mutation
            .as_ref()
            .is_some_and(|mutation| match self {
                ConnectorPosition::Field(pos) => *pos.field.type_name() == mutation.name,
                ConnectorPosition::Type(_) => false,
            })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SchemaTypeRef<'schema>(&'schema Schema, &'schema Name, &'schema ExtendedType);

impl<'schema> SchemaTypeRef<'schema> {
    pub(super) fn new(schema: &'schema Schema, name: &str) -> Option<Self> {
        schema
            .types
            .get_full(name)
            .map(|(_index, name, extended)| Self(schema, name, extended))
    }

    #[allow(dead_code)]
    pub(super) fn from_node(
        schema: &'schema Schema,
        node: &'schema Node<ObjectType>,
    ) -> Option<Self> {
        SchemaTypeRef::new(schema, node.name.as_str())
    }

    pub(super) fn shape(&self) -> Shape {
        self.shape_with_visited(&mut IndexSet::default())
    }

    fn shape_with_visited(&self, visited: &mut IndexSet<String>) -> Shape {
        let type_name = self.name().to_string();
        if visited.contains(&type_name) {
            return Shape::name(&type_name, []);
        }
        visited.insert(type_name.clone());

        let result = match self.extended() {
            ExtendedType::Object(o) => {
                // Check if we're being called from an abstract type (interface or union)
                let from_abstract_parent = visited
                    .iter()
                    .last()
                    .and_then(|parent_name| SchemaTypeRef::new(self.0, parent_name))
                    .map(|parent_ref| parent_ref.is_abstract())
                    .unwrap_or(false);

                // Generate __typename field based on context
                let typename_shape = if from_abstract_parent {
                    // Required typename when accessed via interface/union
                    Shape::string_value(self.name().as_str(), [])
                } else {
                    // Optional typename when accessed directly
                    Shape::one(
                        [Shape::string_value(self.name().as_str(), []), Shape::none()],
                        [],
                    )
                };

                // Build fields map with __typename first
                let mut fields = Shape::empty_map();
                fields.insert("__typename".to_string(), typename_shape);

                // Add all the object's declared fields
                for (name, field) in &o.fields {
                    fields.insert(
                        name.to_string(),
                        self.shape_from_type_with_visited(&field.ty, visited),
                    );
                }

                Shape::record(fields, [])
            }
            ExtendedType::Scalar(_) => Shape::unknown([]),

            ExtendedType::Enum(e) => {
                // Enums are unions of their string values
                Shape::one(
                    e.values
                        .keys()
                        .map(|value| Shape::string_value(value.as_str(), [])),
                    [],
                )
            }
            ExtendedType::Interface(i) => Shape::one(
                self.0.types.values().filter_map(|extended_type| {
                    if let ExtendedType::Object(object_type) = extended_type {
                        if object_type.implements_interfaces.contains(&i.name) {
                            SchemaTypeRef::new(self.0, object_type.name.as_str())
                                .map(|type_ref| type_ref.shape_with_visited(visited))
                        } else {
                            None
                        }
                    } else {
                        None
                    }
                }),
                [],
            ),
            ExtendedType::Union(u) => Shape::one(
                u.members.iter().filter_map(|member_name| {
                    SchemaTypeRef::new(self.0, member_name.as_str())
                        .map(|type_ref| type_ref.shape_with_visited(visited))
                }),
                [],
            ),
            ExtendedType::InputObject(i) => Shape::record(
                i.fields
                    .iter()
                    .map(|(name, field)| {
                        (
                            name.to_string(),
                            self.shape_from_type_with_visited(&field.ty, visited),
                        )
                    })
                    .collect(),
                [],
            ),
        };

        visited.swap_remove(&type_name);
        result
    }

    /// Helper to make a shape nullable (can be null)
    fn nullable(&self, shape: Shape) -> Shape {
        Shape::one([shape, Shape::null([])], [])
    }

    #[allow(dead_code)]
    pub(super) fn shape_from_type(&self, ty: &Type) -> Shape {
        self.shape_from_type_with_visited(ty, &mut IndexSet::default())
    }

    fn shape_from_type_with_visited(&self, ty: &Type, visited: &mut IndexSet<String>) -> Shape {
        let inner_type_name = ty.inner_named_type();
        let base_shape = if visited.contains(inner_type_name.as_str()) {
            // Avoid infinite recursion for circular references
            Shape::name(inner_type_name.as_str(), [])
        } else if let Some(named_type) = SchemaTypeRef::new(self.0, inner_type_name.as_str()) {
            named_type.shape_with_visited(visited)
        } else {
            Shape::name(inner_type_name.as_str(), [])
        };

        match ty {
            Type::Named(_) => self.nullable(base_shape),
            Type::NonNullNamed(_) => base_shape,
            Type::List(inner) => self.nullable(Shape::list(
                self.shape_from_type_with_visited(inner, visited),
                [],
            )),
            Type::NonNullList(inner) => {
                Shape::list(self.shape_from_type_with_visited(inner, visited), [])
            }
        }
    }

    pub(super) fn as_object_node(&self) -> Option<&'schema Node<ObjectType>> {
        if let ExtendedType::Object(obj) = self.2 {
            Some(obj)
        } else {
            None
        }
    }

    #[allow(dead_code)]
    pub(super) fn schema(&self) -> &'schema Schema {
        self.0
    }

    pub(super) fn name(&self) -> &'schema Name {
        self.1
    }

    pub(super) fn extended(&self) -> &'schema ExtendedType {
        self.2
    }

    #[allow(dead_code)]
    pub(super) fn is_object(&self) -> bool {
        self.2.is_object()
    }

    #[allow(dead_code)]
    pub(super) fn is_interface(&self) -> bool {
        self.2.is_interface()
    }

    #[allow(dead_code)]
    pub(super) fn is_union(&self) -> bool {
        self.2.is_union()
    }

    #[allow(dead_code)]
    pub(super) fn is_abstract(&self) -> bool {
        self.is_interface() || self.is_union()
    }

    #[allow(dead_code)]
    pub(super) fn is_input_object(&self) -> bool {
        self.2.is_input_object()
    }

    #[allow(dead_code)]
    pub(super) fn is_enum(&self) -> bool {
        self.2.is_enum()
    }

    #[allow(dead_code)]
    pub(super) fn is_scalar(&self) -> bool {
        self.2.is_scalar()
    }

    #[allow(dead_code)]
    pub(super) fn is_built_in(&self) -> bool {
        self.2.is_built_in()
    }

    pub(super) fn get_fields(
        &self,
        field_name: &str,
    ) -> IndexMap<String, &'schema Component<FieldDefinition>> {
        self.0
            .types
            .get(self.1)
            .into_iter()
            .flat_map(|ty| match ty {
                ExtendedType::Object(o) => {
                    let mut map = IndexMap::default();
                    if let Some(field_def) = o.fields.get(field_name) {
                        map.insert(o.name.to_string(), field_def);
                    }
                    map
                }

                ExtendedType::Interface(i) => {
                    let mut map = IndexMap::default();
                    if let Some(implementers) = self.0.implementers_map().get(i.name.as_str()) {
                        for obj_name in &implementers.objects {
                            if let Some(impl_obj) = SchemaTypeRef::new(self.0, obj_name.as_str()) {
                                map.extend(impl_obj.get_fields(field_name).into_iter());
                            }
                        }
                        for iface_name in &implementers.interfaces {
                            if let Some(impl_iface) =
                                SchemaTypeRef::new(self.0, iface_name.as_str())
                            {
                                map.extend(impl_iface.get_fields(field_name).into_iter());
                            }
                        }
                    }
                    map
                }

                ExtendedType::Union(u) => u
                    .members
                    .iter()
                    .flat_map(|m| {
                        SchemaTypeRef::new(self.0, m.name.as_str())
                            .map(|type_ref| type_ref.get_fields(field_name))
                            .unwrap_or_default()
                    })
                    .collect(),

                _ => IndexMap::default(),
            })
            .collect()
    }

    #[allow(dead_code)]
    pub(super) fn get_type(&self, ty: &Type) -> Option<SchemaTypeRef<'schema>> {
        let inner_name = ty.inner_named_type().as_str();
        SchemaTypeRef::new(self.schema(), inner_name)
    }
}

/// Reifies the connector position into schema definitions
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ConnectedElement<'schema> {
    Field {
        parent_type: SchemaTypeRef<'schema>,
        field_def: &'schema Component<FieldDefinition>,
        parent_category: ObjectCategory,
    },
    Type {
        type_def: SchemaTypeRef<'schema>,
    },
}

impl ConnectedElement<'_> {
    pub(super) fn base_type_name(&self) -> NamedType {
        match self {
            ConnectedElement::Field { field_def, .. } => field_def.ty.inner_named_type().clone(),
            ConnectedElement::Type { type_def } => type_def.name().clone(),
        }
    }

    pub(super) fn is_root_type(&self, schema: &Schema) -> bool {
        self.is_query_type(schema) || self.is_mutation_type(schema)
    }

    fn is_query_type(&self, schema: &Schema) -> bool {
        schema
            .schema_definition
            .query
            .as_ref()
            .is_some_and(|query| match self {
                ConnectedElement::Field { .. } => false,
                ConnectedElement::Type { type_def } => type_def.name() == query.name.as_str(),
            })
    }

    fn is_mutation_type(&self, schema: &Schema) -> bool {
        schema
            .schema_definition
            .mutation
            .as_ref()
            .is_some_and(|mutation| match self {
                ConnectedElement::Field { .. } => false,
                ConnectedElement::Type { type_def } => type_def.name() == mutation.name.as_str(),
            })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ObjectCategory {
    Query,
    Mutation,
    Other,
}

impl Display for ConnectedElement<'_> {
    fn fmt(&self, f: &mut Formatter) -> fmt::Result {
        match self {
            Self::Field {
                parent_type,
                field_def,
                ..
            } => write!(f, "{}.{}", parent_type.name(), field_def.name),
            Self::Type { type_def } => write!(f, "{}", type_def.name()),
        }
    }
}
