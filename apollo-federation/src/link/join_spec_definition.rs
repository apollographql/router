use std::sync::LazyLock;

use apollo_compiler::Name;
use apollo_compiler::Node;
use apollo_compiler::ast::Argument;
use apollo_compiler::ast::DirectiveLocation;
use apollo_compiler::ast::Type;
use apollo_compiler::ast::Value;
use apollo_compiler::name;
use apollo_compiler::schema::Directive;
use apollo_compiler::schema::DirectiveDefinition;
use apollo_compiler::schema::EnumType;
use apollo_compiler::schema::ExtendedType;
use apollo_compiler::ty;
use itertools::Itertools;

use super::argument::directive_optional_list_argument;
use crate::bail;
use crate::error::FederationError;
use crate::error::SingleFederationError;
use crate::link::argument::directive_optional_boolean_argument;
use crate::link::argument::directive_optional_enum_argument;
use crate::link::argument::directive_optional_string_argument;
use crate::link::argument::directive_required_enum_argument;
use crate::link::argument::directive_required_string_argument;
use crate::link::spec::Identity;
use crate::link::spec::Url;
use crate::link::spec::Version;
use crate::link::spec_definition::SpecDefinition;
use crate::link::spec_definition::SpecDefinitions;
use crate::schema::FederationSchema;
use crate::schema::type_and_directive_specification::ArgumentSpecification;
use crate::schema::type_and_directive_specification::DirectiveArgumentSpecification;
use crate::schema::type_and_directive_specification::DirectiveSpecification;
use crate::schema::type_and_directive_specification::EnumTypeSpecification;
use crate::schema::type_and_directive_specification::InputObjectTypeSpecification;
use crate::schema::type_and_directive_specification::ScalarTypeSpecification;
use crate::schema::type_and_directive_specification::TypeAndDirectiveSpecification;

pub(crate) const JOIN_GRAPH_ENUM_NAME_IN_SPEC: Name = name!("Graph");
pub(crate) const JOIN_GRAPH_DIRECTIVE_NAME_IN_SPEC: Name = name!("graph");
pub(crate) const JOIN_TYPE_DIRECTIVE_NAME_IN_SPEC: Name = name!("type");
pub(crate) const JOIN_FIELD_DIRECTIVE_NAME_IN_SPEC: Name = name!("field");
pub(crate) const JOIN_IMPLEMENTS_DIRECTIVE_NAME_IN_SPEC: Name = name!("implements");
pub(crate) const JOIN_UNIONMEMBER_DIRECTIVE_NAME_IN_SPEC: Name = name!("unionMember");
pub(crate) const JOIN_ENUMVALUE_DIRECTIVE_NAME_IN_SPEC: Name = name!("enumValue");
pub(crate) const JOIN_DIRECTIVE_DIRECTIVE_NAME_IN_SPEC: Name = name!("directive");

pub(crate) const JOIN_FIELD_SET_NAME_IN_SPEC: Name = name!("FieldSet");
pub(crate) const JOIN_DIRECTIVE_ARGUMENTS_NAME_IN_SPEC: Name = name!("DirectiveArguments");
pub(crate) const JOIN_CONTEXT_ARGUMENT_NAME_IN_SPEC: Name = name!("ContextArgument");

pub(crate) const JOIN_NAME_ARGUMENT_NAME: Name = name!("name");
pub(crate) const JOIN_URL_ARGUMENT_NAME: Name = name!("url");
pub(crate) const JOIN_GRAPH_ARGUMENT_NAME: Name = name!("graph");
pub(crate) const JOIN_KEY_ARGUMENT_NAME: Name = name!("key");
pub(crate) const JOIN_EXTENSION_ARGUMENT_NAME: Name = name!("extension");
pub(crate) const JOIN_RESOLVABLE_ARGUMENT_NAME: Name = name!("resolvable");
pub(crate) const JOIN_ISINTERFACEOBJECT_ARGUMENT_NAME: Name = name!("isInterfaceObject");
pub(crate) const JOIN_REQUIRES_ARGUMENT_NAME: Name = name!("requires");
pub(crate) const JOIN_PROVIDES_ARGUMENT_NAME: Name = name!("provides");
pub(crate) const JOIN_TYPE_ARGUMENT_NAME: Name = name!("type");
pub(crate) const JOIN_EXTERNAL_ARGUMENT_NAME: Name = name!("external");
pub(crate) const JOIN_OVERRIDE_ARGUMENT_NAME: Name = name!("override");
pub(crate) const JOIN_OVERRIDE_LABEL_ARGUMENT_NAME: Name = name!("overrideLabel");
pub(crate) const JOIN_USEROVERRIDDEN_ARGUMENT_NAME: Name = name!("usedOverridden");
pub(crate) const JOIN_INTERFACE_ARGUMENT_NAME: Name = name!("interface");
pub(crate) const JOIN_MEMBER_ARGUMENT_NAME: Name = name!("member");
pub(crate) const JOIN_CONTEXTARGUMENTS_ARGUMENT_NAME: Name = name!("contextArguments");

pub(crate) struct GraphDirectiveArguments<'doc> {
    pub(crate) name: &'doc str,
    pub(crate) url: &'doc str,
}

pub(crate) struct TypeDirectiveArguments<'doc> {
    pub(crate) graph: Name,
    pub(crate) key: Option<&'doc str>,
    pub(crate) extension: bool,
    pub(crate) resolvable: bool,
    pub(crate) is_interface_object: bool,
}

pub(crate) struct ContextArgument<'doc> {
    pub(crate) name: &'doc str,
    pub(crate) type_: &'doc str,
    pub(crate) context: &'doc str,
    pub(crate) selection: &'doc str,
}

