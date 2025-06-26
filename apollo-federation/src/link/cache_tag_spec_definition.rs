use std::sync::LazyLock;

use apollo_compiler::Name;
use apollo_compiler::Node;
use apollo_compiler::ast::Argument;
use apollo_compiler::ast::Directive;
use apollo_compiler::ast::DirectiveDefinition;
use apollo_compiler::ast::DirectiveLocation;
use apollo_compiler::ast::Value;
use apollo_compiler::name;
use apollo_compiler::ty;

use super::federation_spec_definition::get_federation_spec_definition_from_subgraph;
use crate::error::FederationError;
use crate::internal_error;
use crate::link::argument::directive_required_string_argument;
use crate::link::federation_spec_definition::FEDERATION_CACHE_TAG_DIRECTIVE_NAME_IN_SPEC;
use crate::link::federation_spec_definition::FEDERATION_FORMAT_ARGUMENT_NAME;
use crate::link::spec::Identity;
use crate::link::spec::Url;
use crate::link::spec::Version;
use crate::link::spec_definition::SpecDefinition;
use crate::link::spec_definition::SpecDefinitions;
use crate::schema::FederationSchema;
use crate::schema::type_and_directive_specification::ArgumentSpecification;
use crate::schema::type_and_directive_specification::DirectiveArgumentSpecification;
use crate::schema::type_and_directive_specification::DirectiveSpecification;
use crate::schema::type_and_directive_specification::TypeAndDirectiveSpecification;

pub(crate) const CACHE_TAG_FORMAT_ARGUMENT_NAME: Name = name!("format");

pub(crate) struct CacheTagDirectiveArguments<'doc> {
    pub(crate) format: &'doc str,
}

#[derive(Clone)]
pub(crate) struct CacheTagSpecDefinition {
    url: Url,
    minimum_federation_version: Version,
}

impl CacheTagSpecDefinition {
    pub(crate) fn new(version: Version, minimum_federation_version: Version) -> Self {
        Self {
            url: Url {
                identity: Identity::context_identity(),
                version,
            },
            minimum_federation_version,
        }
    }

    pub(crate) fn cache_tag_directive_definition<'schema>(
        &self,
        schema: &'schema FederationSchema,
    ) -> Result<&'schema Node<DirectiveDefinition>, FederationError> {
        self.directive_definition(schema, &FEDERATION_CACHE_TAG_DIRECTIVE_NAME_IN_SPEC)?
            .ok_or_else(|| internal_error!("Unexpectedly could not find cacheTag spec in schema"))
    }

    pub(crate) fn context_directive_arguments<'doc>(
        &self,
        application: &'doc Node<Directive>,
    ) -> Result<CacheTagDirectiveArguments<'doc>, FederationError> {
        Ok(CacheTagDirectiveArguments {
            format: directive_required_string_argument(
                application,
                &CACHE_TAG_FORMAT_ARGUMENT_NAME,
            )?,
        })
    }

    fn for_federation_schema(schema: &FederationSchema) -> Option<&'static Self> {
        let link = schema
            .metadata()?
            .for_identity(&Identity::cache_tag_identity())?;
        CACHE_TAG_VERSIONS.find(&link.url.version)
    }

    #[allow(dead_code)]
    pub(crate) fn cache_tag_directive_name(
        schema: &FederationSchema,
    ) -> Result<Option<Name>, FederationError> {
        if let Some(spec) = Self::for_federation_schema(schema) {
            spec.directive_name_in_schema(schema, &FEDERATION_CACHE_TAG_DIRECTIVE_NAME_IN_SPEC)
        } else if let Ok(fed_spec) = get_federation_spec_definition_from_subgraph(schema) {
            fed_spec.directive_name_in_schema(schema, &FEDERATION_CACHE_TAG_DIRECTIVE_NAME_IN_SPEC)
        } else {
            Ok(None)
        }
    }

    pub(crate) fn join_directive_application(&self) -> Directive {
        Directive {
            name: name!(join__directive),
            arguments: vec![
                Argument {
                    name: name!("graphs"),
                    value: Value::List(Vec::new()).into(),
                }
                .into(),
                Argument {
                    name: name!("name"),
                    value: Value::String("cacheTag".to_string()).into(),
                }
                .into(),
                Argument {
                    name: name!("args"),
                    value: Value::Object(vec![(
                        name!("url"),
                        Value::String(self.url().to_string()).into(),
                    )])
                    .into(),
                }
                .into(),
            ],
        }
    }
}

impl SpecDefinition for CacheTagSpecDefinition {
    fn url(&self) -> &Url {
        &self.url
    }

    fn directive_specs(&self) -> Vec<Box<dyn TypeAndDirectiveSpecification>> {
        let context_spec = DirectiveSpecification::new(
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
            true,
            Some(&|v| CACHE_TAG_VERSIONS.get_dyn_minimum_required_version(v)),
            None,
        );
        vec![Box::new(context_spec)]
    }

    fn type_specs(&self) -> Vec<Box<dyn TypeAndDirectiveSpecification>> {
        vec![]
    }

    fn minimum_federation_version(&self) -> &Version {
        &self.minimum_federation_version
    }
}

pub(crate) static CACHE_TAG_VERSIONS: LazyLock<SpecDefinitions<CacheTagSpecDefinition>> =
    LazyLock::new(|| {
        let mut definitions = SpecDefinitions::new(Identity::context_identity());
        definitions.add(CacheTagSpecDefinition::new(
            Version { major: 0, minor: 1 },
            Version {
                major: 2,
                minor: 12,
            },
        ));
        definitions
    });
