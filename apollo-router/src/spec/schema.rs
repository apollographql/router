//! GraphQL schema.

use std::collections::HashMap;
use std::collections::HashSet;
use std::str::FromStr;
use std::sync::Arc;

use apollo_compiler::hir;
use apollo_compiler::ApolloCompiler;
use apollo_compiler::AstDatabase;
use apollo_compiler::HirDatabase;
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
use crate::spec::query::parse_hir_value;
use crate::spec::query::parse_value;
use crate::spec::FieldType;
use crate::spec::SpecError;
use crate::Configuration;

/// A GraphQL schema.
pub(crate) struct Schema {
    pub(crate) raw_sdl: Arc<String>,
    pub(crate) type_system: Arc<apollo_compiler::hir::TypeSystem>,
    subtype_map: Arc<HashMap<String, HashSet<String>>>,
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

pub(crate) fn sorted_map<K, V>(
    f: &mut std::fmt::Formatter<'_>,
    indent: &str,
    name: &str,
    map: &HashMap<K, V>,
) -> std::fmt::Result
where
    K: std::fmt::Debug + Ord,
    V: std::fmt::Debug,
{
    writeln!(f, "{indent}{name}:")?;
    for (k, v) in map.iter().sorted_by_key(|&(k, _v)| k) {
        writeln!(f, "{indent}  {k:?}: {v:#?}")?;
    }
    Ok(())
}

/// YAML-like representation with sorted hashmap/sets, more amenable to diffing
impl std::fmt::Debug for Schema {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        fn sorted_map_of_sets(
            f: &mut std::fmt::Formatter<'_>,
            name: &str,
            map: &HashMap<String, HashSet<String>>,
        ) -> std::fmt::Result {
            writeln!(f, "  {name}:")?;
            for (k, set) in map.iter().sorted_by_key(|&(k, _set)| k) {
                writeln!(f, "    {k:?}:")?;
                for v in set.iter().sorted() {
                    writeln!(f, "      {v:?}")?;
                }
            }
            Ok(())
        }

        // Make sure we consider all fields
        let Schema {
            type_system: _,
            raw_sdl,
            subtype_map,
            subgraphs,
            object_types,
            interfaces,
            input_types,
            custom_scalars,
            enums,
            api_schema,
            schema_id,
            root_operations,
        } = self;
        writeln!(f, "Schema:")?;
        writeln!(f, "  raw_sdl: {raw_sdl:?}")?;
        let root = root_operations
            .iter()
            .map(|(k, v)| (format!("{k:?}"), v))
            .collect();
        sorted_map(f, "  ", "root_operations", &root)?;
        writeln!(f, "  object_types:")?;
        for (k, v) in object_types.iter().sorted_by_key(|&(k, _v)| k) {
            let ObjectType {
                name: _,
                fields,
                interfaces,
            } = v;
            writeln!(f, "    {k:?}:")?;
            writeln!(f, "      interfaces: {interfaces:?}")?;
            sorted_map(f, "      ", "fields", fields)?
        }
        writeln!(f, "  interfaces:")?;
        for (k, v) in interfaces.iter().sorted_by_key(|&(k, _v)| k) {
            let Interface {
                name: _,
                fields,
                interfaces,
            } = v;
            writeln!(f, "    {k:?}:")?;
            writeln!(f, "      interfaces: {interfaces:?}")?;
            sorted_map(f, "      ", "fields", fields)?
        }
        writeln!(f, "  input_types:")?;
        for (k, v) in input_types.iter().sorted_by_key(|&(k, _v)| k) {
            let InputObjectType { name: _, fields } = v;
            writeln!(f, "    {k:?}:")?;
            sorted_map(f, "      ", "fields", fields)?
        }
        let scalars = custom_scalars.iter().sorted().collect::<Vec<_>>();
        writeln!(f, "  custom_scalars: {scalars:?}")?;
        sorted_map_of_sets(f, "enums", enums)?;
        sorted_map_of_sets(f, "subtype_map", subtype_map)?;
        sorted_map(f, "  ", "subgraphs", subgraphs)?;
        writeln!(f, "  schema_id: {schema_id:?}")?;
        writeln!(f, "  api_schema: {api_schema:?}")?;
        Ok(())
    }
}

fn make_api_schema(schema: &str) -> Result<String, SchemaError> {
    let s = api_schema::api_schema(schema)
        .map_err(|e| SchemaError::Api(e.to_string()))?
        .map_err(|e| SchemaError::Api(e.iter().filter_map(|e| e.message.as_ref()).join(", ")))?;
    Ok(format!("{s}\n"))
}

impl Schema {
    pub(crate) fn parse(s: &str, configuration: &Configuration) -> Result<Self, SchemaError> {
        Self::parse_with_hir(s, configuration)
    }

