use std::sync::Arc;
use std::sync::LazyLock;

use apollo_compiler::Name;
use apollo_compiler::Node;
use apollo_compiler::ast::Argument;
use apollo_compiler::ast::DirectiveLocation;
use apollo_compiler::ast::Type;
use apollo_compiler::name;
use apollo_compiler::schema::Directive;
use apollo_compiler::schema::DirectiveDefinition;
use apollo_compiler::schema::ExtendedType;
use apollo_compiler::schema::UnionType;
use apollo_compiler::schema::Value;
use apollo_compiler::ty;

use crate::ContextSpecDefinition;
use crate::error::FederationError;
use crate::error::SingleFederationError;
use crate::internal_error;
use crate::link;
use crate::link::argument::directive_optional_boolean_argument;
use crate::link::argument::directive_optional_string_argument;
use crate::link::argument::directive_required_string_argument;
use crate::link::authenticated_spec_definition::AUTHENTICATED_VERSIONS;
use crate::link::cost_spec_definition::COST_VERSIONS;
use crate::link::inaccessible_spec_definition::INACCESSIBLE_VERSIONS;
use crate::link::policy_spec_definition::POLICY_VERSIONS;
use crate::link::requires_scopes_spec_definition::REQUIRES_SCOPES_VERSIONS;
use crate::link::spec::Identity;
use crate::link::spec::Url;
use crate::link::spec::Version;
use crate::link::spec_definition::SpecDefinition;
use crate::link::spec_definition::SpecDefinitions;
use crate::link::tag_spec_definition::TAG_VERSIONS;
use crate::schema::FederationSchema;
use crate::schema::type_and_directive_specification::ArgumentSpecification;
use crate::schema::type_and_directive_specification::DirectiveArgumentSpecification;
use crate::schema::type_and_directive_specification::DirectiveSpecification;
use crate::schema::type_and_directive_specification::ScalarTypeSpecification;
use crate::schema::type_and_directive_specification::TypeAndDirectiveSpecification;

pub(crate) const FEDERATION_ANY_TYPE_NAME_IN_SPEC: Name = name!("_Any");
pub(crate) const FEDERATION_CACHE_TAG_DIRECTIVE_NAME_IN_SPEC: Name = name!("cacheTag");
pub(crate) const FEDERATION_ENTITY_TYPE_NAME_IN_SPEC: Name = name!("_Entity");
pub(crate) const FEDERATION_SERVICE_TYPE_NAME_IN_SPEC: Name = name!("_Service");
pub(crate) const FEDERATION_KEY_DIRECTIVE_NAME_IN_SPEC: Name = name!("key");
pub(crate) const FEDERATION_INTERFACEOBJECT_DIRECTIVE_NAME_IN_SPEC: Name = name!("interfaceObject");
pub(crate) const FEDERATION_EXTENDS_DIRECTIVE_NAME_IN_SPEC: Name = name!("extends");
pub(crate) const FEDERATION_EXTERNAL_DIRECTIVE_NAME_IN_SPEC: Name = name!("external");
pub(crate) const FEDERATION_REQUIRES_DIRECTIVE_NAME_IN_SPEC: Name = name!("requires");
pub(crate) const FEDERATION_PROVIDES_DIRECTIVE_NAME_IN_SPEC: Name = name!("provides");
pub(crate) const FEDERATION_SHAREABLE_DIRECTIVE_NAME_IN_SPEC: Name = name!("shareable");
pub(crate) const FEDERATION_OVERRIDE_DIRECTIVE_NAME_IN_SPEC: Name = name!("override");
pub(crate) const FEDERATION_CONTEXT_DIRECTIVE_NAME_IN_SPEC: Name = name!("context");
pub(crate) const FEDERATION_FROM_CONTEXT_DIRECTIVE_NAME_IN_SPEC: Name = name!("fromContext");
pub(crate) const FEDERATION_TAG_DIRECTIVE_NAME_IN_SPEC: Name = name!("tag");
pub(crate) const FEDERATION_COMPOSEDIRECTIVE_DIRECTIVE_NAME_IN_SPEC: Name =
    name!("composeDirective");

