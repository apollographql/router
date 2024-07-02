use apollo_compiler::name;
use apollo_compiler::schema::Directive;
use apollo_compiler::schema::DirectiveDefinition;
use apollo_compiler::schema::EnumType;
use apollo_compiler::schema::ExtendedType;
use apollo_compiler::Name;
use apollo_compiler::Node;
use lazy_static::lazy_static;

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

pub(crate) const JOIN_GRAPH_ENUM_NAME_IN_SPEC: Name = name!("Graph");
pub(crate) const JOIN_GRAPH_DIRECTIVE_NAME_IN_SPEC: Name = name!("graph");
pub(crate) const JOIN_TYPE_DIRECTIVE_NAME_IN_SPEC: Name = name!("type");
pub(crate) const JOIN_FIELD_DIRECTIVE_NAME_IN_SPEC: Name = name!("field");
pub(crate) const JOIN_IMPLEMENTS_DIRECTIVE_NAME_IN_SPEC: Name = name!("implements");
pub(crate) const JOIN_UNIONMEMBER_DIRECTIVE_NAME_IN_SPEC: Name = name!("unionMember");
pub(crate) const JOIN_ENUMVALUE_DIRECTIVE_NAME_IN_SPEC: Name = name!("enumValue");

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
pub(crate) const JOIN_USEROVERRIDDEN_ARGUMENT_NAME: Name = name!("usedOverridden");
pub(crate) const JOIN_INTERFACE_ARGUMENT_NAME: Name = name!("interface");
pub(crate) const JOIN_MEMBER_ARGUMENT_NAME: Name = name!("member");

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

pub(crate) struct FieldDirectiveArguments<'doc> {
    pub(crate) graph: Option<Name>,
    pub(crate) requires: Option<&'doc str>,
    pub(crate) provides: Option<&'doc str>,
    pub(crate) type_: Option<&'doc str>,
    pub(crate) external: Option<bool>,
    pub(crate) override_: Option<&'doc str>,
    pub(crate) user_overridden: Option<bool>,
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
    minimum_federation_version: Option<Version>,
}

impl JoinSpecDefinition {
    pub(crate) fn new(version: Version, minimum_federation_version: Option<Version>) -> Self {
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
        if let ExtendedType::Enum(ref type_) = type_ {
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
            user_overridden: directive_optional_boolean_argument(
                application,
                &JOIN_USEROVERRIDDEN_ARGUMENT_NAME,
            )?,
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
}

impl SpecDefinition for JoinSpecDefinition {
    fn url(&self) -> &Url {
        &self.url
    }

    fn minimum_federation_version(&self) -> Option<&Version> {
        self.minimum_federation_version.as_ref()
    }
}

lazy_static! {
    pub(crate) static ref JOIN_VERSIONS: SpecDefinitions<JoinSpecDefinition> = {
        let mut definitions = SpecDefinitions::new(Identity::join_identity());
        definitions.add(JoinSpecDefinition::new(
            Version { major: 0, minor: 1 },
            None,
        ));
        definitions.add(JoinSpecDefinition::new(
            Version { major: 0, minor: 2 },
            None,
        ));
        definitions.add(JoinSpecDefinition::new(
            Version { major: 0, minor: 3 },
            Some(Version { major: 2, minor: 0 }),
        ));
        definitions.add(JoinSpecDefinition::new(
            Version { major: 0, minor: 4 },
            Some(Version { major: 2, minor: 7 }),
        ));
        definitions.add(JoinSpecDefinition::new(
            Version { major: 0, minor: 5 },
            Some(Version { major: 2, minor: 8 }),
        ));
        definitions
    };
}