impl<'doc> TryFrom<&'doc Value> for ContextArgument<'doc> {
    type Error = FederationError;

    fn try_from(value: &'doc Value) -> Result<Self, Self::Error> {
        fn insert_value<'a>(
            name: &str,
            field: &mut Option<&'a Value>,
            value: &'a Value,
        ) -> Result<(), FederationError> {
            if let Some(first_value) = field {
                bail!(
                    r#"Input field "{name}" in contextArguments is repeated with value "{value}" (previous value was "{first_value}")"#
                )
            }
            let _ = field.insert(value);
            Ok(())
        }

        fn field_or_else<'a>(
            field_name: &'static str,
            field: Option<&'a Value>,
        ) -> Result<&'a str, FederationError> {
            field
                .ok_or_else(|| {
                    FederationError::internal(format!(
                        r#"Input field "{field_name}" is missing from contextArguments"#
                    ))
                })?
                .as_str()
                .ok_or_else(|| {
                    FederationError::internal(format!(
                        r#"Input field "{field_name}" in contextArguments is not a string"#
                    ))
                })
        }

        let Value::Object(input_object) = value else {
            bail!(r#"Item "{value}" in contextArguments list is not an object"#)
        };
        let mut name = None;
        let mut type_ = None;
        let mut context = None;
        let mut selection = None;
        for (input_field_name, value) in input_object {
            match input_field_name.as_str() {
                "name" => insert_value(input_field_name, &mut name, value)?,
                "type" => insert_value(input_field_name, &mut type_, value)?,
                "context" => insert_value(input_field_name, &mut context, value)?,
                "selection" => insert_value(input_field_name, &mut selection, value)?,
                _ => bail!(r#"Found unknown contextArguments input field "{input_field_name}""#),
            }
        }

        let name = field_or_else("name", name)?;
        let type_ = field_or_else("type", type_)?;
        let context = field_or_else("context", context)?;
        let selection = field_or_else("selection", selection)?;

        Ok(Self {
            name,
            type_,
            context,
            selection,
        })
    }
}

pub(crate) struct FieldDirectiveArguments<'doc> {
    pub(crate) graph: Option<Name>,
    pub(crate) requires: Option<&'doc str>,
    pub(crate) provides: Option<&'doc str>,
    pub(crate) type_: Option<&'doc str>,
    pub(crate) external: Option<bool>,
    pub(crate) override_: Option<&'doc str>,
    pub(crate) override_label: Option<&'doc str>,
    pub(crate) user_overridden: Option<bool>,
    pub(crate) context_arguments: Option<Vec<ContextArgument<'doc>>>,
}

pub(crate) struct ImplementsDirectiveArguments<'doc> {
    pub(crate) graph: Name,
    pub(crate) interface: &'doc str,
}

pub(crate) struct UnionMemberDirectiveArguments<'doc> {
    pub(crate) graph: Name,
    pub(crate) member: &'doc str,
}

pub(crate) struct EnumValueDirectiveArguments {
    pub(crate) graph: Name,
}

#[derive(Clone)]
pub(crate) struct JoinSpecDefinition {
    url: Url,
    minimum_federation_version: Version,
}

impl JoinSpecDefinition {
    pub(crate) fn new(version: Version, minimum_federation_version: Version) -> Self {
        Self {
            url: Url {
                identity: Identity::join_identity(),
                version,
            },
            minimum_federation_version,
        }
    }

