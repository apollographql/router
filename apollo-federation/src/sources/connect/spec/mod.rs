mod directives;
mod schema;
mod type_and_directive_specifications;

use apollo_compiler::{ast::Directive, name, Schema};
use lazy_static::lazy_static;

use crate::{
    error::FederationError,
    link::{
        database::links_metadata,
        spec::{Identity, Url, Version, APOLLO_SPEC_DOMAIN},
        spec_definition::{SpecDefinition, SpecDefinitions},
    },
    schema::FederationSchema,
};

pub(crate) struct ConnectSpecDefinition {
    url: Url,
    minimum_federation_version: Option<Version>,
}

impl ConnectSpecDefinition {
    pub(crate) fn new(version: Version, minimum_federation_version: Option<Version>) -> Self {
        Self {
            url: Url {
                identity: Self::identity(),
                version,
            },
            minimum_federation_version,
        }
    }

    pub(crate) fn from_directive(
        directive: &Directive,
    ) -> Result<Option<&'static Self>, FederationError> {
        let Some(url) = directive.argument_by_name("url").and_then(|a| a.as_str()) else {
            return Ok(None);
        };

        let url: Url = url.parse()?;

        Ok(CONNECT_VERSIONS.find(&url.version))
    }

    pub(crate) fn identity() -> Identity {
        Identity {
            domain: APOLLO_SPEC_DOMAIN.to_string(),
            name: name!("connect"),
        }
    }

    pub(crate) fn get_from_schema(
        schema: &Schema,
    ) -> Result<Option<&'static ConnectSpecDefinition>, FederationError> {
        let metadata = links_metadata(schema)?;
        Ok(metadata
            .as_ref()
            .and_then(|metadata| metadata.for_identity(&ConnectSpecDefinition::identity()))
            .and_then(|link| CONNECT_VERSIONS.find(&link.url.version)))
    }

    pub(crate) fn get_from_federation_schema(
        schema: &FederationSchema,
    ) -> Result<Option<&'static ConnectSpecDefinition>, FederationError> {
        Ok(schema
            .metadata()
            .as_ref()
            .and_then(|metadata| metadata.for_identity(&ConnectSpecDefinition::identity()))
            .and_then(|link| CONNECT_VERSIONS.find(&link.url.version)))
    }

    pub(crate) fn check_or_add(schema: &mut FederationSchema) -> Result<(), FederationError> {
        let Some(link) = schema
            .metadata()
            .and_then(|metadata| metadata.for_identity(&ConnectSpecDefinition::identity()))
        else {
            return Ok(());
        };

        type_and_directive_specifications::check_or_add(&link, schema)
    }
}

impl SpecDefinition for ConnectSpecDefinition {
    fn url(&self) -> &Url {
        &self.url
    }

    fn minimum_federation_version(&self) -> Option<&Version> {
        self.minimum_federation_version.as_ref()
    }
}

lazy_static! {
    pub(crate) static ref CONNECT_VERSIONS: SpecDefinitions<ConnectSpecDefinition> = {
        let mut definitions = SpecDefinitions::new(ConnectSpecDefinition::identity());

        definitions.add(ConnectSpecDefinition::new(
            Version { major: 0, minor: 1 },
            Some(Version { major: 2, minor: 8 }), // TODO
        ));

        definitions
    };
}
