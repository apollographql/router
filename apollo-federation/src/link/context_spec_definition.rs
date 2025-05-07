use std::sync::LazyLock;

use apollo_compiler::Name;
use apollo_compiler::Node;
use apollo_compiler::ast::Directive;
use apollo_compiler::ast::DirectiveDefinition;
use apollo_compiler::name;

use crate::error::FederationError;
use crate::internal_error;
use crate::link::argument::directive_required_string_argument;
use crate::link::spec::Identity;
use crate::link::spec::Url;
use crate::link::spec::Version;
use crate::link::spec_definition::SpecDefinition;
use crate::link::spec_definition::SpecDefinitions;
use crate::schema::FederationSchema;
use crate::schema::type_and_directive_specification::TypeAndDirectiveSpecification;

pub(crate) const CONTEXT_DIRECTIVE_NAME_IN_SPEC: Name = name!("context");
pub(crate) const CONTEXT_NAME_ARGUMENT_NAME: Name = name!("name");

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

    pub(crate) fn context_directive_definition<'schema>(
        &self,
        schema: &'schema FederationSchema,
    ) -> Result<&'schema Node<DirectiveDefinition>, FederationError> {
        self.directive_definition(schema, &CONTEXT_DIRECTIVE_NAME_IN_SPEC)?
            .ok_or_else(|| internal_error!("Unexpectedly could not find context spec in schema"))
    }

    pub(crate) fn context_directive_arguments<'doc>(
        &self,
        application: &'doc Node<Directive>,
    ) -> Result<ContextDirectiveArguments<'doc>, FederationError> {
        Ok(ContextDirectiveArguments {
            name: directive_required_string_argument(application, &CONTEXT_NAME_ARGUMENT_NAME)?,
        })
    }
}

impl SpecDefinition for ContextSpecDefinition {
    fn url(&self) -> &Url {
        &self.url
    }

    fn directive_specs(&self) -> Vec<Box<dyn TypeAndDirectiveSpecification>> {
        todo!()
    }

    fn type_specs(&self) -> Vec<Box<dyn TypeAndDirectiveSpecification>> {
        todo!()
    }

    fn minimum_federation_version(&self) -> &Version {
        &self.minimum_federation_version
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