    pub(crate) fn graph_enum_definition<'schema>(
        &self,
        schema: &'schema FederationSchema,
    ) -> Result<&'schema Node<EnumType>, FederationError> {
        let type_ = self
            .type_definition(schema, &JOIN_GRAPH_ENUM_NAME_IN_SPEC)?
            .ok_or_else(|| SingleFederationError::Internal {
                message: "Unexpectedly could not find join spec in schema".to_owned(),
            })?;
        if let ExtendedType::Enum(type_) = type_ {
            Ok(type_)
        } else {
            Err(SingleFederationError::Internal {
                message: format!(
                    "Unexpectedly found non-enum for join spec's \"{}\" enum definition",
                    JOIN_GRAPH_ENUM_NAME_IN_SPEC,
                ),
            }
            .into())
        }
    }

    pub(crate) fn graph_directive_definition<'schema>(
        &self,
        schema: &'schema FederationSchema,
    ) -> Result<&'schema Node<DirectiveDefinition>, FederationError> {
        self.directive_definition(schema, &JOIN_GRAPH_DIRECTIVE_NAME_IN_SPEC)?
            .ok_or_else(|| {
                SingleFederationError::Internal {
                    message: "Unexpectedly could not find join spec in schema".to_owned(),
                }
                .into()
            })
    }

    pub(crate) fn graph_directive_arguments<'doc>(
        &self,
        application: &'doc Node<Directive>,
    ) -> Result<GraphDirectiveArguments<'doc>, FederationError> {
        Ok(GraphDirectiveArguments {
            name: directive_required_string_argument(application, &JOIN_NAME_ARGUMENT_NAME)?,
            url: directive_required_string_argument(application, &JOIN_URL_ARGUMENT_NAME)?,
        })
    }

    pub(crate) fn type_directive_definition<'schema>(
        &self,
        schema: &'schema FederationSchema,
    ) -> Result<&'schema Node<DirectiveDefinition>, FederationError> {
        self.directive_definition(schema, &JOIN_TYPE_DIRECTIVE_NAME_IN_SPEC)?
            .ok_or_else(|| {
                SingleFederationError::Internal {
                    message: "Unexpectedly could not find join spec in schema".to_owned(),
                }
                .into()
            })
    }

    pub(crate) fn type_directive_arguments<'doc>(
        &self,
        application: &'doc Node<Directive>,
    ) -> Result<TypeDirectiveArguments<'doc>, FederationError> {
        Ok(TypeDirectiveArguments {
            graph: directive_required_enum_argument(application, &JOIN_GRAPH_ARGUMENT_NAME)?,
            key: directive_optional_string_argument(application, &JOIN_KEY_ARGUMENT_NAME)?,
            extension: directive_optional_boolean_argument(
                application,
                &JOIN_EXTENSION_ARGUMENT_NAME,
            )?
            .unwrap_or(false),
            resolvable: directive_optional_boolean_argument(
                application,
                &JOIN_RESOLVABLE_ARGUMENT_NAME,
            )?
            .unwrap_or(true),
            is_interface_object: directive_optional_boolean_argument(
                application,
                &JOIN_ISINTERFACEOBJECT_ARGUMENT_NAME,
            )?
            .unwrap_or(false),
        })
    }

    pub(crate) fn field_directive_definition<'schema>(
        &self,
        schema: &'schema FederationSchema,
    ) -> Result<&'schema Node<DirectiveDefinition>, FederationError> {
        self.directive_definition(schema, &JOIN_FIELD_DIRECTIVE_NAME_IN_SPEC)?
            .ok_or_else(|| {
                SingleFederationError::Internal {
                    message: "Unexpectedly could not find join spec in schema".to_owned(),
                }
                .into()
            })
    }

    pub(crate) fn field_directive_arguments<'doc>(
        &self,
        application: &'doc Node<Directive>,
    ) -> Result<FieldDirectiveArguments<'doc>, FederationError> {
        Ok(FieldDirectiveArguments {
            graph: directive_optional_enum_argument(application, &JOIN_GRAPH_ARGUMENT_NAME)?,
            requires: directive_optional_string_argument(
                application,
                &JOIN_REQUIRES_ARGUMENT_NAME,
            )?,
            provides: directive_optional_string_argument(
                application,
                &JOIN_PROVIDES_ARGUMENT_NAME,
            )?,
            type_: directive_optional_string_argument(application, &JOIN_TYPE_ARGUMENT_NAME)?,
            external: directive_optional_boolean_argument(
                application,
                &JOIN_EXTERNAL_ARGUMENT_NAME,
            )?,
            override_: directive_optional_string_argument(
                application,
                &JOIN_OVERRIDE_ARGUMENT_NAME,
            )?,
            override_label: directive_optional_string_argument(
                application,
                &JOIN_OVERRIDE_LABEL_ARGUMENT_NAME,
            )?,
            user_overridden: directive_optional_boolean_argument(
                application,
                &JOIN_USEROVERRIDDEN_ARGUMENT_NAME,
            )?,
            context_arguments: directive_optional_list_argument(
                application,
                &JOIN_CONTEXTARGUMENTS_ARGUMENT_NAME,
            )?
            .map(|values| {
                values
                    .iter()
                    .map(|value| ContextArgument::try_from(value.as_ref()))
                    .try_collect()
            })
            .transpose()?,
        })
    }

    pub(crate) fn implements_directive_definition<'schema>(
        &self,
        schema: &'schema FederationSchema,
    ) -> Result<Option<&'schema Node<DirectiveDefinition>>, FederationError> {
        if *self.version() < (Version { major: 0, minor: 2 }) {
            return Ok(None);
        }
        self.directive_definition(schema, &JOIN_IMPLEMENTS_DIRECTIVE_NAME_IN_SPEC)?
            .ok_or_else(|| {
                SingleFederationError::Internal {
                    message: "Unexpectedly could not find join spec in schema".to_owned(),
                }
                .into()
            })
            .map(Some)
    }

    pub(crate) fn implements_directive_arguments<'doc>(
        &self,
        application: &'doc Node<Directive>,
    ) -> Result<ImplementsDirectiveArguments<'doc>, FederationError> {
        Ok(ImplementsDirectiveArguments {
            graph: directive_required_enum_argument(application, &JOIN_GRAPH_ARGUMENT_NAME)?,
            interface: directive_required_string_argument(
                application,
                &JOIN_INTERFACE_ARGUMENT_NAME,
            )?,
        })
    }

    pub(crate) fn union_member_directive_definition<'schema>(
        &self,
        schema: &'schema FederationSchema,
    ) -> Result<Option<&'schema Node<DirectiveDefinition>>, FederationError> {
        if *self.version() < (Version { major: 0, minor: 3 }) {
            return Ok(None);
        }
        self.directive_definition(schema, &JOIN_UNIONMEMBER_DIRECTIVE_NAME_IN_SPEC)?
            .ok_or_else(|| {
                SingleFederationError::Internal {
                    message: "Unexpectedly could not find join spec in schema".to_owned(),
                }
                .into()
            })
            .map(Some)
    }

    pub(crate) fn union_member_directive_arguments<'doc>(
        &self,
        application: &'doc Node<Directive>,
    ) -> Result<UnionMemberDirectiveArguments<'doc>, FederationError> {
        Ok(UnionMemberDirectiveArguments {
            graph: directive_required_enum_argument(application, &JOIN_GRAPH_ARGUMENT_NAME)?,
            member: directive_required_string_argument(application, &JOIN_MEMBER_ARGUMENT_NAME)?,
        })
    }

    pub(crate) fn union_member_directive(
        &self,
        schema: &FederationSchema,
        subgraph_name: &Name,
        member_name: &str,
    ) -> Result<Directive, FederationError> {
        let Ok(Some(name_in_schema)) =
            self.directive_name_in_schema(schema, &JOIN_UNIONMEMBER_DIRECTIVE_NAME_IN_SPEC)
        else {
            bail!("Unexpectedly could not find unionMember directive in schema");
        };
        Ok(Directive {
            name: name_in_schema,
            arguments: vec![
                Node::new(Argument {
                    name: JOIN_GRAPH_DIRECTIVE_NAME_IN_SPEC,
                    value: Node::new(Value::Enum(subgraph_name.clone())),
                }),
                {
                    Node::new(Argument {
                        name: JOIN_MEMBER_ARGUMENT_NAME,
                        value: Node::new(Value::String(member_name.to_owned())),
                    })
                },
            ],
        })
    }

    pub(crate) fn enum_value_directive_definition<'schema>(
        &self,
        schema: &'schema FederationSchema,
    ) -> Result<Option<&'schema Node<DirectiveDefinition>>, FederationError> {
        if *self.version() < (Version { major: 0, minor: 3 }) {
            return Ok(None);
        }
        self.directive_definition(schema, &JOIN_ENUMVALUE_DIRECTIVE_NAME_IN_SPEC)?
            .ok_or_else(|| {
                SingleFederationError::Internal {
                    message: "Unexpectedly could not find join spec in schema".to_owned(),
                }
                .into()
            })
            .map(Some)
    }

    pub(crate) fn enum_value_directive_arguments(
        &self,
        application: &Node<Directive>,
    ) -> Result<EnumValueDirectiveArguments, FederationError> {
        Ok(EnumValueDirectiveArguments {
            graph: directive_required_enum_argument(application, &JOIN_GRAPH_ARGUMENT_NAME)?,
        })
    }

    pub(crate) fn enum_value_directive(
        &self,
        schema: &FederationSchema,
        subgraph_name: &Name,
    ) -> Result<Directive, FederationError> {
        let name_in_schema = self
            .directive_name_in_schema(schema, &JOIN_ENUMVALUE_DIRECTIVE_NAME_IN_SPEC)?
            .ok_or_else(|| SingleFederationError::Internal {
                message: "Unexpectedly could not find enumValue directive in schema".to_owned(),
            })?;
        Ok(Directive {
            name: name_in_schema,
            arguments: vec![Node::new(Argument {
                name: JOIN_GRAPH_DIRECTIVE_NAME_IN_SPEC,
                value: Node::new(Value::Enum(subgraph_name.clone())),
            })],
        })
    }

    /// @join__graph
    fn graph_directive_specification(&self) -> DirectiveSpecification {
        DirectiveSpecification::new(
            JOIN_GRAPH_DIRECTIVE_NAME_IN_SPEC,
            &[
                DirectiveArgumentSpecification {
                    base_spec: ArgumentSpecification {
                        name: JOIN_NAME_ARGUMENT_NAME,
                        get_type: |_, _| Ok(ty!(String!)),
                        default_value: None,
                    },
                    composition_strategy: None,
                },
                DirectiveArgumentSpecification {
                    base_spec: ArgumentSpecification {
                        name: JOIN_URL_ARGUMENT_NAME,
                        get_type: |_, _| Ok(ty!(String!)),
                        default_value: None,
                    },
                    composition_strategy: None,
                },
            ],
            false,
            &[DirectiveLocation::EnumValue],
            false,
            Some(&|v| JOIN_VERSIONS.get_dyn_minimum_required_version(v)),
            None,
        )
    }

    /// @join__type
    fn type_directive_specification(&self) -> DirectiveSpecification {
        let mut args = vec![
            DirectiveArgumentSpecification {
                base_spec: ArgumentSpecification {
                    name: JOIN_GRAPH_ARGUMENT_NAME,
                    get_type: |_schema, link| {
                        let graph_name = link.map_or(JOIN_GRAPH_ENUM_NAME_IN_SPEC, |link| {
                            link.type_name_in_schema(&JOIN_GRAPH_ENUM_NAME_IN_SPEC)
                        });
                        Ok(Type::NonNullNamed(graph_name))
                    },
                    default_value: None,
                },
                composition_strategy: None,
            },
            DirectiveArgumentSpecification {
                base_spec: ArgumentSpecification {
                    name: JOIN_KEY_ARGUMENT_NAME,
                    get_type: |_schema, link| {
                        let field_set_name = link.map_or(JOIN_FIELD_SET_NAME_IN_SPEC, |link| {
                            link.type_name_in_schema(&JOIN_FIELD_SET_NAME_IN_SPEC)
                        });
                        Ok(Type::Named(field_set_name))
                    },
                    default_value: None,
                },
                composition_strategy: None,
            },
        ];
        if *self.version() >= (Version { major: 0, minor: 2 }) {
            args.push(DirectiveArgumentSpecification {
                base_spec: ArgumentSpecification {
                    name: JOIN_EXTENSION_ARGUMENT_NAME,
                    get_type: |_, _| Ok(ty!(Boolean!)),
                    default_value: Some(Value::Boolean(false)),
                },
                composition_strategy: None,
            });
            args.push(DirectiveArgumentSpecification {
                base_spec: ArgumentSpecification {
                    name: JOIN_RESOLVABLE_ARGUMENT_NAME,
                    get_type: |_, _| Ok(ty!(Boolean!)),
                    default_value: Some(Value::Boolean(true)),
                },
                composition_strategy: None,
            });
        }
        if *self.version() >= (Version { major: 0, minor: 3 }) {
            args.push(DirectiveArgumentSpecification {
                base_spec: ArgumentSpecification {
                    name: JOIN_ISINTERFACEOBJECT_ARGUMENT_NAME,
                    get_type: |_, _| Ok(ty!(Boolean!)),
                    default_value: Some(Value::Boolean(false)),
                },
                composition_strategy: None,
            });
        }

        DirectiveSpecification::new(
            JOIN_TYPE_DIRECTIVE_NAME_IN_SPEC,
            &args,
            *self.version() >= (Version { major: 0, minor: 2 }),
            &[
                DirectiveLocation::Object,
                DirectiveLocation::Interface,
                DirectiveLocation::Union,
                DirectiveLocation::Enum,
                DirectiveLocation::InputObject,
                DirectiveLocation::Scalar,
            ],
            false,
            Some(&|v| JOIN_VERSIONS.get_dyn_minimum_required_version(v)),
            None,
        )
    }

    /// @join__field
    fn field_directive_specification(&self) -> DirectiveSpecification {
        let mut args = vec![
            DirectiveArgumentSpecification {
                base_spec: ArgumentSpecification {
                    name: JOIN_REQUIRES_ARGUMENT_NAME,
                    get_type: |_schema, link| {
                        let field_set_name = link.map_or(JOIN_FIELD_SET_NAME_IN_SPEC, |link| {
                            link.type_name_in_schema(&JOIN_FIELD_SET_NAME_IN_SPEC)
                        });
                        Ok(Type::Named(field_set_name))
                    },
                    default_value: None,
                },
                composition_strategy: None,
            },
            DirectiveArgumentSpecification {
                base_spec: ArgumentSpecification {
                    name: JOIN_PROVIDES_ARGUMENT_NAME,
                    get_type: |_schema, link| {
                        let field_set_name = link.map_or(JOIN_FIELD_SET_NAME_IN_SPEC, |link| {
                            link.type_name_in_schema(&JOIN_FIELD_SET_NAME_IN_SPEC)
                        });
                        Ok(Type::Named(field_set_name))
                    },
                    default_value: None,
                },
                composition_strategy: None,
            },
        ];
        // The `graph` argument used to be non-nullable, but @interfaceObject makes us add some field in
        // the supergraph that don't "directly" come from any subgraph (they indirectly are inherited from
        // an `@interfaceObject` type), and to indicate that, we use a `@join__field(graph: null)` annotation.
        if *self.version() >= (Version { major: 0, minor: 3 }) {
            args.insert(
                0,
                DirectiveArgumentSpecification {
                    base_spec: ArgumentSpecification {
                        name: JOIN_GRAPH_ARGUMENT_NAME,
                        get_type: |_schema, link| {
                            let graph_name = link.map_or(JOIN_GRAPH_ENUM_NAME_IN_SPEC, |link| {
                                link.type_name_in_schema(&JOIN_GRAPH_ENUM_NAME_IN_SPEC)
                            });
                            Ok(Type::Named(graph_name))
                        },
                        default_value: None,
                    },
                    composition_strategy: None,
                },
            );
        } else {
            args.insert(
                0,
                DirectiveArgumentSpecification {
                    base_spec: ArgumentSpecification {
                        name: JOIN_GRAPH_ARGUMENT_NAME,
                        get_type: |_schema, link| {
                            let graph_name = link.map_or(JOIN_GRAPH_ENUM_NAME_IN_SPEC, |link| {
                                link.type_name_in_schema(&JOIN_GRAPH_ENUM_NAME_IN_SPEC)
                            });
                            Ok(Type::NonNullNamed(graph_name))
                        },
                        default_value: None,
                    },
                    composition_strategy: None,
                },
            );
        }

        if *self.version() >= (Version { major: 0, minor: 2 }) {
            args.push(DirectiveArgumentSpecification {
                base_spec: ArgumentSpecification {
                    name: JOIN_TYPE_ARGUMENT_NAME,
                    get_type: |_, _| Ok(ty!(String)),
                    default_value: None,
                },
                composition_strategy: None,
            });
            args.push(DirectiveArgumentSpecification {
                base_spec: ArgumentSpecification {
                    name: JOIN_EXTERNAL_ARGUMENT_NAME,
                    get_type: |_, _| Ok(ty!(Boolean)),
                    default_value: None,
                },
                composition_strategy: None,
            });
            args.push(DirectiveArgumentSpecification {
                base_spec: ArgumentSpecification {
                    name: JOIN_OVERRIDE_ARGUMENT_NAME,
                    get_type: |_, _| Ok(ty!(String)),
                    default_value: None,
                },
                composition_strategy: None,
            });
            args.push(DirectiveArgumentSpecification {
                base_spec: ArgumentSpecification {
                    name: JOIN_USEROVERRIDDEN_ARGUMENT_NAME,
                    get_type: |_, _| Ok(ty!(Boolean)),
                    default_value: None,
                },
                composition_strategy: None,
            });
        }
        if *self.version() >= (Version { major: 0, minor: 4 }) {
            args.push(DirectiveArgumentSpecification {
                base_spec: ArgumentSpecification {
                    name: JOIN_OVERRIDE_LABEL_ARGUMENT_NAME,
                    get_type: |_, _| Ok(ty!(String)),
                    default_value: None,
                },
                composition_strategy: None,
            });
        }
        if *self.version() >= (Version { major: 0, minor: 5 }) {
            args.push(DirectiveArgumentSpecification {
                base_spec: ArgumentSpecification {
                    name: JOIN_CONTEXTARGUMENTS_ARGUMENT_NAME,
                    get_type: |_schema, link| {
                        let context_arg_name = link
                            .map_or(JOIN_CONTEXT_ARGUMENT_NAME_IN_SPEC, |link| {
                                link.type_name_in_schema(&JOIN_CONTEXT_ARGUMENT_NAME_IN_SPEC)
                            });
                        Ok(Type::List(Box::new(Type::NonNullNamed(context_arg_name))))
                    },
                    default_value: None,
                },
                composition_strategy: None,
            });
        }

        DirectiveSpecification::new(
            JOIN_FIELD_DIRECTIVE_NAME_IN_SPEC,
            &args,
            true, // repeatable
            &[
                DirectiveLocation::FieldDefinition,
                DirectiveLocation::InputFieldDefinition,
            ],
            false, // doesn't compose
            Some(&|v| JOIN_VERSIONS.get_dyn_minimum_required_version(v)),
            None,
        )
    }

    /// @join__implements
    fn implements_directive_spec(&self) -> Option<DirectiveSpecification> {
        if *self.version() < (Version { major: 0, minor: 2 }) {
            return None;
        }
        Some(DirectiveSpecification::new(
            JOIN_IMPLEMENTS_DIRECTIVE_NAME_IN_SPEC,
            &[
                DirectiveArgumentSpecification {
                    base_spec: ArgumentSpecification {
                        name: JOIN_GRAPH_ARGUMENT_NAME,
                        get_type: |_schema, link| {
                            let graph_name = link.map_or(JOIN_GRAPH_ENUM_NAME_IN_SPEC, |link| {
                                link.type_name_in_schema(&JOIN_GRAPH_ENUM_NAME_IN_SPEC)
                            });
                            Ok(Type::NonNullNamed(graph_name))
                        },
                        default_value: None,
                    },
                    composition_strategy: None,
                },
                DirectiveArgumentSpecification {
                    base_spec: ArgumentSpecification {
                        name: JOIN_INTERFACE_ARGUMENT_NAME,
                        get_type: |_, _| Ok(ty!(String!)),
                        default_value: None,
                    },
                    composition_strategy: None,
                },
            ],
            true, // repeatable
            &[DirectiveLocation::Object, DirectiveLocation::Interface],
            false, // doesn't compose
            Some(&|v| JOIN_VERSIONS.get_dyn_minimum_required_version(v)),
            None,
        ))
    }

    /// @join__unionMember
    fn union_member_directive_spec(&self) -> Option<DirectiveSpecification> {
        if *self.version() < (Version { major: 0, minor: 3 }) {
            return None;
        }
        Some(DirectiveSpecification::new(
            JOIN_UNIONMEMBER_DIRECTIVE_NAME_IN_SPEC,
            &[
                DirectiveArgumentSpecification {
                    base_spec: ArgumentSpecification {
                        name: JOIN_GRAPH_ARGUMENT_NAME,
                        get_type: |_schema, link| {
                            let graph_name = link.map_or(JOIN_GRAPH_ENUM_NAME_IN_SPEC, |link| {
                                link.type_name_in_schema(&JOIN_GRAPH_ENUM_NAME_IN_SPEC)
                            });
                            Ok(Type::NonNullNamed(graph_name))
                        },
                        default_value: None,
                    },
                    composition_strategy: None,
                },
                DirectiveArgumentSpecification {
                    base_spec: ArgumentSpecification {
                        name: JOIN_MEMBER_ARGUMENT_NAME,
                        get_type: |_, _| Ok(ty!(String!)),
                        default_value: None,
                    },
                    composition_strategy: None,
                },
            ],
            true, // repeatable
            &[DirectiveLocation::Union],
            false, // doesn't compose
            Some(&|v| JOIN_VERSIONS.get_dyn_minimum_required_version(v)),
            None,
        ))
    }

    /// @join__enumValue
    pub(crate) fn enum_value_directive_spec(&self) -> Option<DirectiveSpecification> {
        if *self.version() < (Version { major: 0, minor: 3 }) {
            return None;
        }
        Some(DirectiveSpecification::new(
            JOIN_ENUMVALUE_DIRECTIVE_NAME_IN_SPEC,
            &[DirectiveArgumentSpecification {
                base_spec: ArgumentSpecification {
                    name: JOIN_GRAPH_ARGUMENT_NAME,
                    get_type: |_schema, link| {
                        let graph_name = link.map_or(JOIN_GRAPH_ENUM_NAME_IN_SPEC, |link| {
                            link.type_name_in_schema(&JOIN_GRAPH_ENUM_NAME_IN_SPEC)
                        });
                        Ok(Type::NonNullNamed(graph_name))
                    },
                    default_value: None,
                },
                composition_strategy: None,
            }],
            true, // repeatable
            &[DirectiveLocation::EnumValue],
            false, // doesn't compose
            Some(&|v| JOIN_VERSIONS.get_dyn_minimum_required_version(v)),
            None,
        ))
    }

    /// @join__directive
    fn directive_directive_spec(&self) -> Option<DirectiveSpecification> {
        if *self.version() < (Version { major: 0, minor: 4 }) {
            return None;
        }
        Some(DirectiveSpecification::new(
            JOIN_DIRECTIVE_DIRECTIVE_NAME_IN_SPEC,
            &[
                DirectiveArgumentSpecification {
                    base_spec: ArgumentSpecification {
                        name: name!("graphs"),
                        get_type: |_schema, link| {
                            let graph_name = link.map_or(JOIN_GRAPH_ENUM_NAME_IN_SPEC, |link| {
                                link.type_name_in_schema(&JOIN_GRAPH_ENUM_NAME_IN_SPEC)
                            });
                            Ok(Type::List(Box::new(Type::NonNullNamed(graph_name))))
                        },
                        default_value: None,
                    },
                    composition_strategy: None,
                },
                DirectiveArgumentSpecification {
                    base_spec: ArgumentSpecification {
                        name: JOIN_NAME_ARGUMENT_NAME,
                        get_type: |_, _| Ok(ty!(String!)),
                        default_value: None,
                    },
                    composition_strategy: None,
                },
                DirectiveArgumentSpecification {
                    base_spec: ArgumentSpecification {
                        name: name!("args"),
                        get_type: |_schema, link| {
                            let directive_args_name =
                                link.map_or(JOIN_DIRECTIVE_ARGUMENTS_NAME_IN_SPEC, |link| {
                                    link.type_name_in_schema(&JOIN_DIRECTIVE_ARGUMENTS_NAME_IN_SPEC)
                                });
                            Ok(Type::Named(directive_args_name))
                        },
                        default_value: None,
                    },
                    composition_strategy: None,
                },
            ],
            true, // repeatable
            &[
                DirectiveLocation::Schema,
                DirectiveLocation::Object,
                DirectiveLocation::Interface,
                DirectiveLocation::FieldDefinition,
            ],
            false, // doesn't compose
            Some(&|v| JOIN_VERSIONS.get_dyn_minimum_required_version(v)),
            None,
        ))
    }

    /// @join__owner
    fn owner_directive_spec(&self) -> Option<DirectiveSpecification> {
        if *self.version() != (Version { major: 0, minor: 1 }) {
            return None;
        }
        Some(DirectiveSpecification::new(
            name!("owner"),
            &[DirectiveArgumentSpecification {
                base_spec: ArgumentSpecification {
                    name: JOIN_GRAPH_ARGUMENT_NAME,
                    get_type: |_schema, link| {
                        let graph_name = link.map_or(JOIN_GRAPH_ENUM_NAME_IN_SPEC, |link| {
                            link.type_name_in_schema(&JOIN_GRAPH_ENUM_NAME_IN_SPEC)
                        });
                        Ok(Type::NonNullNamed(graph_name))
                    },
                    default_value: None,
                },
                composition_strategy: None,
            }],
            false, // not repeatable
            &[DirectiveLocation::Object],
            false, // doesn't compose
            Some(&|v| JOIN_VERSIONS.get_dyn_minimum_required_version(v)),
            None,
        ))
    }
}

