use std::rc::Rc;
use std::sync::LazyLock;

use apollo_compiler::Name;
use apollo_compiler::Node;
use apollo_compiler::ast::Directive;
use apollo_compiler::ast::DirectiveDefinition;
use apollo_compiler::ast::DirectiveLocation;
use apollo_compiler::ast::Type;
use apollo_compiler::collections::IndexMap;
use apollo_compiler::schema::Value;
use apollo_compiler::ty;

use super::federation_spec_definition::get_federation_spec_definition_from_subgraph;
use crate::bail;
use crate::error::FederationError;
use crate::internal_error;
use crate::link::Purpose;
use crate::link::argument::directive_required_string_argument;
use crate::link::federation_spec_definition::FEDERATION_CONTEXT_DIRECTIVE_NAME_IN_SPEC;
use crate::link::federation_spec_definition::FEDERATION_FIELD_ARGUMENT_NAME;
use crate::link::federation_spec_definition::FEDERATION_FROM_CONTEXT_DIRECTIVE_NAME_IN_SPEC;
use crate::link::federation_spec_definition::FEDERATION_NAME_ARGUMENT_NAME;
use crate::link::spec::Identity;
use crate::link::spec::Url;
use crate::link::spec::Version;
use crate::link::spec_definition::SpecDefinition;
use crate::link::spec_definition::SpecDefinitions;
use crate::schema::FederationSchema;
use crate::schema::type_and_directive_specification::ArgumentSpecification;
use crate::schema::type_and_directive_specification::DirectiveArgumentSpecification;
use crate::schema::type_and_directive_specification::DirectiveSpecification;
use crate::schema::type_and_directive_specification::ScalarTypeSpecification;
use crate::schema::type_and_directive_specification::TypeAndDirectiveSpecification;
use crate::subgraph::spec::CONTEXTFIELDVALUE_SCALAR_NAME;
use crate::subgraph::typestate::Subgraph;
use crate::subgraph::typestate::Validated;

pub(crate) struct ContextDirectiveArguments<'doc> {
    pub(crate) name: &'doc str,
}

#[derive(Clone)]
pub(crate) struct ContextSpecDefinition {
    url: Url,
    minimum_federation_version: Version,
}

impl ContextSpecDefinition {
    pub(crate) fn new(version: Version, minimum_federation_version: Version) -> Self {
        Self {
            url: Url {
                identity: Identity::context_identity(),
                version,
            },
            minimum_federation_version,
        }
    }

    fn static_argument_transform(
        subgraph: &Subgraph<Validated>,
        args: IndexMap<Name, Value>,
    ) -> IndexMap<Name, Value> {
        let subgraph_name = &subgraph.name;
        args.into_iter()
            .map(|(key, value)| {
                if key.as_str() == "name" {
                    (
                        key,
                        Value::String(format!("{}__{}", subgraph_name, value.as_str().unwrap())),
                    )
                } else {
                    (key, value)
                }
            })
            .collect()
    }

