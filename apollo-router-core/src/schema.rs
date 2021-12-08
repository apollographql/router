use crate::*;
use apollo_parser::ast;
use std::collections::{HashMap, HashSet};

#[derive(Debug)]
pub struct Schema {
    string: String,
    subtype_map: HashMap<String, HashSet<String>>,
    subgraphs: HashMap<String, String>,
    object_types: HashMap<String, ObjectType>,
    interfaces: HashMap<String, Interface>,
    custom_scalars: HashSet<String>,
    fragments: Fragments,
}

impl std::str::FromStr for Schema {
    type Err = SchemaError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let parser = apollo_parser::Parser::new(s);
        let tree = parser.parse();

        if tree.errors().next().is_some() {
            return Err(SchemaError::ParseErrors(tree.errors().cloned().collect()));
        }

        let document = tree.document();
        let mut subtype_map: HashMap<String, HashSet<String>> = Default::default();
        let mut subgraphs = HashMap::new();

        // the logic of this algorithm is inspired from the npm package graphql:
        // https://github.com/graphql/graphql-js/blob/ac8f0c6b484a0d5dca2dc13c387247f96772580a/src/type/schema.ts#L302-L327
        // https://github.com/graphql/graphql-js/blob/ac8f0c6b484a0d5dca2dc13c387247f96772580a/src/type/schema.ts#L294-L300
        // https://github.com/graphql/graphql-js/blob/ac8f0c6b484a0d5dca2dc13c387247f96772580a/src/type/schema.ts#L215-L263
        for definition in document.definitions() {
            macro_rules! implements_interfaces {
                ($definition:expr) => {{
                    let name = $definition
                        .name()
                        .expect("never optional according to spec; qed")
                        .text()
                        .to_string();

                    for key in $definition
                        .implements_interfaces()
                        .iter()
                        .flat_map(|member_types| member_types.named_types().flat_map(|x| x.name()))
                    {
                        let key = key.text().to_string();
                        let set = subtype_map.entry(key).or_default();
                        set.insert(name.clone());
                    }
                }};
            }

            macro_rules! union_member_types {
                ($definition:expr) => {{
                    let key = $definition
                        .name()
                        .expect("never optional according to spec; qed")
                        .text()
                        .to_string();
                    let set = subtype_map.entry(key).or_default();

                    for name in $definition
                        .union_member_types()
                        .iter()
                        .flat_map(|member_types| member_types.named_types().flat_map(|x| x.name()))
                    {
                        set.insert(name.text().to_string());
                    }
                }};
            }

            match definition {
                // Spec: https://spec.graphql.org/draft/#ObjectTypeDefinition
                ast::Definition::ObjectTypeDefinition(object) => implements_interfaces!(object),
                // Spec: https://spec.graphql.org/draft/#InterfaceTypeDefinition
                ast::Definition::InterfaceTypeDefinition(interface) => {
                    implements_interfaces!(interface)
                }
                // Spec: https://spec.graphql.org/draft/#UnionTypeDefinition
                ast::Definition::UnionTypeDefinition(union) => union_member_types!(union),
                // Spec: https://spec.graphql.org/draft/#sec-Object-Extensions
                ast::Definition::ObjectTypeExtension(object) => implements_interfaces!(object),
                // Spec: https://spec.graphql.org/draft/#sec-Interface-Extensions
                ast::Definition::InterfaceTypeExtension(interface) => {
                    implements_interfaces!(interface)
                }
                // Spec: https://spec.graphql.org/draft/#sec-Union-Extensions
                ast::Definition::UnionTypeExtension(union) => union_member_types!(union),
                // Spec: https://spec.graphql.org/draft/#sec-Enums
                ast::Definition::EnumTypeDefinition(enum_type) => {
                    if enum_type
                        .name()
                        .and_then(|n| n.ident_token())
                        .as_ref()
                        .map(|id| id.text())
                        == Some("join__Graph")
                    {
                        if let Some(enums) = enum_type.enum_values_definition() {
                            for enum_kind in enums.enum_value_definitions() {
                                if let Some(directives) = enum_kind.directives() {
                                    for directive in directives.directives() {
                                        if directive
                                            .name()
                                            .and_then(|n| n.ident_token())
                                            .as_ref()
                                            .map(|id| id.text())
                                            == Some("join__graph")
                                        {
                                            let mut name = None;
                                            let mut url = None;

                                            if let Some(arguments) = directive.arguments() {
                                                for argument in arguments.arguments() {
                                                    let arg_name = argument
                                                        .name()
                                                        .and_then(|n| n.ident_token())
                                                        .as_ref()
                                                        .map(|id| id.text().to_owned());

                                                    let arg_value: Option<String> =
                                                        match argument.value() {
                                                            // We are currently parsing name or url.
                                                            // Both have to be strings.
                                                            Some(ast::Value::StringValue(sv)) => {
                                                                Some(sv.into())
                                                            }
                                                            _ => None,
                                                        };

                                                    match arg_name.as_deref() {
                                                        Some("name") => name = arg_value,
                                                        Some("url") => url = arg_value,
                                                        _ => {}
                                                    };
                                                }
                                            }
                                            if let (Some(name), Some(url)) = (name, url) {
                                                // FIXME: return an error on name collisions
                                                subgraphs.insert(name, url);
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
                _ => {}
            }
        }

        macro_rules! implement_object_type_or_interface_map {
            ($ty:ty, $ast_ty:path, $ast_extension_ty:path $(,)?) => {{
                let mut map = document
                    .definitions()
                    .filter_map(|definition| {
                        if let $ast_ty(definition) = definition {
                            let instance = <$ty>::from(definition);
                            Some((instance.name.clone(), instance))
                        } else {
                            None
                        }
                    })
                    .collect::<HashMap<String, $ty>>();

                document
                    .definitions()
                    .filter_map(|definition| {
                        if let $ast_extension_ty(extension) = definition {
                            Some(<$ty>::from(extension))
                        } else {
                            None
                        }
                    })
                    .for_each(|extension| {
                        if let Some(instance) = map.get_mut(&extension.name) {
                            instance.fields.extend(extension.fields);
                            instance.interfaces.extend(extension.interfaces);
                        } else {
                            failfast_debug!(
                                concat!(
                                    "Extension exists for {:?} but ",
                                    stringify!($ty),
                                    " could not be found."
                                ),
                                extension.name,
                            );
                        }
                    });

                map
            }};
        }

        let object_types = implement_object_type_or_interface_map!(
            ObjectType,
            ast::Definition::ObjectTypeDefinition,
            ast::Definition::ObjectTypeExtension,
        );

        let interfaces = implement_object_type_or_interface_map!(
            Interface,
            ast::Definition::InterfaceTypeDefinition,
            ast::Definition::InterfaceTypeExtension,
        );

        let custom_scalars = document
            .definitions()
            .filter_map(|definition| match definition {
                // Spec: https://spec.graphql.org/draft/#sec-Scalars
                // Spec: https://spec.graphql.org/draft/#sec-Scalar-Extensions
                ast::Definition::ScalarTypeDefinition(definition) => Some(
                    definition
                        .name()
                        .expect("the node Name is not optional in the spec; qed")
                        .text()
                        .to_string(),
                ),
                ast::Definition::ScalarTypeExtension(extension) => Some(
                    extension
                        .name()
                        .expect("the node Name is not optional in the spec; qed")
                        .text()
                        .to_string(),
                ),
                _ => None,
            })
            .collect();

        let fragments = Fragments::from(&document);

        Ok(Self {
            subtype_map,
            string: s.to_owned(),
            subgraphs,
            object_types,
            interfaces,
            custom_scalars,
            fragments,
        })
    }
}

impl Schema {
    pub fn read(path: impl AsRef<std::path::Path>) -> Result<Self, SchemaError> {
        std::fs::read_to_string(path)?.parse()
    }

    pub fn as_str(&self) -> &str {
        &self.string
    }

    pub fn is_subtype(&self, abstract_type: &str, maybe_subtype: &str) -> bool {
        self.subtype_map
            .get(abstract_type)
            .map(|x| x.contains(maybe_subtype))
            .unwrap_or(false)
    }

    pub fn subgraphs(&self) -> impl Iterator<Item = (&String, &String)> {
        self.subgraphs.iter()
    }

    pub(crate) fn fragments(&self) -> &Fragments {
        &self.fragments
    }
}

macro_rules! implement_object_type_or_interface {
    ($visibility:vis $name:ident => $( $ast_ty:ty ),+ $(,)?) => {
        #[derive(Debug)]
        $visibility struct $name {
            name: String,
            fields: HashMap<String, FieldType>,
            interfaces: Vec<String>,
        }

        impl $name {
            fn validate_object(&self, object: &Object, schema: &Schema) -> bool {
                self
                    .fields
                    .iter()
                    .map(|(name, ty)| {
                        let value = object.get(name).unwrap_or(&Value::Null);
                        ty.validate_value(value, schema)
                    })
                    .chain(
                        self
                            .interfaces
                            .iter()
                            .flat_map(|name| schema.interfaces.get(name))
                            .map(|interface| interface.validate_object(object, schema))
                    )
                    .all(std::convert::identity)
            }
        }

        $(
        impl From<$ast_ty> for $name {
            fn from(definition: $ast_ty) -> Self {
                let name = definition
                    .name()
                    .expect("the node Name is not optional in the spec; qed")
                    .text()
                    .to_string();
                let fields = definition
                    .fields_definition()
                    .iter()
                    .flat_map(|x| x.field_definitions())
                    .map(|x| {
                        let name = x
                            .name()
                            .expect("the node Name is not optional in the spec; qed")
                            .text()
                            .to_string();
                        let ty = x
                            .ty()
                            .expect("the node Type is not optional in the spec; qed")
                            .into();
                        (name, ty)
                    })
                    .collect();
                let interfaces = definition
                    .implements_interfaces()
                    .iter()
                    .flat_map(|x| x.named_types())
                    .map(|x| {
                        x.name()
                            .expect("neither Name neither NamedType are optionals; qed")
                            .text()
                            .to_string()
                    })
                    .collect();

                $name {
                    name,
                    fields,
                    interfaces,
                }
            }
        }
        )+
    };
}

// Spec: https://spec.graphql.org/draft/#sec-Objects
// Spec: https://spec.graphql.org/draft/#sec-Object-Extensions
implement_object_type_or_interface!(
    pub(crate) ObjectType =>
    ast::ObjectTypeDefinition,
    ast::ObjectTypeExtension,
);
// Spec: https://spec.graphql.org/draft/#sec-Interfaces
// Spec: https://spec.graphql.org/draft/#sec-Interface-Extensions
implement_object_type_or_interface!(
    Interface =>
    ast::InterfaceTypeDefinition,
    ast::InterfaceTypeExtension,
);

// Primitives are taken from scalars: https://spec.graphql.org/draft/#sec-Scalars
#[derive(Debug)]
pub(crate) enum FieldType {
    Named(String),
    List(Box<FieldType>),
    NonNull(Box<FieldType>),
    String,
    Int,
    Float,
    Id,
    Boolean,
}

impl FieldType {
    pub(crate) fn validate_value(&self, value: &Value, schema: &Schema) -> bool {
        match (self, value) {
            (FieldType::String, Value::String(_)) => true,
            (FieldType::Int, Value::Number(number)) if !number.is_f64() => true,
            (FieldType::Float, Value::Number(number)) if number.is_f64() => true,
            // "The ID scalar type represents a unique identifier, often used to refetch an object
            // or as the key for a cache. The ID type is serialized in the same way as a String;
            // however, it is not intended to be human-readable. While it is often numeric, it
            // should always serialize as a String."
            //
            // In practice it seems Int works too
            (FieldType::Id, Value::String(_) | Value::Number(_)) => true,
            (FieldType::Boolean, Value::Bool(_)) => true,
            (FieldType::List(inner_ty), Value::Array(vec)) => {
                vec.iter().all(|x| inner_ty.validate_value(x, schema))
            }
            (FieldType::NonNull(inner_ty), value) => {
                !value.is_null() && inner_ty.validate_value(value, schema)
            }
            (FieldType::Named(name), _) if schema.custom_scalars.contains(name) => true,
            (FieldType::Named(name), value) if value.is_object() => {
                if let Some(object_ty) = schema.object_types.get(name) {
                    object_ty.validate_object(value.as_object().unwrap(), schema)
                } else {
                    false
                }
            }
            // NOTE: graphql's types are all optional by default
            // TODO add integration test for this
            (_, Value::Null) => true,
            _ => false,
        }
    }
}

impl From<ast::Type> for FieldType {
    // Spec: https://spec.graphql.org/draft/#sec-Type-References
    fn from(ty: ast::Type) -> Self {
        match ty {
            ast::Type::NamedType(named) => named.into(),
            ast::Type::ListType(list) => list.into(),
            ast::Type::NonNullType(non_null) => non_null.into(),
        }
    }
}

impl From<ast::NamedType> for FieldType {
    // Spec: https://spec.graphql.org/draft/#NamedType
    fn from(named: ast::NamedType) -> Self {
        let name = named
            .name()
            .expect("the node Name is not optional in the spec; qed")
            .text()
            .to_string();
        match name.as_str() {
            "String" => Self::String,
            "Int" => Self::Int,
            "Float" => Self::Float,
            "ID" => Self::Id,
            "Boolean" => Self::Boolean,
            _ => Self::Named(name),
        }
    }
}

impl From<ast::ListType> for FieldType {
    // Spec: https://spec.graphql.org/draft/#ListType
    fn from(list: ast::ListType) -> Self {
        Self::List(Box::new(
            list.ty()
                .expect("the node Type is not optional in the spec; qed")
                .into(),
        ))
    }
}

impl From<ast::NonNullType> for FieldType {
    // Spec: https://spec.graphql.org/draft/#NonNullType
    fn from(non_null: ast::NonNullType) -> Self {
        if let Some(list) = non_null.list_type() {
            Self::NonNull(Box::new(list.into()))
        } else if let Some(named) = non_null.named_type() {
            Self::NonNull(Box::new(named.into()))
        } else {
            eprintln!("{:?}", non_null);
            eprintln!("{:?}", non_null.to_string());
            unreachable!("either the NamedType node is provided, either the ListType node; qed")
        }
    }
}

#[derive(Debug)]
pub(crate) struct Fragments {
    map: HashMap<String, Vec<Selection>>,
}

impl From<&ast::Document> for Fragments {
    fn from(document: &ast::Document) -> Self {
        let map = document
            .definitions()
            .filter_map(|definition| match definition {
                // Spec: https://spec.graphql.org/draft/#FragmentDefinition
                ast::Definition::FragmentDefinition(fragment_definition) => {
                    let name = fragment_definition
                        .fragment_name()
                        .expect("the node FragmentName is not optional in the spec; qed")
                        .name()
                        .unwrap()
                        .text()
                        .to_string();
                    let selection_set = fragment_definition
                        .selection_set()
                        .expect("the node SelectionSet is not optional in the spec; qed");

                    Some((name, selection_set.selections().map(Into::into).collect()))
                }
                _ => None,
            })
            .collect();
        Fragments { map }
    }
}

impl Fragments {
    pub(crate) fn get(&self, key: impl AsRef<str>) -> Option<&[Selection]> {
        self.map.get(key.as_ref()).map(|x| x.as_slice())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_subtype() {
        let schema: Schema = "union UnionType = Foo | Bar | Baz".parse().unwrap();
        assert!(schema.is_subtype("UnionType", "Foo"));
        assert!(schema.is_subtype("UnionType", "Bar"));
        assert!(schema.is_subtype("UnionType", "Baz"));
        let schema: Schema = "type ObjectType implements Foo & Bar & Baz { }"
            .parse()
            .unwrap();
        assert!(schema.is_subtype("Foo", "ObjectType"));
        assert!(schema.is_subtype("Bar", "ObjectType"));
        assert!(schema.is_subtype("Baz", "ObjectType"));
        let schema: Schema = "interface InterfaceType implements Foo & Bar & Baz { }"
            .parse()
            .unwrap();
        assert!(schema.is_subtype("Foo", "InterfaceType"));
        assert!(schema.is_subtype("Bar", "InterfaceType"));
        assert!(schema.is_subtype("Baz", "InterfaceType"));
        let schema: Schema = "extend union UnionType = Foo | Bar | Baz".parse().unwrap();
        assert!(schema.is_subtype("UnionType", "Foo"));
        assert!(schema.is_subtype("UnionType", "Bar"));
        assert!(schema.is_subtype("UnionType", "Baz"));
        let schema: Schema = "extend type ObjectType implements Foo & Bar & Baz { }"
            .parse()
            .unwrap();
        assert!(schema.is_subtype("Foo", "ObjectType"));
        assert!(schema.is_subtype("Bar", "ObjectType"));
        assert!(schema.is_subtype("Baz", "ObjectType"));
        let schema: Schema = "extend interface InterfaceType implements Foo & Bar & Baz { }"
            .parse()
            .unwrap();
        assert!(schema.is_subtype("Foo", "InterfaceType"));
        assert!(schema.is_subtype("Bar", "InterfaceType"));
        assert!(schema.is_subtype("Baz", "InterfaceType"));
    }

    #[test]
    fn routing_urls() {
        let schema: Schema = r#"schema
        @core(feature: "https://specs.apollo.dev/core/v0.1"),
        @core(feature: "https://specs.apollo.dev/join/v0.1")
      {
        query: Query
        mutation: Mutation
      }

      enum join__Graph {
        ACCOUNTS @join__graph(name:"accounts" url: "http://localhost:4001/graphql")
        INVENTORY
          @join__graph(name: "inventory", url: "http://localhost:4004/graphql")
        PRODUCTS
        @join__graph(name: "products" url: "http://localhost:4003/graphql")
        REVIEWS @join__graph(name: "reviews" url: "http://localhost:4002/graphql")
      }"#
        .parse()
        .unwrap();

        println!("subgraphs: {:?}", schema.subgraphs);
        assert_eq!(schema.subgraphs.len(), 4);
        assert_eq!(
            schema.subgraphs.get("accounts").map(|s| s.as_str()),
            Some("http://localhost:4001/graphql"),
            "Incorrect url for accounts"
        );

        assert_eq!(
            schema.subgraphs.get("inventory").map(|s| s.as_str()),
            Some("http://localhost:4004/graphql"),
            "Incorrect url for inventory"
        );

        assert_eq!(
            schema.subgraphs.get("products").map(|s| s.as_str()),
            Some("http://localhost:4003/graphql"),
            "Incorrect url for products"
        );

        assert_eq!(
            schema.subgraphs.get("reviews").map(|s| s.as_str()),
            Some("http://localhost:4002/graphql"),
            "Incorrect url for reviews"
        );

        assert_eq!(schema.subgraphs.get("test"), None);
    }
}