impl SpecDefinition for JoinSpecDefinition {
    fn url(&self) -> &Url {
        &self.url
    }

    fn directive_specs(&self) -> Vec<Box<dyn TypeAndDirectiveSpecification>> {
        let mut specs: Vec<Box<dyn TypeAndDirectiveSpecification>> = vec![
            Box::new(self.graph_directive_specification()),
            Box::new(self.type_directive_specification()),
            Box::new(self.field_directive_specification()),
        ];
        if let Some(spec) = self.implements_directive_spec() {
            specs.push(Box::new(spec));
        }
        if let Some(spec) = self.union_member_directive_spec() {
            specs.push(Box::new(spec));
        }
        if let Some(spec) = self.enum_value_directive_spec() {
            specs.push(Box::new(spec));
        }
        if let Some(spec) = self.directive_directive_spec() {
            specs.push(Box::new(spec));
        }
        if let Some(spec) = self.owner_directive_spec() {
            specs.push(Box::new(spec));
        }

        specs
    }

    fn type_specs(&self) -> Vec<Box<dyn TypeAndDirectiveSpecification>> {
        let mut specs: Vec<Box<dyn TypeAndDirectiveSpecification>> = Vec::new();

        // Enum Graph
        specs.push(Box::new(EnumTypeSpecification {
            name: JOIN_GRAPH_ENUM_NAME_IN_SPEC,
            values: vec![], // Initialized with no values, but graphs will be added later as they get merged in
        }));

        // Scalar FieldSet
        specs.push(Box::new(ScalarTypeSpecification {
            name: JOIN_FIELD_SET_NAME_IN_SPEC,
        }));

        // Scalar DirectiveArguments (v0.4+)
        if *self.version() >= (Version { major: 0, minor: 4 }) {
            specs.push(Box::new(ScalarTypeSpecification {
                name: JOIN_DIRECTIVE_ARGUMENTS_NAME_IN_SPEC,
            }));
        }

        if *self.version() >= (Version { major: 0, minor: 5 }) {
            // Scalar FieldValue (v0.5+)
            specs.push(Box::new(ScalarTypeSpecification {
                name: name!("FieldValue"),
            }));

            // InputObject join__ContextArgument (v0.5+)
            specs.push(Box::new(InputObjectTypeSpecification {
                name: name!("ContextArgument"),
                fields: |_| {
                    vec![
                        ArgumentSpecification {
                            name: name!("name"),
                            get_type: |_, _| Ok(ty!(String!)),
                            default_value: None,
                        },
                        ArgumentSpecification {
                            name: name!("type"),
                            get_type: |_, _| Ok(ty!(String!)),
                            default_value: None,
                        },
                        ArgumentSpecification {
                            name: name!("context"),
                            get_type: |_, _| Ok(ty!(String!)),
                            default_value: None,
                        },
                        ArgumentSpecification {
                            name: name!("selection"),
                            get_type: |_schema, link| {
                                let field_value_name = link.map_or(name!("FieldValue"), |link| {
                                    link.type_name_in_schema(&name!("FieldValue"))
                                });
                                Ok(Type::Named(field_value_name))
                            },
                            default_value: None,
                        },
                    ]
                },
            }));
        }

        specs
    }