pub(crate) const FEDERATION_FIELDSET_TYPE_NAME_IN_SPEC: Name = name!("FieldSet");
pub(crate) const FEDERATION_FIELDS_ARGUMENT_NAME: Name = name!("fields");
pub(crate) const FEDERATION_FORMAT_ARGUMENT_NAME: Name = name!("format");
pub(crate) const FEDERATION_RESOLVABLE_ARGUMENT_NAME: Name = name!("resolvable");
pub(crate) const FEDERATION_REASON_ARGUMENT_NAME: Name = name!("reason");
pub(crate) const FEDERATION_FROM_ARGUMENT_NAME: Name = name!("from");
pub(crate) const FEDERATION_OVERRIDE_LABEL_ARGUMENT_NAME: Name = name!("label");
pub(crate) const FEDERATION_USED_OVERRIDEN_ARGUMENT_NAME: Name = name!("usedOverridden");
pub(crate) const FEDERATION_CONTEXT_ARGUMENT_NAME: Name = name!("contextArguments");
pub(crate) const FEDERATION_SELECTION_ARGUMENT_NAME: Name = name!("selection");
pub(crate) const FEDERATION_TYPE_ARGUMENT_NAME: Name = name!("type");
pub(crate) const FEDERATION_GRAPH_ARGUMENT_NAME: Name = name!("graph");
pub(crate) const FEDERATION_NAME_ARGUMENT_NAME: Name = name!("name");
pub(crate) const FEDERATION_FIELD_ARGUMENT_NAME: Name = name!("field");

pub(crate) const FEDERATION_OPERATION_TYPES: [Name; 3] = [
    FEDERATION_ANY_TYPE_NAME_IN_SPEC,
    FEDERATION_ENTITY_TYPE_NAME_IN_SPEC,
    FEDERATION_SERVICE_TYPE_NAME_IN_SPEC,
];

pub(crate) struct KeyDirectiveArguments<'doc> {
    pub(crate) fields: &'doc str,
    pub(crate) resolvable: bool,
}

pub(crate) struct ExternalDirectiveArguments<'doc> {
    pub(crate) reason: Option<&'doc str>,
}

pub(crate) struct RequiresDirectiveArguments<'doc> {
    pub(crate) fields: &'doc str,
}

pub(crate) struct TagDirectiveArguments<'doc> {
    pub(crate) name: &'doc str,
}

pub(crate) struct ProvidesDirectiveArguments<'doc> {
    pub(crate) fields: &'doc str,
}

pub(crate) struct ContextDirectiveArguments<'doc> {
    pub(crate) name: &'doc str,
}

pub(crate) struct FromContextDirectiveArguments<'doc> {
    pub(crate) field: &'doc str,
}

pub(crate) struct OverrideDirectiveArguments<'doc> {
    pub(crate) from: &'doc str,
    pub(crate) label: Option<&'doc str>,
}

pub(crate) struct CacheTagDirectiveArguments<'doc> {
    pub(crate) format: &'doc str,
}

