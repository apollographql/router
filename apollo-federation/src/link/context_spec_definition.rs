use apollo_compiler::name;
use apollo_compiler::Name;
use lazy_static::lazy_static;

use crate::error::FederationError;
use crate::link::spec::Identity;
use crate::link::spec::Url;
use crate::link::spec::Version;
use crate::link::spec_definition::SpecDefinition;
use crate::link::spec_definition::SpecDefinitions;
use crate::schema::FederationSchema;

pub(crate) const CONTEXT_DIRECTIVE_NAME_IN_SPEC: Name = name!("context");
pub(crate) const CONTEXT_DIRECTIVE_NAME_DEFAULT: Name = name!("federation__context");

#[derive(Clone)]
pub(crate) struct ContextSpecDefinition {
    url: Url,
    minimum_federation_version: Option<Version>,
}

impl ContextSpecDefinition {
    pub(crate) fn new(version: Version, minimum_federation_version: Option<Version>) -> Self {
        Self {
            url: Url {
                identity: Identity::context_identity(),
                version,
            },
            minimum_federation_version,
        }
    }

    pub(crate) fn context_directive_name_in_schema(
        &self,
        schema: &FederationSchema,
    ) -> Result<Name, FederationError> {
        Ok(self
            .directive_name_in_schema(schema, &CONTEXT_DIRECTIVE_NAME_IN_SPEC)?
            .unwrap_or(CONTEXT_DIRECTIVE_NAME_DEFAULT))
    }
}

impl SpecDefinition for ContextSpecDefinition {
    fn url(&self) -> &Url {
        &self.url
    }

    fn minimum_federation_version(&self) -> Option<&Version> {
        self.minimum_federation_version.as_ref()
    }
}

lazy_static! {
    pub(crate) static ref CONTEXT_VERSIONS: SpecDefinitions<ContextSpecDefinition> = {
        let mut definitions = SpecDefinitions::new(Identity::context_identity());
        definitions.add(ContextSpecDefinition::new(
            Version { major: 0, minor: 1 },
            Some(Version { major: 2, minor: 8 }),
        ));
        definitions
    };
}