    pub(crate) fn context_directive_definition<'schema>(
        &self,
        schema: &'schema FederationSchema,
    ) -> Result<&'schema Node<DirectiveDefinition>, FederationError> {
        self.directive_definition(schema, &FEDERATION_CONTEXT_DIRECTIVE_NAME_IN_SPEC)?
            .ok_or_else(|| internal_error!("Unexpectedly could not find context spec in schema"))
    }

    pub(crate) fn context_directive_arguments<'doc>(
        &self,
        application: &'doc Node<Directive>,
    ) -> Result<ContextDirectiveArguments<'doc>, FederationError> {
        Ok(ContextDirectiveArguments {
            name: directive_required_string_argument(application, &FEDERATION_NAME_ARGUMENT_NAME)?,
        })
    }

    fn field_argument_specification() -> DirectiveArgumentSpecification {
        DirectiveArgumentSpecification {
            base_spec: ArgumentSpecification {
                name: FEDERATION_FIELD_ARGUMENT_NAME,
                get_type: |_schema, link| {
                    let Some(link) = link else {
                        bail!(
                            "Type {FEDERATION_FIELD_ARGUMENT_NAME} shouldn't be added without being attached to a @link spec"
                        )
                    };
                    let type_name = link.type_name_in_schema(&CONTEXTFIELDVALUE_SCALAR_NAME);
                    Ok(Type::nullable(Type::Named(type_name)))
                },
                default_value: None,
            },
            composition_strategy: None,
        }
    }

    fn for_federation_schema(schema: &FederationSchema) -> Option<&'static Self> {
        let link = schema
            .metadata()?
            .for_identity(&Identity::context_identity())?;
        CONTEXT_VERSIONS.find(&link.url.version)
    }

    #[allow(dead_code)]
    pub(crate) fn context_directive_name(
        schema: &FederationSchema,
    ) -> Result<Option<Name>, FederationError> {
        if let Some(spec) = Self::for_federation_schema(schema) {
            spec.directive_name_in_schema(schema, &FEDERATION_CONTEXT_DIRECTIVE_NAME_IN_SPEC)
        } else if let Ok(fed_spec) = get_federation_spec_definition_from_subgraph(schema) {
            fed_spec.directive_name_in_schema(schema, &FEDERATION_CONTEXT_DIRECTIVE_NAME_IN_SPEC)
        } else {
            Ok(None)
        }
    }

    #[allow(dead_code)]
    pub(crate) fn from_context_directive_name(
        schema: &FederationSchema,
    ) -> Result<Option<Name>, FederationError> {
        if let Some(spec) = Self::for_federation_schema(schema) {
            spec.directive_name_in_schema(schema, &FEDERATION_FROM_CONTEXT_DIRECTIVE_NAME_IN_SPEC)
        } else if let Ok(fed_spec) = get_federation_spec_definition_from_subgraph(schema) {
            fed_spec
                .directive_name_in_schema(schema, &FEDERATION_FROM_CONTEXT_DIRECTIVE_NAME_IN_SPEC)
        } else {
            Ok(None)
        }
    }
}

impl SpecDefinition for ContextSpecDefinition {
    fn url(&self) -> &Url {
        &self.url
    }

    fn directive_specs(&self) -> Vec<Box<dyn TypeAndDirectiveSpecification>> {
        let context_spec = DirectiveSpecification::new(
            FEDERATION_CONTEXT_DIRECTIVE_NAME_IN_SPEC,
            &[DirectiveArgumentSpecification {
                base_spec: ArgumentSpecification {
                    name: FEDERATION_NAME_ARGUMENT_NAME,
                    get_type: |_, _| Ok(ty!(String!)),
                    default_value: None,
                },
                composition_strategy: None,
            }],
            true,
            &[
                DirectiveLocation::Interface,
                DirectiveLocation::Object,
                DirectiveLocation::Union,
            ],
            true,
            Some(&|v| CONTEXT_VERSIONS.get_dyn_minimum_required_version(v)),
            Some(Rc::new(Self::static_argument_transform)),
        );
        let from_context_spec = DirectiveSpecification::new(
            FEDERATION_FROM_CONTEXT_DIRECTIVE_NAME_IN_SPEC,
            &[Self::field_argument_specification()],
            false,
            &[DirectiveLocation::ArgumentDefinition],
            false,
            None,
            None,
        );
        vec![Box::new(context_spec), Box::new(from_context_spec)]
    }

    fn type_specs(&self) -> Vec<Box<dyn TypeAndDirectiveSpecification>> {
        vec![Box::new(ScalarTypeSpecification {
            name: CONTEXTFIELDVALUE_SCALAR_NAME,
        })]
    }

    fn minimum_federation_version(&self) -> &Version {
        &self.minimum_federation_version
    }

    fn purpose(&self) -> Option<Purpose> {
        Some(Purpose::SECURITY)
    }
}

pub(crate) static CONTEXT_VERSIONS: LazyLock<SpecDefinitions<ContextSpecDefinition>> =
    LazyLock::new(|| {
        let mut definitions = SpecDefinitions::new(Identity::context_identity());
        definitions.add(ContextSpecDefinition::new(
            Version { major: 0, minor: 1 },
            Version { major: 2, minor: 8 },
        ));
        definitions
    });