#[derive(Clone)]
pub(crate) struct ComposeDirectiveArguments<'doc> {
    pub(crate) name: &'doc str,
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

    // PORT_NOTE: a port of `federationSpec` from JS
    pub(crate) fn for_version(version: &Version) -> Result<&'static Self, FederationError> {
        FEDERATION_VERSIONS
            .find(version)
            .ok_or_else(|| internal_error!("Unknown Federation spec version: {version}"))
    }

    // PORT_NOTE: a port of `latestFederationSpec`, which is defined as `federationSpec()` in JS.
    pub(crate) fn latest() -> &'static Self {
        // Note: The `unwrap()` calls won't panic, since `FEDERATION_VERSIONS` will always have at
        // least one version.
        let latest_version = FEDERATION_VERSIONS.versions().last().unwrap();
        Self::for_version(latest_version).unwrap()
    }

    /// Some users rely on auto-expanding fed v1 graphs with fed v2 directives. While technically
    /// we should only expand @tag directive from v2 definitions, we will continue expanding other
    /// directives (up to v2.4) to ensure backwards compatibility.
    pub(crate) fn auto_expanded_federation_spec() -> &'static Self {
        Self::for_version(&Version { major: 2, minor: 4 }).unwrap()
    }

    pub(crate) fn is_fed1(&self) -> bool {
        self.version().satisfies(&Version { major: 1, minor: 0 })
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
                    "Unexpectedly found non-union for federation spec's \"{FEDERATION_ENTITY_TYPE_NAME_IN_SPEC}\" type definition"
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
                        "Unexpectedly could not find federation spec's \"@{FEDERATION_KEY_DIRECTIVE_NAME_IN_SPEC}\" directive definition"
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
            .unwrap_or(Self::resolvable_argument_default_value()),
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
                        "Unexpectedly could not find federation spec's \"@{FEDERATION_INTERFACEOBJECT_DIRECTIVE_NAME_IN_SPEC}\" directive definition"
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
                    "Unexpectedly could not find federation spec's \"@{FEDERATION_EXTENDS_DIRECTIVE_NAME_IN_SPEC}\" directive definition"
                ))
            })
    }

    pub(crate) fn external_directive_name_in_schema(
        &self,
        schema: &FederationSchema,
    ) -> Result<Option<Name>, FederationError> {
        self.directive_name_in_schema(schema, &FEDERATION_EXTERNAL_DIRECTIVE_NAME_IN_SPEC)
    }

    pub(crate) fn external_directive_definition<'schema>(
        &self,
        schema: &'schema FederationSchema,
    ) -> Result<&'schema Node<DirectiveDefinition>, FederationError> {
        self.directive_definition(schema, &FEDERATION_EXTERNAL_DIRECTIVE_NAME_IN_SPEC)?
            .ok_or_else(|| {
                SingleFederationError::Internal {
                    message: format!(
                        "Unexpectedly could not find federation spec's \"@{FEDERATION_EXTERNAL_DIRECTIVE_NAME_IN_SPEC}\" directive definition"
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
            .external_directive_name_in_schema(schema)?
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

    pub(crate) fn external_directive_arguments<'doc>(
        &self,
        application: &'doc Node<Directive>,
    ) -> Result<ExternalDirectiveArguments<'doc>, FederationError> {
        Ok(ExternalDirectiveArguments {
            reason: directive_optional_string_argument(
                application,
                &FEDERATION_REASON_ARGUMENT_NAME,
            )?,
        })
    }

    pub(crate) fn tag_directive_definition<'schema>(
        &self,
        schema: &'schema FederationSchema,
    ) -> Result<&'schema Node<DirectiveDefinition>, FederationError> {
        self.directive_definition(schema, &FEDERATION_TAG_DIRECTIVE_NAME_IN_SPEC)?
            .ok_or_else(|| {
                SingleFederationError::Internal {
                    message: format!(
                        "Unexpectedly could not find federation spec's \"@{FEDERATION_TAG_DIRECTIVE_NAME_IN_SPEC}\" directive definition"
                    ),
                }.into()
            })
    }

    #[allow(unused)]
    pub(crate) fn tag_directive(
        &self,
        schema: &FederationSchema,
        name: String,
    ) -> Result<Directive, FederationError> {
        let name_in_schema = self
            .directive_name_in_schema(schema, &FEDERATION_TAG_DIRECTIVE_NAME_IN_SPEC)?
            .ok_or_else(|| SingleFederationError::Internal {
                message: "Unexpectedly could not find federation spec in schema".to_owned(),
            })?;
        let mut arguments = vec![Node::new(Argument {
            name: FEDERATION_NAME_ARGUMENT_NAME,
            value: Node::new(Value::String(name)),
        })];
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
                        "Unexpectedly could not find federation spec's \"@{FEDERATION_REQUIRES_DIRECTIVE_NAME_IN_SPEC}\" directive definition"
                    ),
                }.into()
            })
    }

    pub(crate) fn tag_directive_arguments<'doc>(
        &self,
        application: &'doc Node<Directive>,
    ) -> Result<TagDirectiveArguments<'doc>, FederationError> {
        Ok(TagDirectiveArguments {
            name: directive_required_string_argument(application, &FEDERATION_NAME_ARGUMENT_NAME)?,
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
                        "Unexpectedly could not find federation spec's \"@{FEDERATION_PROVIDES_DIRECTIVE_NAME_IN_SPEC}\" directive definition"
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

    pub(crate) fn shareable_directive_name_in_schema(
        &self,
        schema: &FederationSchema,
    ) -> Result<Option<Name>, FederationError> {
        self.directive_name_in_schema(schema, &FEDERATION_SHAREABLE_DIRECTIVE_NAME_IN_SPEC)
    }

    pub(crate) fn shareable_directive_definition<'schema>(
        &self,
        schema: &'schema FederationSchema,
    ) -> Result<&'schema Node<DirectiveDefinition>, FederationError> {
        self.directive_definition(schema, &FEDERATION_SHAREABLE_DIRECTIVE_NAME_IN_SPEC)?
            .ok_or_else(|| {
                FederationError::internal(format!(
                    "Unexpectedly could not find federation spec's \"@{FEDERATION_SHAREABLE_DIRECTIVE_NAME_IN_SPEC}\" directive definition"
                ))
            })
    }

    pub(crate) fn shareable_directive(
        &self,
        schema: &FederationSchema,
    ) -> Result<Directive, FederationError> {
        let name_in_schema = self
            .shareable_directive_name_in_schema(schema)?
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
                    "Unexpectedly could not find federation spec's \"@{FEDERATION_OVERRIDE_DIRECTIVE_NAME_IN_SPEC}\" directive definition"
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

    pub(crate) fn context_directive_definition<'schema>(
        &self,
        schema: &'schema FederationSchema,
    ) -> Result<&'schema Node<DirectiveDefinition>, FederationError> {
        self.directive_definition(schema, &FEDERATION_CONTEXT_DIRECTIVE_NAME_IN_SPEC)?
            .ok_or_else(|| {
                FederationError::internal(format!(
                    "Unexpectedly could not find federation spec's \"@{FEDERATION_CONTEXT_DIRECTIVE_NAME_IN_SPEC}\" directive definition",
                ))
            })
    }

    pub(crate) fn context_directive(
        &self,
        schema: &FederationSchema,
        name: String,
    ) -> Result<Directive, FederationError> {
        let name_in_schema = self
            .directive_name_in_schema(schema, &FEDERATION_CONTEXT_DIRECTIVE_NAME_IN_SPEC)?
            .ok_or_else(|| SingleFederationError::Internal {
                message: "Unexpectedly could not find federation spec in schema".to_owned(),
            })?;

        let arguments = vec![Node::new(Argument {
            name: FEDERATION_NAME_ARGUMENT_NAME,
            value: Node::new(Value::String(name)),
        })];

        Ok(Directive {
            name: name_in_schema,
            arguments,
        })
    }

    pub(crate) fn context_directive_arguments<'doc>(
        &self,
        application: &'doc Node<Directive>,
    ) -> Result<ContextDirectiveArguments<'doc>, FederationError> {
        Ok(ContextDirectiveArguments {
            name: directive_required_string_argument(application, &FEDERATION_NAME_ARGUMENT_NAME)?,
        })
    }

    // The directive is named `@fromContext`. This is confusing for clippy, as
    // `from` is a conventional prefix used in conversion methods, which do not
    // take `self` as an argument. This function does **not** perform
    // conversion, but extracts `@fromContext` directive definition.
    #[allow(clippy::wrong_self_convention)]
    pub(crate) fn from_context_directive_definition<'schema>(
        &self,
        schema: &'schema FederationSchema,
    ) -> Result<&'schema Node<DirectiveDefinition>, FederationError> {
        self.directive_definition(schema, &FEDERATION_FROM_CONTEXT_DIRECTIVE_NAME_IN_SPEC)?
            .ok_or_else(|| {
                FederationError::internal(format!(
                    "Unexpectedly could not find federation spec's \"@{FEDERATION_FROM_CONTEXT_DIRECTIVE_NAME_IN_SPEC}\" directive definition",
                ))
            })
    }

    // The directive is named `@fromContext`. This is confusing for clippy, as
    // `from` is a conventional prefix used in conversion methods, which do not
    // take `self` as an argument. This function does **not** perform
    // conversion, but extracts `@fromContext` directive.
    #[allow(clippy::wrong_self_convention)]
    pub(crate) fn from_context_directive(
        &self,
        schema: &FederationSchema,
        name: String,
    ) -> Result<Directive, FederationError> {
        let name_in_schema = self
            .directive_name_in_schema(schema, &FEDERATION_FROM_CONTEXT_DIRECTIVE_NAME_IN_SPEC)?
            .ok_or_else(|| SingleFederationError::Internal {
                message: "Unexpectedly could not find federation spec in schema".to_owned(),
            })?;

        let arguments = vec![Node::new(Argument {
            name: FEDERATION_FIELD_ARGUMENT_NAME,
            value: Node::new(Value::String(name)),
        })];

        Ok(Directive {
            name: name_in_schema,
            arguments,
        })
    }

    // The directive is named `@fromContext`. This is confusing for clippy, as
    // `from` is a conventional prefix used in conversion methods, which do not
    // take `self` as an argument. This function does **not** perform
    // conversion, but extracts `@fromContext` directive arguments.
    #[allow(clippy::wrong_self_convention)]
    pub(crate) fn from_context_directive_arguments<'doc>(
        &self,
        application: &'doc Node<Directive>,
    ) -> Result<FromContextDirectiveArguments<'doc>, FederationError> {
        Ok(FromContextDirectiveArguments {
            field: directive_required_string_argument(
                application,
                &FEDERATION_FIELD_ARGUMENT_NAME,
            )?,
        })
    }

    pub(crate) fn cache_tag_directive_definition<'schema>(
        &self,
        schema: &'schema FederationSchema,
    ) -> Result<&'schema Node<DirectiveDefinition>, FederationError> {
        self.directive_definition(schema, &FEDERATION_CACHE_TAG_DIRECTIVE_NAME_IN_SPEC)?
            .ok_or_else(|| {
                FederationError::internal(format!(
                    "Unexpectedly could not find federation spec's \"@{FEDERATION_CACHE_TAG_DIRECTIVE_NAME_IN_SPEC}\" directive definition",
                ))
            })
    }

    pub(crate) fn cache_tag_directive_arguments<'doc>(
        &self,
        application: &'doc Node<Directive>,
    ) -> Result<CacheTagDirectiveArguments<'doc>, FederationError> {
        Ok(CacheTagDirectiveArguments {
            format: directive_required_string_argument(
                application,
                &FEDERATION_FORMAT_ARGUMENT_NAME,
            )?,
        })
    }

    pub(crate) fn compose_directive_definition<'schema>(
        &self,
        schema: &'schema FederationSchema,
    ) -> Result<&'schema Node<DirectiveDefinition>, FederationError> {
        self.directive_definition(schema, &FEDERATION_COMPOSEDIRECTIVE_DIRECTIVE_NAME_IN_SPEC)?
            .ok_or_else(|| {
                FederationError::internal(format!(
                    "Unexpectedly could not find federation spec's \"@{FEDERATION_COMPOSEDIRECTIVE_DIRECTIVE_NAME_IN_SPEC}\" directive definition",
                ))
            })
    }

    pub(crate) fn compose_directive_arguments<'doc>(
        &self,
        application: &'doc Node<Directive>,
    ) -> Result<ComposeDirectiveArguments<'doc>, FederationError> {
        Ok(ComposeDirectiveArguments {
            name: directive_required_string_argument(application, &FEDERATION_NAME_ARGUMENT_NAME)?,
        })
    }

    fn key_directive_specification() -> DirectiveSpecification {
        DirectiveSpecification::new(
            FEDERATION_KEY_DIRECTIVE_NAME_IN_SPEC,
            &[
                Self::fields_argument_specification(),
                Self::resolvable_argument_specification(),
            ],
            true,
            &[DirectiveLocation::Object, DirectiveLocation::Interface],
            false,
            None,
            None,
        )
    }

    fn fields_argument_specification() -> DirectiveArgumentSpecification {
        DirectiveArgumentSpecification {
            base_spec: ArgumentSpecification {
                name: FEDERATION_FIELDS_ARGUMENT_NAME,
                get_type: |schema, _| field_set_type(schema),
                default_value: None,
            },
            composition_strategy: None,
        }
    }

    fn resolvable_argument_default_value() -> bool {
        true
    }

    fn resolvable_argument_specification() -> DirectiveArgumentSpecification {
        DirectiveArgumentSpecification {
            base_spec: ArgumentSpecification {
                name: FEDERATION_RESOLVABLE_ARGUMENT_NAME,
                get_type: |_, _| Ok(ty!(Boolean)),
                default_value: Some(Value::Boolean(Self::resolvable_argument_default_value())),
            },
            composition_strategy: None,
        }
    }

    fn requires_directive_specification() -> DirectiveSpecification {
        DirectiveSpecification::new(
            FEDERATION_REQUIRES_DIRECTIVE_NAME_IN_SPEC,
            &[Self::fields_argument_specification()],
            false,
            &[DirectiveLocation::FieldDefinition],
            false,
            None,
            None,
        )
    }

    fn provides_directive_specification() -> DirectiveSpecification {
        DirectiveSpecification::new(
            FEDERATION_PROVIDES_DIRECTIVE_NAME_IN_SPEC,
            &[Self::fields_argument_specification()],
            false,
            &[DirectiveLocation::FieldDefinition],
            false,
            None,
            None,
        )
    }

    fn external_directive_specification() -> DirectiveSpecification {
        DirectiveSpecification::new(
            FEDERATION_EXTERNAL_DIRECTIVE_NAME_IN_SPEC,
            &[DirectiveArgumentSpecification {
                base_spec: ArgumentSpecification {
                    name: FEDERATION_REASON_ARGUMENT_NAME,
                    get_type: |_, _| Ok(ty!(String)),
                    default_value: None,
                },
                composition_strategy: None,
            }],
            false,
            &[
                DirectiveLocation::Object,
                DirectiveLocation::FieldDefinition,
            ],
            false,
            None,
            None,
        )
    }

    fn extends_directive_specification() -> DirectiveSpecification {
        DirectiveSpecification::new(
            FEDERATION_EXTENDS_DIRECTIVE_NAME_IN_SPEC,
            &[],
            false,
            &[DirectiveLocation::Object, DirectiveLocation::Interface],
            false,
            None,
            None,
        )
    }

    fn shareable_directive_specification(&self) -> DirectiveSpecification {
        DirectiveSpecification::new(
            FEDERATION_SHAREABLE_DIRECTIVE_NAME_IN_SPEC,
            &[],
            self.version().ge(&Version { major: 2, minor: 2 }),
            &[
                DirectiveLocation::Object,
                DirectiveLocation::FieldDefinition,
            ],
            false,
            None,
            None,
        )
    }

    fn override_directive_specification(&self) -> DirectiveSpecification {
        let mut args = vec![DirectiveArgumentSpecification {
            base_spec: ArgumentSpecification {
                name: FEDERATION_FROM_ARGUMENT_NAME,
                get_type: |_, _| Ok(ty!(String!)),
                default_value: None,
            },
            composition_strategy: None,
        }];
        if self.version().satisfies(&Version { major: 2, minor: 7 }) {
            args.push(DirectiveArgumentSpecification {
                base_spec: ArgumentSpecification {
                    name: FEDERATION_OVERRIDE_LABEL_ARGUMENT_NAME,
                    get_type: |_, _| Ok(ty!(String)),
                    default_value: None,
                },
                composition_strategy: None,
            });
        }
        DirectiveSpecification::new(
            FEDERATION_OVERRIDE_DIRECTIVE_NAME_IN_SPEC,
            &args,
            false,
            &[DirectiveLocation::FieldDefinition],
            false,
            None,
            None,
        )
    }

    // NOTE: due to the long-standing subgraph-js bug we'll continue to define name argument
    // as nullable and rely on validations to ensure that value is set.
    fn compose_directive_directive_specification() -> DirectiveSpecification {
        DirectiveSpecification::new(
            FEDERATION_COMPOSEDIRECTIVE_DIRECTIVE_NAME_IN_SPEC,
            &[DirectiveArgumentSpecification {
                base_spec: ArgumentSpecification {
                    name: FEDERATION_NAME_ARGUMENT_NAME,
                    get_type: |_, _| Ok(ty!(String)),
                    default_value: None,
                },
                composition_strategy: None,
            }],
            true,
            &[DirectiveLocation::Schema],
            false,
            None,
            None,
        )
    }

    fn interface_object_directive_directive_specification() -> DirectiveSpecification {
        DirectiveSpecification::new(
            FEDERATION_INTERFACEOBJECT_DIRECTIVE_NAME_IN_SPEC,
            &[],
            false,
            &[DirectiveLocation::Object],
            false,
            None,
            None,
        )
    }

    fn cache_tag_directive_specification() -> DirectiveSpecification {
        DirectiveSpecification::new(
            FEDERATION_CACHE_TAG_DIRECTIVE_NAME_IN_SPEC,
            &[DirectiveArgumentSpecification {
                base_spec: ArgumentSpecification {
                    name: FEDERATION_FORMAT_ARGUMENT_NAME,
                    get_type: |_, _| Ok(ty!(String!)),
                    default_value: None,
                },
                composition_strategy: None,
            }],
            true,
            &[
                DirectiveLocation::Object,
                DirectiveLocation::Interface,
                DirectiveLocation::FieldDefinition,
            ],
            false,
            None,
            None,
        )
    }
}

