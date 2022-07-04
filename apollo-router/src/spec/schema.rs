//! GraphQL schema.

use std::collections::HashMap;
use std::collections::HashSet;

use apollo_parser::ast;
use http::Uri;
use itertools::Itertools;
use router_bridge::api_schema;
use sha2::Digest;
use sha2::Sha256;

use crate::error::ParseErrors;
use crate::error::SchemaError;
use crate::json_ext::Object;
use crate::json_ext::Value;
use crate::query_planner::OperationKind;
use crate::*;

/// A GraphQL schema.
#[derive(Debug, Default, Clone)]
pub struct Schema {
    string: String,
    subtype_map: HashMap<String, HashSet<String>>,
    subgraphs: HashMap<String, Uri>,
    pub(crate) object_types: HashMap<String, ObjectType>,
    pub(crate) interfaces: HashMap<String, Interface>,
    pub(crate) input_types: HashMap<String, InputObjectType>,
    pub(crate) custom_scalars: HashSet<String>,
    pub(crate) enums: HashMap<String, HashSet<String>>,
    api_schema: Option<Box<Schema>>,
    pub schema_id: Option<String>,
    root_operations: HashMap<OperationKind, String>,
}

impl std::str::FromStr for Schema {
    type Err = SchemaError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let mut schema = parse(s)?;
        schema.api_schema = Some(Box::new(api_schema(s)?));
        return Ok(schema);

        fn api_schema(schema: &str) -> Result<Schema, SchemaError> {
            let api_schema = format!(
                "{}\n",
                api_schema::api_schema(schema)
                    .map_err(|e| SchemaError::Api(e.to_string()))?
                    .map_err(|e| {
                        SchemaError::Api(e.iter().filter_map(|e| e.message.as_ref()).join(", "))
                    })?
            );

            parse(&api_schema)
        }