    pub(crate) fn parse_with_hir(
        s: &str,
        configuration: &Configuration,
    ) -> Result<Self, SchemaError> {
        let mut schema = parse(s, configuration)?;
        schema.api_schema = Some(Box::new(parse(&make_api_schema(s)?, configuration)?));
        return Ok(schema);

        fn parse(schema: &str, _configuration: &Configuration) -> Result<Schema, SchemaError> {
            let mut compiler = ApolloCompiler::new();
            compiler.add_type_system(
                include_str!("introspection_types.graphql"),
                "introspection_types.graphql",
            );
            let id = compiler.add_type_system(schema, "schema.graphql");

            let ast = compiler.db.ast(id);

            // Trace log recursion limit data
            let recursion_limit = ast.recursion_limit();
            tracing::trace!(?recursion_limit, "recursion limit data");

            // TODO: run full compiler-based validation instead?
            let errors = ast.errors().cloned().collect::<Vec<_>>();
            if !errors.is_empty() {
                let errors = ParseErrors {
                    raw_schema: schema.to_string(),
                    errors,
                };
                errors.print();
                return Err(SchemaError::Parse(errors));
            }

            fn as_string(value: &hir::Value) -> Option<&String> {
                if let hir::Value::String(string) = value {
                    Some(string)
                } else {
                    None
                }
            }

            let mut subgraphs = HashMap::new();
            // TODO: error if not found?
            if let Some(join_enum) = compiler.db.find_enum_by_name("join__Graph".into()) {
                for (name, url) in join_enum
                    .enum_values_definition()
                    .iter()
                    .filter_map(|value| {
                        let join_directive = value
                            .directives()
                            .iter()
                            .find(|directive| directive.name() == "join__graph")?;
                        let name = as_string(join_directive.argument_by_name("name")?)?;
                        let url = as_string(join_directive.argument_by_name("url")?)?;
                        Some((name, url))
                    })
                {
                    if url.is_empty() {
                        return Err(SchemaError::MissingSubgraphUrl(name.clone()));
                    }
                    let url = Uri::from_str(url)
                        .map_err(|err| SchemaError::UrlParse(name.clone(), err))?;
                    if subgraphs.insert(name.clone(), url).is_some() {
                        return Err(SchemaError::Api(format!(
                            "must not have several subgraphs with same name '{name}'"
                        )));
                    }
                }
            }

            let object_types: HashMap<_, _> = compiler
                .db
                .object_types()
                .iter()
                .map(|(name, def)| (name.clone(), (&**def).into()))
                .collect();

            let interfaces: HashMap<_, _> = compiler
                .db
                .interfaces()
                .iter()
                .map(|(name, def)| (name.clone(), (&**def).into()))
                .collect();

            let input_types: HashMap<_, _> = compiler
                .db
                .input_objects()
                .iter()
                .map(|(name, def)| (name.clone(), (&**def).into()))
                .collect();

            let enums = compiler
                .db
                .enums()
                .iter()
                .map(|(name, def)| {
                    let values = def
                        .enum_values_definition()
                        .iter()
                        .map(|value| value.enum_value().to_owned())
                        .collect();
                    (name.clone(), values)
                })
                .collect();

            let root_operations = compiler
                .db
                .schema()
                .root_operation_type_definition()
                .iter()
                .filter(|def| def.loc().is_some()) // exclude implict operations
                .map(|def| {
                    (
                        def.operation_ty().into(),
                        if let hir::Type::Named { name, .. } = def.named_type() {
                            name.clone()
                        } else {
                            // FIXME: hir::RootOperationTypeDefinition should contain
                            // the name directly, not a `Type` enum value which happens to always
                            // be the `Named` variant.
                            unreachable!()
                        },
                    )
                })
                .collect();

            let custom_scalars = compiler
                .db
                .scalars()
                .iter()
                .filter(|(_name, def)| !def.is_built_in())
                .map(|(name, _def)| name.clone())
                .collect();

            let mut hasher = Sha256::new();
            hasher.update(schema.as_bytes());
            let schema_id = Some(format!("{:x}", hasher.finalize()));

            Ok(Schema {
                raw_sdl: Arc::new(schema.into()),
                type_system: compiler.db.type_system(),
                subtype_map: compiler.db.subtype_map(),
                subgraphs,
                object_types,
                interfaces,
                input_types,
                custom_scalars,
                enums,
                api_schema: None,
                schema_id,
                root_operations,
            })
        }
    }

