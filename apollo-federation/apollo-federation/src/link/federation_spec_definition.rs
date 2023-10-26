use crate::error::{FederationError, SingleFederationError};
use crate::link::spec::{Identity, Url, Version};
use crate::link::spec_definition::{SpecDefinition, SpecDefinitions};
use crate::schema::FederationSchema;
use apollo_compiler::ast::Argument;
use apollo_compiler::schema::{Directive, DirectiveDefinition, Value};
use apollo_compiler::{Node, NodeStr};
use lazy_static::lazy_static;

pub(crate) const FEDERATION_KEY_DIRECTIVE_NAME_IN_SPEC: &str = "key";
pub(crate) const FEDERATION_INTERFACEOBJECT_DIRECTIVE_NAME_IN_SPEC: &str = "interfaceObject";
pub(crate) const FEDERATION_EXTERNAL_DIRECTIVE_NAME_IN_SPEC: &str = "external";
pub(crate) const FEDERATION_REQUIRES_DIRECTIVE_NAME_IN_SPEC: &str = "requires";
pub(crate) const FEDERATION_PROVIDES_DIRECTIVE_NAME_IN_SPEC: &str = "provides";
pub(crate) const FEDERATION_SHAREABLE_DIRECTIVE_NAME_IN_SPEC: &str = "shareable";
pub(crate) const FEDERATION_OVERRIDE_DIRECTIVE_NAME_IN_SPEC: &str = "override";

pub(crate) const FEDERATION_FIELDS_ARGUMENT_NAME: &str = "fields";
pub(crate) const FEDERATION_RESOLVABLE_ARGUMENT_NAME: &str = "resolvable";
pub(crate) const FEDERATION_REASON_ARGUMENT_NAME: &str = "reason";
pub(crate) const FEDERATION_FROM_ARGUMENT_NAME: &str = "from";

pub(crate) struct FederationSpecDefinition {
    url: Url,
}

impl FederationSpecDefinition {
    pub(crate) fn new(version: Version) -> Self {
        Self {
            url: Url {
                identity: Identity::join_identity(),
                version,
            },
        }
    }

