use apollo_compiler::ast::Argument;
use apollo_compiler::name;
use apollo_compiler::schema::Directive;
use apollo_compiler::schema::DirectiveDefinition;
use apollo_compiler::schema::ExtendedType;
use apollo_compiler::schema::UnionType;
use apollo_compiler::schema::Value;
use apollo_compiler::Name;
use apollo_compiler::Node;
use lazy_static::lazy_static;

use crate::error::FederationError;
use crate::error::SingleFederationError;
use crate::link::argument::directive_optional_boolean_argument;
use crate::link::argument::directive_optional_string_argument;
use crate::link::argument::directive_required_string_argument;
use crate::link::cost_spec_definition::CostSpecDefinition;
use crate::link::cost_spec_definition::COST_VERSIONS;
use crate::link::spec::Identity;
use crate::link::spec::Url;
use crate::link::spec::Version;
use crate::link::spec_definition::SpecDefinition;
use crate::link::spec_definition::SpecDefinitions;
use crate::schema::FederationSchema;

pub(crate) const FEDERATION_ENTITY_TYPE_NAME_IN_SPEC: Name = name!("_Entity");
pub(crate) const FEDERATION_KEY_DIRECTIVE_NAME_IN_SPEC: Name = name!("key");
pub(crate) const FEDERATION_INTERFACEOBJECT_DIRECTIVE_NAME_IN_SPEC: Name = name!("interfaceObject");
pub(crate) const FEDERATION_EXTENDS_DIRECTIVE_NAME_IN_SPEC: Name = name!("extends");
pub(crate) const FEDERATION_EXTERNAL_DIRECTIVE_NAME_IN_SPEC: Name = name!("external");
pub(crate) const FEDERATION_REQUIRES_DIRECTIVE_NAME_IN_SPEC: Name = name!("requires");
pub(crate) const FEDERATION_PROVIDES_DIRECTIVE_NAME_IN_SPEC: Name = name!("provides");
pub(crate) const FEDERATION_SHAREABLE_DIRECTIVE_NAME_IN_SPEC: Name = name!("shareable");
pub(crate) const FEDERATION_OVERRIDE_DIRECTIVE_NAME_IN_SPEC: Name = name!("override");

pub(crate) const FEDERATION_FIELDS_ARGUMENT_NAME: Name = name!("fields");
pub(crate) const FEDERATION_RESOLVABLE_ARGUMENT_NAME: Name = name!("resolvable");
pub(crate) const FEDERATION_REASON_ARGUMENT_NAME: Name = name!("reason");
pub(crate) const FEDERATION_FROM_ARGUMENT_NAME: Name = name!("from");
pub(crate) const FEDERATION_OVERRIDE_LABEL_ARGUMENT_NAME: Name = name!("label");

pub(crate) struct KeyDirectiveArguments<'doc> {
    pub(crate) fields: &'doc str,
    pub(crate) resolvable: bool,
}

pub(crate) struct RequiresDirectiveArguments<'doc> {
    pub(crate) fields: &'doc str,
}

pub(crate) struct ProvidesDirectiveArguments<'doc> {
    pub(crate) fields: &'doc str,
}

pub(crate) struct OverrideDirectiveArguments<'doc> {
    pub(crate) from: &'doc str,
    pub(crate) label: Option<&'doc str>,
}

#[derive(Debug)]
pub(crate) struct FederationSpecDefinition {
    url: Url,
}

impl FederationSpecDefinition {
    pub(crate) fn new(version: Version) -> Self {
        Self {
            url: Url {
                identity: Identity::federation_identity(),
                version,
            },
        }
    }

