use apollo_compiler::Name;
use apollo_compiler::Node;
use apollo_compiler::Schema;
use apollo_compiler::ast::FieldDefinition;
use apollo_compiler::ast::Type;
use apollo_compiler::collections::IndexMap;
use apollo_compiler::collections::IndexSet;
use apollo_compiler::schema::Component;
use apollo_compiler::schema::ExtendedType;
use apollo_compiler::schema::ObjectType;
use shape::Shape;

/// A [`SchemaTypeRef`] is a `Copy`able reference to a named type within a
/// [`Schema`]. Because [`SchemaTypeRef`] holds a `&'schema Schema` reference to
/// the schema in question, it can perform operations like finding all the
/// concrete types of an interface or union, which requires full-schema
/// awareness. Other reference-like types, such as [`ExtendedType`], only
/// provide access to a single element, not the rest of the schema. In fact, as
/// you can get an [`&ExtendedType`] by calling [`SchemaTypeRef::extended`], you
/// can pretty much always safely use a [`SchemaTypeRef`] where you would have
/// previously used an [`ExtendedType`].
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
            ExtendedType::Scalar(s) => match s.name.as_str() {
                "String" => Shape::string([]),
                "Int" => Shape::int([]),
                "Float" => Shape::float([]),
                "Boolean" => Shape::bool(None),
                "ID" => Shape::one([Shape::string([]), Shape::int([])], []),
                // All other custom scalars (including JSON)
                _ => Shape::unknown([]),
            },

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
}