fn field_set_type(schema: &FederationSchema) -> Result<Type, FederationError> {
    // PORT_NOTE: `schema.subgraph_metadata` is not accessible, since it's not validated, yet.
    // PORT_NOTE: No counterpart for metadata.fieldSetType. Use FederationSchema::field_set_type.
    schema
        .field_set_type()
        .map(|pos| Type::non_null(Type::Named(pos.type_name)))
}

impl SpecDefinition for FederationSpecDefinition {
    fn url(&self) -> &Url {
        &self.url
    }

    fn directive_specs(&self) -> Vec<Box<dyn TypeAndDirectiveSpecification>> {
        let mut specs: Vec<Box<dyn TypeAndDirectiveSpecification>> = vec![
            Box::new(Self::key_directive_specification()),
            Box::new(Self::requires_directive_specification()),
            Box::new(Self::provides_directive_specification()),
            Box::new(Self::external_directive_specification()),
        ];
        // Federation 2.3+ use tag spec v0.3, otherwise use v0.2
        if self.version().satisfies(&Version { major: 2, minor: 3 }) {
            if let Some(tag_spec) = TAG_VERSIONS.find(&Version { major: 0, minor: 3 }) {
                specs.extend(tag_spec.directive_specs());
            }
        } else if let Some(tag_spec) = TAG_VERSIONS.find(&Version { major: 0, minor: 2 }) {
            specs.extend(tag_spec.directive_specs());
        }
        specs.push(Box::new(Self::extends_directive_specification()));

        if self.is_fed1() {
            // PORT_NOTE: Fed 1 has `@key`, `@requires`, `@provides`, `@external`, `@tag` (v0.2) and `@extends`.
            // The specs we return at this point correspond to `legacyFederationDirectives` in JS.
            return specs;
        }

        specs.push(Box::new(self.shareable_directive_specification()));

        if let Some(inaccessible_spec) =
            INACCESSIBLE_VERSIONS.get_dyn_minimum_required_version(self.version())
        {
            specs.extend(inaccessible_spec.directive_specs());
        }

        specs.push(Box::new(self.override_directive_specification()));

        if self.version().satisfies(&Version { major: 2, minor: 1 }) {
            specs.push(Box::new(Self::compose_directive_directive_specification()));
        }

        if self.version().satisfies(&Version { major: 2, minor: 3 }) {
            specs.push(Box::new(
                Self::interface_object_directive_directive_specification(),
            ));
        }

        if self.version().satisfies(&Version { major: 2, minor: 5 }) {
            if let Some(auth_spec) = AUTHENTICATED_VERSIONS.find(&Version { major: 0, minor: 1 }) {
                specs.extend(auth_spec.directive_specs());
            }
            if let Some(requires_scopes_spec) =
                REQUIRES_SCOPES_VERSIONS.find(&Version { major: 0, minor: 1 })
            {
                specs.extend(requires_scopes_spec.directive_specs());
            }
        }

        if self.version().satisfies(&Version { major: 2, minor: 6 })
            && let Some(policy_spec) = POLICY_VERSIONS.find(&Version { major: 0, minor: 1 })
        {
            specs.extend(policy_spec.directive_specs());
        }

        if self.version().satisfies(&Version { major: 2, minor: 8 }) {
            let context_spec_definitions =
                ContextSpecDefinition::new(self.version().clone(), Version { major: 2, minor: 8 })
                    .directive_specs();
            specs.extend(context_spec_definitions);
        }

        if self.version().satisfies(&Version { major: 2, minor: 9 })
            && let Some(cost_spec) = COST_VERSIONS.find(&Version { major: 0, minor: 1 })
        {
            specs.extend(cost_spec.directive_specs());
        }

        if self.version().satisfies(&Version {
            major: 2,
            minor: 12,
        }) {
            specs.push(Box::new(Self::cache_tag_directive_specification()));
        }

        specs
    }