        fn parse(schema: &str) -> Result<Schema, SchemaError> {
            let schema_with_introspection = Schema::with_introspection(schema);
            let parser = apollo_parser::Parser::new(&schema_with_introspection);
            let tree = parser.parse();

            // Trace log recursion limit data
            let recursion_limit = tree.recursion_limit();
            tracing::trace!(?recursion_limit, "recursion limit data");

            let errors = tree.errors().cloned().collect::<Vec<_>>();

            if !errors.is_empty() {
                let errors = ParseErrors {
                    raw_schema: schema.to_string(),
                    errors,
                };
                errors.print();
                return Err(SchemaError::Parse(errors));
            }

            let document = tree.document();
            let mut subtype_map: HashMap<String, HashSet<String>> = Default::default();
            let mut subgraphs = HashMap::new();
            let mut root_operations = HashMap::new();

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

                        for key in
                            $definition
                                .implements_interfaces()
                                .iter()
                                .flat_map(|member_types| {
                                    member_types.named_types().flat_map(|x| x.name())
                                })
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

                        for name in
                            $definition
                                .union_member_types()
                                .iter()
                                .flat_map(|member_types| {
                                    member_types.named_types().flat_map(|x| x.name())
                                })
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
                                                                Some(ast::Value::StringValue(
                                                                    sv,
                                                                )) => Some(sv.into()),
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
                                                    if url.is_empty() {
                                                        return Err(
                                                            SchemaError::MissingSubgraphUrl(name),
                                                        );
                                                    }
                                                    if subgraphs
                                                        .insert(
                                                            name.clone(),
                                                            Uri::from_str(&url).map_err(|err| {
                                                                SchemaError::UrlParse(
                                                                    name.clone(),
                                                                    err,
                                                                )
                                                            })?,
                                                        )
                                                        .is_some()
                                                    {
                                                        return Err(SchemaError::Api(format!("must not have several subgraphs with same name '{}'", name)));
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                    // Spec: https://spec.graphql.org/draft/#SchemaDefinition
                    ast::Definition::SchemaDefinition(schema) => {
                        for operation in schema.root_operation_type_definitions() {
                            match (
                                operation.operation_type(),
                                operation.named_type().map(|n| {
                                    n.name()
                                        .expect("the node Name is not optional in the spec; qed")
                                        .text()
                                        .to_string()
                                }),
                            ) {
                                (Some(optype), Some(name)) => {
                                    root_operations.insert(optype.into(), name);
                                }
                                _ => {
                                    return Err(SchemaError::Api("a field on the schema definition should have a name and operation type".to_string()));
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

            macro_rules! implement_input_object_type_or_interface_map {
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

            let input_types = implement_input_object_type_or_interface_map!(
                InputObjectType,
                ast::Definition::InputObjectTypeDefinition,
                ast::Definition::InputObjectTypeExtension,
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

            let enums: HashMap<String, HashSet<String>> = document
                .definitions()
                .filter_map(|definition| match definition {
                    // Spec: https://spec.graphql.org/draft/#sec-Enums
                    ast::Definition::EnumTypeDefinition(definition) => {
                        let name = definition
                            .name()
                            .expect("the node Name is not optional in the spec; qed")
                            .text()
                            .to_string();

                        let enum_values: HashSet<String> = definition
                            .enum_values_definition()
                            .expect(
                                "the node EnumValuesDefinition is not optional in the spec; qed",
                            )
                            .enum_value_definitions()
                            .filter_map(|value| {
                                value.enum_value().map(|val| {
                                    //FIXME: should we check for true/false/null here
                                    // https://spec.graphql.org/draft/#EnumValue
                                    val.name()
                                        .expect("the node Name is not optional in the spec; qed")
                                        .text()
                                        .to_string()
                                })
                            })
                            .collect();

                        Some((name, enum_values))
                    }

                    _ => None,
                })
                .collect();

            let mut hasher = Sha256::new();
            hasher.update(schema.as_bytes());
            let schema_id = Some(format!("{:x}", hasher.finalize()));

            Ok(Schema {
                subtype_map,
                string: schema.to_owned(),
                subgraphs,
                object_types,
                input_types,
                interfaces,
                custom_scalars,
                enums,
                api_schema: None,
                schema_id,
                root_operations,
            })
        }
    }
}

impl Schema {
    /// Read a [`Schema`] from a file at a path.
    pub fn read(path: impl AsRef<std::path::Path>) -> Result<Self, SchemaError> {
        std::fs::read_to_string(path)?.parse()
    }

    /// Extracts a string slice containing the entire [`Schema`].
    pub fn as_str(&self) -> &str {
        &self.string
    }

    pub(crate) fn is_subtype(&self, abstract_type: &str, maybe_subtype: &str) -> bool {
        self.subtype_map
            .get(abstract_type)
            .map(|x| x.contains(maybe_subtype))
            .unwrap_or(false)
    }

    /// Return an iterator over subgraphs that yields the subgraph name and its URL.
    pub fn subgraphs(&self) -> impl Iterator<Item = (&String, &Uri)> {
        self.subgraphs.iter()
    }

    pub fn api_schema(&self) -> &Schema {
        match &self.api_schema {
            Some(schema) => schema,
            None => self,
        }
    }

    pub fn boxed(self) -> Box<Self> {
        Box::new(self)
    }

    fn with_introspection(schema: &str) -> String {
        format!(
            "{}\n{}",
            schema,
            include_str!("introspection_types.graphql")
        )
    }

    pub(crate) fn root_operation_name(&self, kind: OperationKind) -> &str {
        self.root_operations
            .get(&kind)
            .map(|s| s.as_str())
            .unwrap_or_else(|| match kind {
                OperationKind::Query => "Query",
                OperationKind::Mutation => "Mutation",
                OperationKind::Subscription => "SubScription",
            })
    }
}

#[derive(Debug)]
pub(crate) struct InvalidObject;

macro_rules! implement_object_type_or_interface {
    ($visibility:vis $name:ident => $( $ast_ty:ty ),+ $(,)?) => {
        #[derive(Debug, Clone)]
        $visibility struct $name {
            pub(crate) name: String,
            fields: HashMap<String, FieldType>,
            interfaces: Vec<String>,
        }

        impl $name {
            pub(crate) fn field(&self, name: &str) -> Option<&FieldType> {
                self.fields.get(name)
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
    pub(crate) Interface =>
    ast::InterfaceTypeDefinition,
    ast::InterfaceTypeExtension,
);

macro_rules! implement_input_object_type_or_interface {
    ($visibility:vis $name:ident => $( $ast_ty:ty ),+ $(,)?) => {
        #[derive(Debug, Clone)]
        $visibility struct $name {
            name: String,
            fields: HashMap<String, FieldType>,
        }

        impl $name {
            pub(crate) fn validate_object(
                &self,
                object: &Object,
                schema: &Schema,
            ) -> Result<(), InvalidObject> {
                self
                    .fields
                    .iter()
                    .try_for_each(|(name, ty)| {
                        let value = object.get(name.as_str()).unwrap_or(&Value::Null);
                        ty.validate_input_value(value, schema)
                    })
                    .map_err(|_| InvalidObject)
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
                    .input_fields_definition()
                    .iter()
                    .flat_map(|x| x.input_value_definitions())
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

                $name {
                    name,
                    fields,
                }
            }
        }
        )+
    };
}

implement_input_object_type_or_interface!(
    pub(crate) InputObjectType =>
    ast::InputObjectTypeDefinition,
    ast::InputObjectTypeExtension,
);

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use super::*;

    fn with_supergraph_boilerplate(content: &str) -> String {
        format!(
            "{}\n{}",
            r#"
        schema
            @core(feature: "https://specs.apollo.dev/core/v0.1")
            @core(feature: "https://specs.apollo.dev/join/v0.1") {
            query: Query
        }
        directive @core(feature: String!) repeatable on SCHEMA
        directive @join__graph(name: String!, url: String!) on ENUM_VALUE
        enum join__Graph {
            TEST @join__graph(name: "test", url: "http://localhost:4001/graphql")
        }

        "#,
            content
        )
    }

    #[test]
    fn is_subtype() {
        fn gen_schema_types(schema: &str) -> Schema {
            let base_schema = with_supergraph_boilerplate(
                r#"
            type Query {
              me: String
            }
            type Foo {
              me: String
            }
            type Bar {
              me: String
            }
            type Baz {
              me: String
            }
            
            union UnionType2 = Foo | Bar
            "#,
            );
            format!("{}\n{}", base_schema, schema).parse().unwrap()
        }

        fn gen_schema_interfaces(schema: &str) -> Schema {
            let base_schema = with_supergraph_boilerplate(
                r#"
            type Query {
              me: String
            }
            interface Foo {
              me: String
            }
            interface Bar {
              me: String
            }
            interface Baz {
              me: String,
            }

            type ObjectType2 implements Foo & Bar { me: String }
            interface InterfaceType2 implements Foo & Bar { me: String }
            "#,
            );
            format!("{}\n{}", base_schema, schema).parse().unwrap()
        }
        let schema = gen_schema_types("union UnionType = Foo | Bar | Baz");
        assert!(schema.is_subtype("UnionType", "Foo"));
        assert!(schema.is_subtype("UnionType", "Bar"));
        assert!(schema.is_subtype("UnionType", "Baz"));
        let schema =
            gen_schema_interfaces("type ObjectType implements Foo & Bar & Baz { me: String }");
        assert!(schema.is_subtype("Foo", "ObjectType"));
        assert!(schema.is_subtype("Bar", "ObjectType"));
        assert!(schema.is_subtype("Baz", "ObjectType"));
        let schema = gen_schema_interfaces(
            "interface InterfaceType implements Foo & Bar & Baz { me: String }",
        );
        assert!(schema.is_subtype("Foo", "InterfaceType"));
        assert!(schema.is_subtype("Bar", "InterfaceType"));
        assert!(schema.is_subtype("Baz", "InterfaceType"));
        let schema = gen_schema_types("extend union UnionType2 = Baz");
        assert!(schema.is_subtype("UnionType2", "Foo"));
        assert!(schema.is_subtype("UnionType2", "Bar"));
        assert!(schema.is_subtype("UnionType2", "Baz"));
        let schema =
            gen_schema_interfaces("extend type ObjectType2 implements Baz { me2: String }");
        assert!(schema.is_subtype("Foo", "ObjectType2"));
        assert!(schema.is_subtype("Bar", "ObjectType2"));
        assert!(schema.is_subtype("Baz", "ObjectType2"));
        let schema =
            gen_schema_interfaces("extend interface InterfaceType2 implements Baz { me2: String }");
        assert!(schema.is_subtype("Foo", "InterfaceType2"));
        assert!(schema.is_subtype("Bar", "InterfaceType2"));
        assert!(schema.is_subtype("Baz", "InterfaceType2"));
    }

    #[test]
    fn routing_urls() {
        let schema: Schema = r#"
        schema
          @core(feature: "https://specs.apollo.dev/core/v0.1"),
          @core(feature: "https://specs.apollo.dev/join/v0.1")
        {
          query: Query
        }
        type Query {
          me: String
        }
        directive @core(feature: String!) repeatable on SCHEMA
        directive @join__graph(name: String!, url: String!) on ENUM_VALUE

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

        assert_eq!(schema.subgraphs.len(), 4);
        assert_eq!(
            schema
                .subgraphs
                .get("accounts")
                .map(|s| s.to_string())
                .as_deref(),
            Some("http://localhost:4001/graphql"),
            "Incorrect url for accounts"
        );

        assert_eq!(
            schema
                .subgraphs
                .get("inventory")
                .map(|s| s.to_string())
                .as_deref(),
            Some("http://localhost:4004/graphql"),
            "Incorrect url for inventory"
        );

        assert_eq!(
            schema
                .subgraphs
                .get("products")
                .map(|s| s.to_string())
                .as_deref(),
            Some("http://localhost:4003/graphql"),
            "Incorrect url for products"
        );

        assert_eq!(
            schema
                .subgraphs
                .get("reviews")
                .map(|s| s.to_string())
                .as_deref(),
            Some("http://localhost:4002/graphql"),
            "Incorrect url for reviews"
        );

        assert_eq!(schema.subgraphs.get("test"), None);
    }

    #[test]
    fn api_schema() {
        let schema = Schema::from_str(include_str!("../testdata/contract_schema.graphql")).unwrap();
        assert!(schema.object_types["Product"]
            .fields
            .get("inStock")
            .is_some());
        assert!(schema.api_schema.unwrap().object_types["Product"]
            .fields
            .get("inStock")
            .is_none());
    }

    #[test]
    fn schema_id() {
        #[cfg(not(windows))]
        {
            let schema =
                Schema::from_str(include_str!("../testdata/starstuff@current.graphql")).unwrap();

            assert_eq!(
                schema.schema_id,
                Some(
                    "8e2021d131b23684671c3b85f82dfca836908c6a541bbd5c3772c66e7f8429d8".to_string()
                )
            );

            assert_eq!(
                schema.api_schema().schema_id,
                Some(
                    "ba573b479c8b3fa273f439b26b9eda700152341d897f18090d52cd073b15f909".to_string()
                )
            );
        }
    }

    // test for https://github.com/apollographql/federation/pull/1769
    #[test]
    fn inaccessible_on_non_core() {
        match Schema::from_str(include_str!("../testdata/inaccessible_on_non_core.graphql")) {
            Err(SchemaError::Api(s)) => {
                assert_eq!(
                    s,
                    "The supergraph schema failed to produce a valid API schema"
                );
            }
            other => panic!("unexpected schema result: {:?}", other),
        };
    }
}