    fn minimum_federation_version(&self) -> &Version {
        &self.minimum_federation_version
    }
}

/// The versions are as follows:
///  - 0.1: this is the version used by federation 1 composition. Federation 2 is still able to read supergraphs
///    using that verison for backward compatibility, but never writes this spec version is not expressive enough
///    for federation 2 in general.
///  - 0.2: this is the original version released with federation 2.
///  - 0.3: adds the `isInterfaceObject` argument to `@join__type`, and make the `graph` in `@join__field` skippable.
///  - 0.4: adds the optional `overrideLabel` argument to `@join_field` for progressive override.
///  - 0.5: adds the `contextArguments` argument to `@join_field` for setting context.
pub(crate) static JOIN_VERSIONS: LazyLock<SpecDefinitions<JoinSpecDefinition>> =
    LazyLock::new(|| {
        let mut definitions = SpecDefinitions::new(Identity::join_identity());
        definitions.add(JoinSpecDefinition::new(
            Version { major: 0, minor: 1 },
            Version { major: 1, minor: 0 },
        ));
        definitions.add(JoinSpecDefinition::new(
            Version { major: 0, minor: 2 },
            Version { major: 1, minor: 0 },
        ));
        definitions.add(JoinSpecDefinition::new(
            Version { major: 0, minor: 3 },
            Version { major: 2, minor: 0 },
        ));
        definitions.add(JoinSpecDefinition::new(
            Version { major: 0, minor: 4 },
            Version { major: 2, minor: 7 },
        ));
        definitions.add(JoinSpecDefinition::new(
            Version { major: 0, minor: 5 },
            Version { major: 2, minor: 8 },
        ));
        definitions
    });