    fn type_specs(&self) -> Vec<Box<dyn TypeAndDirectiveSpecification>> {
        let mut type_specs: Vec<Box<dyn TypeAndDirectiveSpecification>> =
            vec![Box::new(ScalarTypeSpecification {
                name: FEDERATION_FIELDSET_TYPE_NAME_IN_SPEC,
            })];

        if self.version().satisfies(&Version { major: 2, minor: 5 })
            && let Some(requires_scopes_spec) =
                REQUIRES_SCOPES_VERSIONS.find(&Version { major: 0, minor: 1 })
        {
            type_specs.extend(requires_scopes_spec.type_specs());
        }

        if self.version().satisfies(&Version { major: 2, minor: 6 })
            && let Some(policy_spec) = POLICY_VERSIONS.find(&Version { major: 0, minor: 1 })
        {
            type_specs.extend(policy_spec.type_specs());
        }

        if self.version().satisfies(&Version { major: 2, minor: 8 }) {
            type_specs.extend(
                ContextSpecDefinition::new(self.version().clone(), Version { major: 2, minor: 8 })
                    .type_specs(),
            );
        }
        type_specs
    }

    fn minimum_federation_version(&self) -> &Version {
        &self.url.version
    }

    fn purpose(&self) -> Option<link::Purpose> {
        None
    }
}