    pub(crate) fn entity_type_definition<'schema>(
        &self,
        schema: &'schema FederationSchema,
    ) -> Result<Option<&'schema Node<UnionType>>, FederationError> {
        // Note that the _Entity type is special in that:
        // 1. Spec renaming doesn't take place for it (there's no prefixing or importing needed),
        //    in order to maintain backwards compatibility with Fed 1.
        // 2. Its presence is optional; if absent, it means the subgraph has no resolvable keys.
        match schema
            .schema()
            .types
            .get(&FEDERATION_ENTITY_TYPE_NAME_IN_SPEC)
        {
            Some(ExtendedType::Union(type_)) => Ok(Some(type_)),
            None => Ok(None),
            _ => Err(SingleFederationError::Internal {
                message: format!(
                    "Unexpectedly found non-union for federation spec's \"{}\" type definition",
                    FEDERATION_ENTITY_TYPE_NAME_IN_SPEC
                ),
            }
            .into()),
        }
    }

    pub(crate) fn key_directive_definition<'schema>(
        &self,
        schema: &'schema FederationSchema,
    ) -> Result<&'schema Node<DirectiveDefinition>, FederationError> {
        self.directive_definition(schema, &FEDERATION_KEY_DIRECTIVE_NAME_IN_SPEC)?
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
        fields: &str,
        resolvable: bool,
    ) -> Result<Directive, FederationError> {
        let name_in_schema = self
            .directive_name_in_schema(schema, &FEDERATION_KEY_DIRECTIVE_NAME_IN_SPEC)?
            .ok_or_else(|| SingleFederationError::Internal {
                message: "Unexpectedly could not find federation spec in schema".to_owned(),
            })?;
        Ok(Directive {
            name: name_in_schema,
            arguments: vec![
                Node::new(Argument {
                    name: FEDERATION_FIELDS_ARGUMENT_NAME,
                    value: Node::new(Value::String(fields.to_owned())),
                }),
                Node::new(Argument {
                    name: FEDERATION_RESOLVABLE_ARGUMENT_NAME,
                    value: Node::new(Value::Boolean(resolvable)),
                }),
            ],
        })
    }

    pub(crate) fn key_directive_arguments<'doc>(
        &self,
        application: &'doc Node<Directive>,
    ) -> Result<KeyDirectiveArguments<'doc>, FederationError> {
        Ok(KeyDirectiveArguments {
            fields: directive_required_string_argument(
                application,
                &FEDERATION_FIELDS_ARGUMENT_NAME,
            )?,
            resolvable: directive_optional_boolean_argument(
                application,
                &FEDERATION_RESOLVABLE_ARGUMENT_NAME,
            )?
            .unwrap_or(false),
        })
    }

    pub(crate) fn interface_object_directive_definition<'schema>(
        &self,
        schema: &'schema FederationSchema,
    ) -> Result<Option<&'schema Node<DirectiveDefinition>>, FederationError> {
        if *self.version() < (Version { major: 2, minor: 3 }) {
            return Ok(None);
        }
        self.directive_definition(schema, &FEDERATION_INTERFACEOBJECT_DIRECTIVE_NAME_IN_SPEC)?
            .ok_or_else(|| {
                SingleFederationError::Internal {
                    message: format!(
                        "Unexpectedly could not find federation spec's \"@{}\" directive definition",
                        FEDERATION_INTERFACEOBJECT_DIRECTIVE_NAME_IN_SPEC
                    ),
                }.into()
            })
            .map(Some)
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
            .directive_name_in_schema(schema, &FEDERATION_INTERFACEOBJECT_DIRECTIVE_NAME_IN_SPEC)?
            .ok_or_else(|| SingleFederationError::Internal {
                message: "Unexpectedly could not find federation spec in schema".to_owned(),
            })?;
        Ok(Directive {
            name: name_in_schema,
            arguments: Vec::new(),
        })
    }

    pub(crate) fn extends_directive_definition<'schema>(
        &self,
        schema: &'schema FederationSchema,
    ) -> Result<&'schema Node<DirectiveDefinition>, FederationError> {
        self.directive_definition(schema, &FEDERATION_EXTENDS_DIRECTIVE_NAME_IN_SPEC)?
            .ok_or_else(|| {
                FederationError::internal(format!(
                    "Unexpectedly could not find federation spec's \"@{}\" directive definition",
                    FEDERATION_EXTENDS_DIRECTIVE_NAME_IN_SPEC
                ))
            })
    }

    pub(crate) fn external_directive_definition<'schema>(
        &self,
        schema: &'schema FederationSchema,
    ) -> Result<&'schema Node<DirectiveDefinition>, FederationError> {
        self.directive_definition(schema, &FEDERATION_EXTERNAL_DIRECTIVE_NAME_IN_SPEC)?
            .ok_or_else(|| {
                SingleFederationError::Internal {
                    message: format!(
                        "Unexpectedly could not find federation spec's \"@{}\" directive definition",
                        FEDERATION_EXTERNAL_DIRECTIVE_NAME_IN_SPEC
                    ),
                }.into()
            })
    }

    pub(crate) fn external_directive(
        &self,
        schema: &FederationSchema,
        reason: Option<String>,
    ) -> Result<Directive, FederationError> {
        let name_in_schema = self
            .directive_name_in_schema(schema, &FEDERATION_EXTERNAL_DIRECTIVE_NAME_IN_SPEC)?
            .ok_or_else(|| SingleFederationError::Internal {
                message: "Unexpectedly could not find federation spec in schema".to_owned(),
            })?;
        let mut arguments = Vec::new();
        if let Some(reason) = reason {
            arguments.push(Node::new(Argument {
                name: FEDERATION_REASON_ARGUMENT_NAME,
                value: Node::new(Value::String(reason)),
            }));
        }
        Ok(Directive {
            name: name_in_schema,
            arguments,
        })
    }

    pub(crate) fn requires_directive_definition<'schema>(
        &self,
        schema: &'schema FederationSchema,
    ) -> Result<&'schema Node<DirectiveDefinition>, FederationError> {
        self.directive_definition(schema, &FEDERATION_REQUIRES_DIRECTIVE_NAME_IN_SPEC)?
            .ok_or_else(|| {
                SingleFederationError::Internal {
                    message: format!(
                        "Unexpectedly could not find federation spec's \"@{}\" directive definition",
                        FEDERATION_REQUIRES_DIRECTIVE_NAME_IN_SPEC
                    ),
                }.into()
            })
    }

    pub(crate) fn requires_directive(
        &self,
        schema: &FederationSchema,
        fields: String,
    ) -> Result<Directive, FederationError> {
        let name_in_schema = self
            .directive_name_in_schema(schema, &FEDERATION_REQUIRES_DIRECTIVE_NAME_IN_SPEC)?
            .ok_or_else(|| SingleFederationError::Internal {
                message: "Unexpectedly could not find federation spec in schema".to_owned(),
            })?;
        Ok(Directive {
            name: name_in_schema,
            arguments: vec![Node::new(Argument {
                name: FEDERATION_FIELDS_ARGUMENT_NAME,
                value: Node::new(Value::String(fields)),
            })],
        })
    }

    pub(crate) fn requires_directive_arguments<'doc>(
        &self,
        application: &'doc Node<Directive>,
    ) -> Result<RequiresDirectiveArguments<'doc>, FederationError> {
        Ok(RequiresDirectiveArguments {
            fields: directive_required_string_argument(
                application,
                &FEDERATION_FIELDS_ARGUMENT_NAME,
            )?,
        })
    }

    pub(crate) fn provides_directive_definition<'schema>(
        &self,
        schema: &'schema FederationSchema,
    ) -> Result<&'schema Node<DirectiveDefinition>, FederationError> {
        self.directive_definition(schema, &FEDERATION_PROVIDES_DIRECTIVE_NAME_IN_SPEC)?
            .ok_or_else(|| {
                SingleFederationError::Internal {
                    message: format!(
                        "Unexpectedly could not find federation spec's \"@{}\" directive definition",
                        FEDERATION_PROVIDES_DIRECTIVE_NAME_IN_SPEC
                    ),
                }.into()
            })
    }

    pub(crate) fn provides_directive(
        &self,
        schema: &FederationSchema,
        fields: String,
    ) -> Result<Directive, FederationError> {
        let name_in_schema = self
            .directive_name_in_schema(schema, &FEDERATION_PROVIDES_DIRECTIVE_NAME_IN_SPEC)?
            .ok_or_else(|| SingleFederationError::Internal {
                message: "Unexpectedly could not find federation spec in schema".to_owned(),
            })?;
        Ok(Directive {
            name: name_in_schema,
            arguments: vec![Node::new(Argument {
                name: FEDERATION_FIELDS_ARGUMENT_NAME,
                value: Node::new(Value::String(fields)),
            })],
        })
    }

    pub(crate) fn provides_directive_arguments<'doc>(
        &self,
        application: &'doc Node<Directive>,
    ) -> Result<ProvidesDirectiveArguments<'doc>, FederationError> {
        Ok(ProvidesDirectiveArguments {
            fields: directive_required_string_argument(
                application,
                &FEDERATION_FIELDS_ARGUMENT_NAME,
            )?,
        })
    }

    pub(crate) fn shareable_directive_definition<'schema>(
        &self,
        schema: &'schema FederationSchema,
    ) -> Result<&'schema Node<DirectiveDefinition>, FederationError> {
        self.directive_definition(schema, &FEDERATION_SHAREABLE_DIRECTIVE_NAME_IN_SPEC)?
            .ok_or_else(|| {
                FederationError::internal(format!(
                    "Unexpectedly could not find federation spec's \"@{}\" directive definition",
                    FEDERATION_SHAREABLE_DIRECTIVE_NAME_IN_SPEC
                ))
            })
    }

    pub(crate) fn shareable_directive(
        &self,
        schema: &FederationSchema,
    ) -> Result<Directive, FederationError> {
        let name_in_schema = self
            .directive_name_in_schema(schema, &FEDERATION_SHAREABLE_DIRECTIVE_NAME_IN_SPEC)?
            .ok_or_else(|| SingleFederationError::Internal {
                message: "Unexpectedly could not find federation spec in schema".to_owned(),
            })?;
        Ok(Directive {
            name: name_in_schema,
            arguments: Vec::new(),
        })
    }

    pub(crate) fn override_directive_definition<'schema>(
        &self,
        schema: &'schema FederationSchema,
    ) -> Result<&'schema Node<DirectiveDefinition>, FederationError> {
        self.directive_definition(schema, &FEDERATION_OVERRIDE_DIRECTIVE_NAME_IN_SPEC)?
            .ok_or_else(|| {
                FederationError::internal(format!(
                    "Unexpectedly could not find federation spec's \"@{}\" directive definition",
                    FEDERATION_OVERRIDE_DIRECTIVE_NAME_IN_SPEC
                ))
            })
    }

    pub(crate) fn override_directive(
        &self,
        schema: &FederationSchema,
        from: String,
        label: &Option<&str>,
    ) -> Result<Directive, FederationError> {
        let name_in_schema = self
            .directive_name_in_schema(schema, &FEDERATION_OVERRIDE_DIRECTIVE_NAME_IN_SPEC)?
            .ok_or_else(|| SingleFederationError::Internal {
                message: "Unexpectedly could not find federation spec in schema".to_owned(),
            })?;

        let mut arguments = vec![Node::new(Argument {
            name: FEDERATION_FROM_ARGUMENT_NAME,
            value: Node::new(Value::String(from)),
        })];

        if let Some(label) = label {
            arguments.push(Node::new(Argument {
                name: FEDERATION_OVERRIDE_LABEL_ARGUMENT_NAME,
                value: Node::new(Value::String(label.to_string())),
            }));
        }
        Ok(Directive {
            name: name_in_schema,
            arguments,
        })
    }

    pub(crate) fn override_directive_arguments<'doc>(
        &self,
        application: &'doc Node<Directive>,
    ) -> Result<OverrideDirectiveArguments<'doc>, FederationError> {
        Ok(OverrideDirectiveArguments {
            from: directive_required_string_argument(application, &FEDERATION_FROM_ARGUMENT_NAME)?,
            label: directive_optional_string_argument(
                application,
                &FEDERATION_OVERRIDE_LABEL_ARGUMENT_NAME,
            )?,
        })
    }

    pub(crate) fn get_cost_spec_definition(
        &self,
        schema: &FederationSchema,
    ) -> Option<&'static CostSpecDefinition> {
        schema
            .metadata()
            .and_then(|metadata| metadata.for_identity(&Identity::cost_identity()))
            .and_then(|link| COST_VERSIONS.find(&link.url.version))
            .or_else(|| COST_VERSIONS.find_for_federation_version(self.version()))
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
    pub(crate) static ref FEDERATION_VERSIONS: SpecDefinitions<FederationSpecDefinition> = {
        let mut definitions = SpecDefinitions::new(Identity::federation_identity());
        definitions.add(FederationSpecDefinition::new(Version {
            major: 2,
            minor: 0,
        }));
        definitions.add(FederationSpecDefinition::new(Version {
            major: 2,
            minor: 1,
        }));
        definitions.add(FederationSpecDefinition::new(Version {
            major: 2,
            minor: 2,
        }));
        definitions.add(FederationSpecDefinition::new(Version {
            major: 2,
            minor: 3,
        }));
        definitions.add(FederationSpecDefinition::new(Version {
            major: 2,
            minor: 4,
        }));
        definitions.add(FederationSpecDefinition::new(Version {
            major: 2,
            minor: 5,
        }));
        definitions.add(FederationSpecDefinition::new(Version {
            major: 2,
            minor: 6,
        }));
        definitions.add(FederationSpecDefinition::new(Version {
            major: 2,
            minor: 7,
        }));
        definitions.add(FederationSpecDefinition::new(Version {
            major: 2,
            minor: 8,
        }));
        definitions.add(FederationSpecDefinition::new(Version {
            major: 2,
            minor: 9,
        }));
        definitions
    };
}

pub(crate) fn get_federation_spec_definition_from_subgraph(
    schema: &FederationSchema,
) -> Result<&'static FederationSpecDefinition, FederationError> {
    let federation_link = schema
        .metadata()
        .as_ref()
        .and_then(|metadata| metadata.for_identity(&Identity::federation_identity()))
        .ok_or_else(|| SingleFederationError::Internal {
            message: "Subgraph unexpectedly does not use federation spec".to_owned(),
        })?;
    Ok(FEDERATION_VERSIONS
        .find(&federation_link.url.version)
        .ok_or_else(|| SingleFederationError::Internal {
            message: "Subgraph unexpectedly does not use a supported federation spec version"
                .to_owned(),
        })?)
}