    pub(crate) fn key_directive_definition<'schema>(
        &self,
        schema: &'schema FederationSchema,
    ) -> Result<&'schema Node<DirectiveDefinition>, FederationError> {
        self.directive_definition(schema, FEDERATION_KEY_DIRECTIVE_NAME_IN_SPEC)?
            .ok_or_else(|| {
                SingleFederationError::Internal {
                    message: format!(
                        "Unexpectedly could not find federation spec's \"@{}\" directive definition",
                        FEDERATION_KEY_DIRECTIVE_NAME_IN_SPEC
                    ),
                }
                .into()
            })
    }

    pub(crate) fn key_directive(
        &self,
        schema: &FederationSchema,
        fields: NodeStr,
        resolvable: bool,
    ) -> Result<Directive, FederationError> {
        let name_in_schema = self
            .directive_name_in_schema(schema, FEDERATION_KEY_DIRECTIVE_NAME_IN_SPEC)?
            .ok_or_else(|| SingleFederationError::Internal {
                message: "Unexpectedly could not find federation spec in schema".to_owned(),
            })?;
        Ok(Directive {
            name: NodeStr::new(&name_in_schema),
            arguments: vec![
                Node::new(Argument {
                    name: NodeStr::new(FEDERATION_FIELDS_ARGUMENT_NAME),
                    value: Node::new(Value::String(fields)),
                }),
                Node::new(Argument {
                    name: NodeStr::new(FEDERATION_RESOLVABLE_ARGUMENT_NAME),
                    value: Node::new(Value::Boolean(resolvable)),
                }),
            ],
        })
    }

    pub(crate) fn interface_object_directive(
        &self,
        schema: &FederationSchema,
    ) -> Result<Directive, FederationError> {
        if *self.version() < (Version { major: 2, minor: 3 }) {
            return Err(SingleFederationError::Internal {
                message: "Must be using federation >= v2.3 to use interface object".to_owned(),
            }
            .into());
        }
        let name_in_schema = self
            .directive_name_in_schema(schema, FEDERATION_INTERFACEOBJECT_DIRECTIVE_NAME_IN_SPEC)?
            .ok_or_else(|| SingleFederationError::Internal {
                message: "Unexpectedly could not find federation spec in schema".to_owned(),
            })?;
        Ok(Directive {
            name: NodeStr::new(&name_in_schema),
            arguments: Vec::new(),
        })
    }

    pub(crate) fn external_directive(
        &self,
        schema: &FederationSchema,
        reason: Option<NodeStr>,
    ) -> Result<Directive, FederationError> {
        let name_in_schema = self
            .directive_name_in_schema(schema, FEDERATION_EXTERNAL_DIRECTIVE_NAME_IN_SPEC)?
            .ok_or_else(|| SingleFederationError::Internal {
                message: "Unexpectedly could not find federation spec in schema".to_owned(),
            })?;
        let mut arguments = Vec::new();
        if let Some(reason) = reason {
            arguments.push(Node::new(Argument {
                name: NodeStr::new(FEDERATION_REASON_ARGUMENT_NAME),
                value: Node::new(Value::String(reason)),
            }))
        }
        Ok(Directive {
            name: NodeStr::new(&name_in_schema),
            arguments,
        })
    }

    pub(crate) fn requires_directive(
        &self,
        schema: &FederationSchema,
        fields: NodeStr,
    ) -> Result<Directive, FederationError> {
        let name_in_schema = self
            .directive_name_in_schema(schema, FEDERATION_REQUIRES_DIRECTIVE_NAME_IN_SPEC)?
            .ok_or_else(|| SingleFederationError::Internal {
                message: "Unexpectedly could not find federation spec in schema".to_owned(),
            })?;
        Ok(Directive {
            name: NodeStr::new(&name_in_schema),
            arguments: vec![Node::new(Argument {
                name: NodeStr::new(FEDERATION_FIELDS_ARGUMENT_NAME),
                value: Node::new(Value::String(fields)),
            })],
        })
    }

    pub(crate) fn provides_directive(
        &self,
        schema: &FederationSchema,
        fields: NodeStr,
    ) -> Result<Directive, FederationError> {
        let name_in_schema = self
            .directive_name_in_schema(schema, FEDERATION_PROVIDES_DIRECTIVE_NAME_IN_SPEC)?
            .ok_or_else(|| SingleFederationError::Internal {
                message: "Unexpectedly could not find federation spec in schema".to_owned(),
            })?;
        Ok(Directive {
            name: NodeStr::new(&name_in_schema),
            arguments: vec![Node::new(Argument {
                name: NodeStr::new(FEDERATION_FIELDS_ARGUMENT_NAME),
                value: Node::new(Value::String(fields)),
            })],
        })
    }

    pub(crate) fn shareable_directive(
        &self,
        schema: &FederationSchema,
    ) -> Result<Directive, FederationError> {
        let name_in_schema = self
            .directive_name_in_schema(schema, FEDERATION_SHAREABLE_DIRECTIVE_NAME_IN_SPEC)?
            .ok_or_else(|| SingleFederationError::Internal {
                message: "Unexpectedly could not find federation spec in schema".to_owned(),
            })?;
        Ok(Directive {
            name: NodeStr::new(&name_in_schema),
            arguments: Vec::new(),
        })
    }

    pub(crate) fn override_directive(
        &self,
        schema: &FederationSchema,
        from: NodeStr,
    ) -> Result<Directive, FederationError> {
        let name_in_schema = self
            .directive_name_in_schema(schema, FEDERATION_OVERRIDE_DIRECTIVE_NAME_IN_SPEC)?
            .ok_or_else(|| SingleFederationError::Internal {
                message: "Unexpectedly could not find federation spec in schema".to_owned(),
            })?;
        Ok(Directive {
            name: NodeStr::new(&name_in_schema),
            arguments: vec![Node::new(Argument {
                name: NodeStr::new(FEDERATION_FROM_ARGUMENT_NAME),
                value: Node::new(Value::String(from)),
            })],
        })
    }
}

impl SpecDefinition for FederationSpecDefinition {
    fn url(&self) -> &Url {
        &self.url
    }

    fn minimum_federation_version(&self) -> Option<&Version> {
        None
    }
}

lazy_static! {
    pub(crate) static ref FEDERATION_VERSIONS: Result<SpecDefinitions<FederationSpecDefinition>, FederationError> = {
        let mut definitions = SpecDefinitions::new(Identity::federation_identity());
        definitions.add(FederationSpecDefinition::new(Version {
            major: 2,
            minor: 0,
        }))?;
        definitions.add(FederationSpecDefinition::new(Version {
            major: 2,
            minor: 1,
        }))?;
        definitions.add(FederationSpecDefinition::new(Version {
            major: 2,
            minor: 2,
        }))?;
        definitions.add(FederationSpecDefinition::new(Version {
            major: 2,
            minor: 3,
        }))?;
        definitions.add(FederationSpecDefinition::new(Version {
            major: 2,
            minor: 4,
        }))?;
        definitions.add(FederationSpecDefinition::new(Version {
            major: 2,
            minor: 5,
        }))?;
        Ok(definitions)
    };
}