pub(crate) static FED_1: LazyLock<FederationSpecDefinition> =
    LazyLock::new(|| FederationSpecDefinition::new(Version { major: 1, minor: 0 }));

pub(crate) static FEDERATION_VERSIONS: LazyLock<SpecDefinitions<FederationSpecDefinition>> =
    LazyLock::new(|| {
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
        definitions.add(FederationSpecDefinition::new(Version {
            major: 2,
            minor: 10,
        }));
        definitions.add(FederationSpecDefinition::new(Version {
            major: 2,
            minor: 11,
        }));
        definitions.add(FederationSpecDefinition::new(Version {
            major: 2,
            minor: 12,
        }));
        definitions
    });

pub(crate) fn get_federation_spec_definition_from_subgraph(
    schema: &FederationSchema,
) -> Result<&'static FederationSpecDefinition, FederationError> {
    if let Some(federation_link) = schema
        .metadata()
        .as_ref()
        .and_then(|metadata| metadata.for_identity(&Identity::federation_identity()))
    {
        if FED_1.url.version == federation_link.url.version {
            return Ok(&FED_1);
        }
        FEDERATION_VERSIONS
            .find(&federation_link.url.version)
            .ok_or_else(|| internal_error!(
                "Subgraph unexpectedly does not use a supported federation spec version. Requested version: {}",
                federation_link.url.version,
            ))
    } else {
        // No federation link found in schema. The default is v1.0.
        Ok(&FED_1)
    }
}