#[cfg(test)]
mod test {
    use apollo_compiler::ast::Argument;
    use apollo_compiler::name;

    use super::*;
    use crate::link::DEFAULT_LINK_NAME;
    use crate::link::link_spec_definition::LINK_DIRECTIVE_FOR_ARGUMENT_NAME;
    use crate::link::link_spec_definition::LINK_DIRECTIVE_URL_ARGUMENT_NAME;
    use crate::schema::position::SchemaDefinitionPosition;
    use crate::subgraph::test_utils::BuildOption;
    use crate::subgraph::test_utils::build_inner_expanded;

    impl JoinSpecDefinition {
        fn link(&self) -> Directive {
            Directive {
                name: DEFAULT_LINK_NAME,
                arguments: vec![
                    Node::new(Argument {
                        name: LINK_DIRECTIVE_URL_ARGUMENT_NAME,
                        value: self.url.to_string().into(),
                    }),
                    Node::new(Argument {
                        name: LINK_DIRECTIVE_FOR_ARGUMENT_NAME,
                        value: Node::new(Value::Enum(name!("EXECUTION"))),
                    }),
                ],
            }
        }
    }

    fn trivial_schema() -> FederationSchema {
        build_inner_expanded("type Query { hello: String }", BuildOption::AsFed2)
            .unwrap()
            .schema()
            .to_owned()
    }