    pub(crate) fn parse_with_ast(
        s: &str,
        configuration: &Configuration,
    ) -> Result<Self, SchemaError> {
        let mut schema = parse(s, configuration)?;
        schema.api_schema = Some(Box::new(parse(&make_api_schema(s)?, configuration)?));
        return Ok(schema);

        fn parse(schema: &str, _configuration: &Configuration) -> Result<Schema, SchemaError> {
            let mut compiler = ApolloCompiler::new();
            let id = compiler.add_type_system(
                include_str!("introspection_types.graphql"),
                "introspection_types.graphql",
            );
            let introspection_tree = compiler.db.ast(id);
            let id = compiler.add_type_system(schema, "schema.graphql");
            let tree = compiler.db.ast(id);

            // Trace log recursion limit data
            let recursion_limit = tree.recursion_limit();
            tracing::trace!(?recursion_limit, "recursion limit data");

            let introspection_errors = introspection_tree.errors().cloned().collect::<Vec<_>>();
            let errors = tree.errors().cloned().collect::<Vec<_>>();

            assert_eq!(introspection_errors, &[]);
            if !errors.is_empty() {
                let errors = ParseErrors {
                    raw_schema: schema.to_string(),
                    errors,
                };
                errors.print();
                return Err(SchemaError::Parse(errors));
            }

            let document = tree.document();
            let introspection_document = introspection_tree.document();
            let definitions = || {
                document
                    .definitions()
                    .chain(introspection_document.definitions())
            };
            let mut subtype_map: HashMap<String, HashSet<String>> = Default::default();
            let mut subgraphs = HashMap::new();
            let mut root_operations = HashMap::new();

            // the logic of this algorithm is inspired from the npm package graphql:
            // https://github.com/graphql/graphql-js/blob/ac8f0c6b484a0d5dca2dc13c387247f96772580a/src/type/schema.ts#L302-L327
            // https://github.com/graphql/graphql-js/blob/ac8f0c6b484a0d5dca2dc13c387247f96772580a/src/type/schema.ts#L294-L300
            // https://github.com/graphql/graphql-js/blob/ac8f0c6b484a0d5dca2dc13c387247f96772580a/src/type/schema.ts#L215-L263
            for definition in definitions() {
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
                                                        return Err(SchemaError::Api(format!("must not have several subgraphs with same name '{name}'")));
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
                    let mut map = definitions()
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

                    definitions()
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
                    let mut map = definitions()
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

                    definitions()
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

            let custom_scalars = definitions()
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

            let enums: HashMap<String, HashSet<String>> = definitions()
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
                raw_sdl: Arc::new(schema.to_owned()),
                type_system: compiler.db.type_system(),
                subtype_map: Arc::new(subtype_map),
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
        &self.raw_sdl
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
    ($visibility:vis $name:ident => $hir_ty:ty, $( $ast_ty:ty ),+ $(,)?) => {
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

        impl From<&'_ $hir_ty> for $name {
            fn from(def: &'_ $hir_ty) -> Self {
                Self {
                    name: def.name().to_owned(),
                    fields: def
                        .fields_definition()
                        .iter()
                        .chain(
                            def.extensions()
                                .iter()
                                .flat_map(|ext| ext.fields_definition()),
                        )
                        .map(|field| (field.name().to_owned(), field.ty().into()))
                        .collect(),
                    interfaces: def
                        .implements_interfaces()
                        .iter()
                        .chain(
                            def.extensions()
                                .iter()
                                .flat_map(|ext| ext.implements_interfaces()),
                        )
                        .map(|imp| imp.interface().to_owned())
                        .collect(),
                }
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
    hir::ObjectTypeDefinition,
    ast::ObjectTypeDefinition,
    ast::ObjectTypeExtension,
);
// Spec: https://spec.graphql.org/draft/#sec-Interfaces
// Spec: https://spec.graphql.org/draft/#sec-Interface-Extensions
implement_object_type_or_interface!(
    pub(crate) Interface =>
    hir::InterfaceTypeDefinition,
    ast::InterfaceTypeDefinition,
    ast::InterfaceTypeExtension,
);

macro_rules! implement_input_object_type_or_interface {
    ($visibility:vis $name:ident => $( $ast_ty:ty ),+ $(,)?) => {
        #[derive(Debug, Clone)]
        $visibility struct $name {
            name: String,
            pub(crate) fields: HashMap<String, (FieldType, Option<Value>)>,
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

impl From<&'_ hir::InputObjectTypeDefinition> for InputObjectType {
    fn from(def: &'_ hir::InputObjectTypeDefinition) -> Self {
        InputObjectType {
            name: def.name().to_owned(),
            fields: def
                .input_fields_definition()
                .iter()
                .chain(
                    def.extensions()
                        .iter()
                        .flat_map(|ext| ext.input_fields_definition()),
                )
                .map(|field| {
                    (
                        field.name().to_owned(),
                        (
                            field.ty().into(),
                            field.default_value().and_then(parse_hir_value),
                        ),
                    )
                })
                .collect(),
        }
    }
}

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
            let schema = format!("{base_schema}\n{schema}");
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
            let schema = format!("{base_schema}\n{schema}");
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
            other => panic!("unexpected schema result: {other:?}"),
        };
    }

    // https://github.com/apollographql/router/issues/2269
    #[test]
    fn unclosed_brace_error_does_not_panic() {
        let schema = "schema {";
        let result = Schema::parse(schema, &Default::default());
        assert!(result.is_err());
    }
}