/// Creates a fake imports for fed 1 link directive.
/// - Fed 1 does not support `import` argument, but we use it to simulate fed 1 behavior.
// PORT_NOTE: From `FAKE_FED1_CORE_FEATURE_TO_RENAME_TYPES` in JS
// Federation 1 has that specificity that it wasn't using @link to name-space federation elements,
// and so to "distinguish" the few federation type names, it prefixed those with a `_`. That is,
// the `FieldSet` type was named `_FieldSet` in federation1. To handle this without too much effort,
// we use a fake `Link` with imports for all the fed1 types to use those specific "aliases"
// and we pass it when adding those types. This allows to reuse the same `TypeSpecification` objects
// for both fed1 and fed2.
pub(crate) fn fed1_link_imports() -> Vec<Arc<link::Import>> {
    let type_specs = FED_1.type_specs();
    let directive_specs = FED_1.directive_specs();
    let type_imports = type_specs.iter().map(|spec| link::Import {
        element: spec.name().clone(),
        is_directive: false,
        alias: Some(Name::new_unchecked(&format!("_{}", spec.name()))),
    });
    let directive_imports = directive_specs.iter().map(|spec| link::Import {
        element: spec.name().clone(),
        is_directive: true,
        alias: None,
    });
    type_imports
        .chain(directive_imports)
        .map(Arc::new)
        .collect()
}
