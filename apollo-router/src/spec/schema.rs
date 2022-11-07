//! GraphQL schema.

use std::collections::HashMap;
use std::collections::HashSet;
use std::str::FromStr;
use std::sync::Arc;

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
use crate::spec::query::parse_value;
use crate::*;

/// A GraphQL schema.
#[derive(Debug, Default, Clone)]
pub(crate) struct Schema {
    string: Arc<String>,
    subtype_map: HashMap<String, HashSet<String>>,
    subgraphs: HashMap<String, Uri>,
    pub(crate) object_types: HashMap<String, ObjectType>,
    pub(crate) interfaces: HashMap<String, Interface>,
    pub(crate) input_types: HashMap<String, InputObjectType>,
    pub(crate) custom_scalars: HashSet<String>,
    pub(crate) enums: HashMap<String, HashSet<String>>,
    api_schema: Option<Box<Schema>>,
    pub(crate) schema_id: Option<String>,
    root_operations: HashMap<OperationKind, String>,
}

impl Schema {
    pub(crate) fn parse(s: &str, configuration: &Configuration) -> Result<Self, SchemaError> {
        let mut schema = parse(s, configuration)?;
        schema.api_schema = Some(Box::new(api_schema(s, configuration)?));
        return Ok(schema);

        fn api_schema(schema: &str, configuration: &Configuration) -> Result<Schema, SchemaError> {
            let api_schema = format!(
                "{}\n",
                api_schema::api_schema(schema)
                    .map_err(|e| SchemaError::Api(e.to_string()))?
                    .map_err(|e| {
                        SchemaError::Api(e.iter().filter_map(|e| e.message.as_ref()).join(", "))
                    })?
            );

            parse(&api_schema, configuration)
        }

        fn parse(schema: &str, _configuration: &Configuration) -> Result<Schema, SchemaError> {
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
                            match (operation.operation_type(), operation.named_type()) {
                                (Some(optype), Some(name)) => {
                                    let name = name
                                        .name()
                                        .ok_or_else(|| {
                                            SchemaError::Api(
                                                "the node Name is not optional in the spec"
                                                    .to_string(),
                                            )
                                        })?
                                        .text()
                                        .to_string();
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
                                match <$ty>::try_from(definition) {
                                    Ok(instance) => Some(Ok((instance.name.clone(), instance))),
                                    Err(e) => Some(Err(e)),
                                }
                            } else {
                                None
                            }
                        })
                        .collect::<Result<HashMap<String, $ty>, SchemaError>>()?;

                    document
                        .definitions()
                        .filter_map(|definition| {
                            if let $ast_extension_ty(extension) = definition {
                                match <$ty>::try_from(extension) {
                                    Ok(extension) => Some(Ok(extension)),
                                    Err(e) => Some(Err(e)),
                                }
                            } else {
                                None
                            }
                        })
                        .collect::<Result<Vec<_>, _>>()?
                        .into_iter()
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
                                match <$ty>::try_from(definition) {
                                    Ok(instance) => Some(Ok((instance.name.clone(), instance))),
                                    Err(e) => Some(Err(e)),
                                }
                            } else {
                                None
                            }
                        })
                        // todo: impl from
                        .collect::<Result<HashMap<String, $ty>, _>>()
                        .map_err(|e| SchemaError::Api(e.to_string()))?;

                    document
                        .definitions()
                        .filter_map(|definition| {
                            if let $ast_extension_ty(extension) = definition {
                                Some(<$ty>::try_from(extension))
                            } else {
                                None
                            }
                        })
                        // todo: impl from
                        .collect::<Result<Vec<_>, _>>()
                        .map_err(|e| SchemaError::Api(e.to_string()))?
                        .into_iter()
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
                            .ok_or_else(|| {
                                SchemaError::Api(
                                    "the node Name is not optional in the spec".to_string(),
                                )
                            })
                            .map(|name| name.text().to_string()),
                    ),
                    ast::Definition::ScalarTypeExtension(extension) => Some(
                        extension
                            .name()
                            .ok_or_else(|| {
                                SchemaError::Api(
                                    "the node Name is not optional in the spec".to_string(),
                                )
                            })
                            .map(|name| name.text().to_string()),
                    ),
                    _ => None,
                })
                .collect::<Result<_, _>>()?;

            let enums: HashMap<String, HashSet<String>> = document
                .definitions()
                .filter_map(|definition| match definition {
                    // Spec: https://spec.graphql.org/draft/#sec-Enums
                    ast::Definition::EnumTypeDefinition(definition) => {
                        let name = definition
                            .name()
                            .ok_or_else(|| {
                                SchemaError::Api(
                                    "the node Name is not optional in the spec".to_string(),
                                )
                            })
                            .map(|name| name.text().to_string());

                        let enum_values: Result<HashSet<String>, _> = definition
                            .enum_values_definition()
                            .ok_or_else(|| {
                                SchemaError::Api(
                                    "the node EnumValuesDefinition is not optional in the spec"
                                        .to_string(),
                                )
                            })
                            .and_then(|definition| {
                                definition
                                    .enum_value_definitions()
                                    .filter_map(|value| {
                                        value.enum_value().map(|val| {
                                            // No need to check for true/false/null here because it's already checked in apollo-rs
                                            val.name()
                                                .ok_or_else(|| {
                                                    SchemaError::Api(
                                                        "the node Name is not optional in the spec"
                                                            .to_string(),
                                                    )
                                                })
                                                .map(|name| name.text().to_string())
                                        })
                                    })
                                    .collect()
                            });

                        match (name, enum_values) {
                            (Ok(name), Ok(enum_values)) => Some(Ok((name, enum_values))),
                            (Err(schema_error), _) => Some(Err(schema_error)),
                            (_, Err(schema_error)) => Some(Err(schema_error)),
                        }
                    }

                    _ => None,
                })
                .collect::<Result<_, _>>()?;

            let mut hasher = Sha256::new();
            hasher.update(schema.as_bytes());
            let schema_id = Some(format!("{:x}", hasher.finalize()));

            Ok(Schema {
                subtype_map,
                string: Arc::new(schema.to_owned()),
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
    /// Extracts a string containing the entire [`Schema`].
    pub(crate) fn as_string(&self) -> &Arc<String> {
        &self.string
    }

    pub(crate) fn is_subtype(&self, abstract_type: &str, maybe_subtype: &str) -> bool {
        self.subtype_map
            .get(abstract_type)
            .map(|x| x.contains(maybe_subtype))
            .unwrap_or(false)
    }

    /// Return an iterator over subgraphs that yields the subgraph name and its URL.
    pub(crate) fn subgraphs(&self) -> impl Iterator<Item = (&String, &Uri)> {
        self.subgraphs.iter()
    }

    pub(crate) fn api_schema(&self) -> &Schema {
        match &self.api_schema {
            Some(schema) => schema,
            None => self,
        }
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
            .unwrap_or_else(|| kind.as_str())
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
        impl TryFrom<$ast_ty> for $name {
            type Error = SchemaError;

            fn try_from(definition: $ast_ty) -> Result<Self, Self::Error> {
                let name = definition
                    .name()
                    .ok_or_else(|| {
                        SchemaError::Api(
                            "the node Name is not optional in the spec;".to_string(),
                        )
                    })?
                    .text()
                    .to_string();
                let fields = definition
                    .fields_definition()
                    .iter()
                    .flat_map(|x| x.field_definitions())
                    .map(|x| {
                        let name = x
                            .name()

                    .ok_or_else(|| {
                        SchemaError::Api(
                            "the node Name is not optional in the spec;".to_string(),
                        )
                    })?
                    .text()
                    .to_string();
                    let ty = x
                        .ty()
                        .ok_or_else(|| {
                            SchemaError::Api(
                                "the node Type is not optional in the spec;".to_string(),
                            )
                        })?
                        // todo: there must be a better way
                        .try_into().map_err(|e: SpecError|SchemaError::Api(e.to_string()))?;
                        Ok((name, ty))
                    })
                    .collect::<Result<_,_>>()?;
                let interfaces = definition
                    .implements_interfaces()
                    .iter()
                    .flat_map(|x| x.named_types())
                    .map(|x| {
                        Ok(x.name()
                            .ok_or_else(|| {
                                SchemaError::Api(
                                    "neither Name nor NamedType are optionals".to_string(),
                                )
                            })?
                            .text()
                            .to_string())
                    })
                    .collect::<Result<_,_>>()?;

                Ok($name {
                    name,
                    fields,
                    interfaces,
                })
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
            fields: HashMap<String, (FieldType, Option<Value>)>,
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
                    .try_for_each(|(name, (ty, default_value))| {
                        let value = match object.get(name.as_str()) {
                            Some(&Value::Null) | None => default_value.as_ref().unwrap_or(&Value::Null),
                            Some(value) => value,
                        };
                        ty.validate_input_value(value, schema)
                    })
                    .map_err(|_| InvalidObject)
            }
        }

        $(

        impl TryFrom<$ast_ty> for $name {
            type Error = SpecError;
            fn try_from(definition: $ast_ty) -> Result<Self, Self::Error> {
                let name = definition
                    .name()
                    .ok_or_else(|| {
                        SpecError::ParsingError(
                            "the node Name is not optional in the spec".to_string(),
                        )
                    })?
                    .text()
                    .to_string();
                let fields = definition
                    .input_fields_definition()
                    .iter()
                    .flat_map(|x| x.input_value_definitions())
                    .map(|x| {
                        let name = x
                            .name()
                            .ok_or_else(|| {
                                SpecError::ParsingError(
                                    "the node Name is not optional in the spec".to_string(),
                                )
                            })?
                            .text()
                            .to_string();
                        let ty = x
                            .ty()
                            .ok_or_else(|| {
                                SpecError::ParsingError(
                                    "the node Name is not optional in the spec".to_string(),
                                )
                            })?
                            .try_into()?;
                        let default = x.default_value().and_then(|v| v.value()).as_ref().and_then(parse_value);
                        Ok((name, (ty, default)))
                    })
                    .collect::<Result<_,_>>()?;

                Ok($name {
                    name,
                    fields,
                })
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
            let schema = format!("{}\n{}", base_schema, schema);
            Schema::parse(&schema, &Default::default()).unwrap()
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
            let schema = format!("{}\n{}", base_schema, schema);
            Schema::parse(&schema, &Default::default()).unwrap()
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
        let schema = r#"
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
        }"#;
        let schema = Schema::parse(schema, &Default::default()).unwrap();

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
        let schema = include_str!("../testdata/contract_schema.graphql");
        let schema = Schema::parse(schema, &Default::default()).unwrap();
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
            let schema = include_str!("../testdata/starstuff@current.graphql");
            let schema = Schema::parse(schema, &Default::default()).unwrap();

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
        let schema = include_str!("../testdata/inaccessible_on_non_core.graphql");
        match Schema::parse(schema, &Default::default()) {
            Err(SchemaError::Api(s)) => {
                assert_eq!(
                    s,
                    r#"The supergraph schema failed to produce a valid API schema. Caused by:
Input field "InputObject.privateField" is @inaccessible but is used in the default value of "@foo(someArg:)", which is in the API schema.

GraphQL request:42:1
41 |
42 | input InputObject {
   | ^
43 |   someField: String"#
                );
            }
            other => panic!("unexpected schema result: {:?}", other),
        };
    }
}
