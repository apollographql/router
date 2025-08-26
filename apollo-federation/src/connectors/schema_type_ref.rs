use apollo_compiler::Name;
use apollo_compiler::Node;
use apollo_compiler::Schema;
use apollo_compiler::ast::FieldDefinition;
use apollo_compiler::collections::IndexMap;
use apollo_compiler::schema::Component;
use apollo_compiler::schema::ExtendedType;
use apollo_compiler::schema::ObjectType;

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