    fn get_schema_with_join(version: Version) -> FederationSchema {
        let mut schema = trivial_schema();
        let join_spec = JOIN_VERSIONS.find(&version).unwrap();
        SchemaDefinitionPosition
            .insert_directive(&mut schema, join_spec.link().into())
            .unwrap();
        join_spec.add_elements_to_schema(&mut schema).unwrap();
        schema
    }

    fn join_spec_directives_snapshot(schema: &FederationSchema) -> String {
        schema
            .schema()
            .directive_definitions
            .iter()
            .filter_map(|(name, def)| {
                if name.as_str().starts_with("join__") {
                    Some(def.to_string())
                } else {
                    None
                }
            })
            .join("\n")
    }

    fn join_spec_types_snapshot(schema: &FederationSchema) -> String {
        schema
            .schema()
            .types
            .iter()
            .filter_map(|(name, ty)| {
                if name.as_str().starts_with("join__") {
                    Some(ty.to_string())
                } else {
                    None
                }
            })
            .join("")
    }

    #[test]
    fn join_spec_v0_1_definitions() {
        let schema = get_schema_with_join(Version { major: 0, minor: 1 });

        insta::assert_snapshot!(join_spec_types_snapshot(&schema), @r#"enum join__Graph
scalar join__FieldSet
"#);
        insta::assert_snapshot!(join_spec_directives_snapshot(&schema), @r#"directive @join__graph(name: String!, url: String!) on ENUM_VALUE
directive @join__type(graph: join__Graph!, key: join__FieldSet) on OBJECT | INTERFACE | UNION | ENUM | INPUT_OBJECT | SCALAR
directive @join__field(graph: join__Graph!, requires: join__FieldSet, provides: join__FieldSet) repeatable on FIELD_DEFINITION | INPUT_FIELD_DEFINITION
directive @join__owner(graph: join__Graph!) on OBJECT
"#);
    }

    #[test]
    fn join_spec_v0_2_definitions() {
        let schema = get_schema_with_join(Version { major: 0, minor: 2 });

        insta::assert_snapshot!(join_spec_types_snapshot(&schema), @r#"enum join__Graph
scalar join__FieldSet
"#);

        insta::assert_snapshot!(join_spec_directives_snapshot(&schema), @r#"directive @join__graph(name: String!, url: String!) on ENUM_VALUE
directive @join__type(graph: join__Graph!, key: join__FieldSet, extension: Boolean! = false, resolvable: Boolean! = true) repeatable on OBJECT | INTERFACE | UNION | ENUM | INPUT_OBJECT | SCALAR
directive @join__field(graph: join__Graph!, requires: join__FieldSet, provides: join__FieldSet, type: String, external: Boolean, override: String, usedOverridden: Boolean) repeatable on FIELD_DEFINITION | INPUT_FIELD_DEFINITION
directive @join__implements(graph: join__Graph!, interface: String!) repeatable on OBJECT | INTERFACE
"#);
    }

    #[test]
    fn join_spec_v0_3_definitions() {
        let schema = get_schema_with_join(Version { major: 0, minor: 3 });

        insta::assert_snapshot!(join_spec_types_snapshot(&schema), @r#"enum join__Graph
scalar join__FieldSet
"#);

        insta::assert_snapshot!(join_spec_directives_snapshot(&schema), @r#"directive @join__graph(name: String!, url: String!) on ENUM_VALUE
directive @join__type(graph: join__Graph!, key: join__FieldSet, extension: Boolean! = false, resolvable: Boolean! = true, isInterfaceObject: Boolean! = false) repeatable on OBJECT | INTERFACE | UNION | ENUM | INPUT_OBJECT | SCALAR
directive @join__field(graph: join__Graph, requires: join__FieldSet, provides: join__FieldSet, type: String, external: Boolean, override: String, usedOverridden: Boolean) repeatable on FIELD_DEFINITION | INPUT_FIELD_DEFINITION
directive @join__implements(graph: join__Graph!, interface: String!) repeatable on OBJECT | INTERFACE
directive @join__unionMember(graph: join__Graph!, member: String!) repeatable on UNION
directive @join__enumValue(graph: join__Graph!) repeatable on ENUM_VALUE
"#);
    }

    #[test]
    fn join_spec_v0_4_definitions() {
        let schema = get_schema_with_join(Version { major: 0, minor: 4 });

        insta::assert_snapshot!(join_spec_types_snapshot(&schema), @r#"enum join__Graph
scalar join__FieldSet
scalar join__DirectiveArguments
"#);

        insta::assert_snapshot!(join_spec_directives_snapshot(&schema), @r#"directive @join__graph(name: String!, url: String!) on ENUM_VALUE
directive @join__type(graph: join__Graph!, key: join__FieldSet, extension: Boolean! = false, resolvable: Boolean! = true, isInterfaceObject: Boolean! = false) repeatable on OBJECT | INTERFACE | UNION | ENUM | INPUT_OBJECT | SCALAR
directive @join__field(graph: join__Graph, requires: join__FieldSet, provides: join__FieldSet, type: String, external: Boolean, override: String, usedOverridden: Boolean, overrideLabel: String) repeatable on FIELD_DEFINITION | INPUT_FIELD_DEFINITION
directive @join__implements(graph: join__Graph!, interface: String!) repeatable on OBJECT | INTERFACE
directive @join__unionMember(graph: join__Graph!, member: String!) repeatable on UNION
directive @join__enumValue(graph: join__Graph!) repeatable on ENUM_VALUE
directive @join__directive(graphs: [join__Graph!], name: String!, args: join__DirectiveArguments) repeatable on SCHEMA | OBJECT | INTERFACE | FIELD_DEFINITION
"#);
    }

    #[test]
    fn join_spec_v0_5_definitions() {
        let schema = get_schema_with_join(Version { major: 0, minor: 5 });

        insta::assert_snapshot!(join_spec_types_snapshot(&schema), @r#"enum join__Graph
scalar join__FieldSet
scalar join__DirectiveArguments
scalar join__FieldValue
input join__ContextArgument {
  name: String!
  type: String!
  context: String!
  selection: join__FieldValue
}
"#);

        insta::assert_snapshot!(join_spec_directives_snapshot(&schema), @r#"directive @join__graph(name: String!, url: String!) on ENUM_VALUE
directive @join__type(graph: join__Graph!, key: join__FieldSet, extension: Boolean! = false, resolvable: Boolean! = true, isInterfaceObject: Boolean! = false) repeatable on OBJECT | INTERFACE | UNION | ENUM | INPUT_OBJECT | SCALAR
directive @join__field(graph: join__Graph, requires: join__FieldSet, provides: join__FieldSet, type: String, external: Boolean, override: String, usedOverridden: Boolean, overrideLabel: String, contextArguments: [join__ContextArgument!]) repeatable on FIELD_DEFINITION | INPUT_FIELD_DEFINITION
directive @join__implements(graph: join__Graph!, interface: String!) repeatable on OBJECT | INTERFACE
directive @join__unionMember(graph: join__Graph!, member: String!) repeatable on UNION
directive @join__enumValue(graph: join__Graph!) repeatable on ENUM_VALUE
directive @join__directive(graphs: [join__Graph!], name: String!, args: join__DirectiveArguments) repeatable on SCHEMA | OBJECT | INTERFACE | FIELD_DEFINITION
"#);
    }
}
