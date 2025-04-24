// No panics allowed in this module
// The expansion code is called with potentially invalid schemas during the
// authoring process and we can't panic in the language server.
#![cfg_attr(
    not(test),
    deny(
        clippy::exit,
        clippy::panic,
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::indexing_slicing,
        clippy::unimplemented,
        clippy::todo
    )
)]

mod directives;
pub(crate) mod schema;
mod type_and_directive_specifications;
pub(crate) mod versions;

use std::fmt::Display;

use apollo_compiler::Name;
use apollo_compiler::Schema;
use apollo_compiler::ast::Argument;
use apollo_compiler::ast::Directive;
use apollo_compiler::ast::DirectiveLocation;
use apollo_compiler::ast::Value;
use apollo_compiler::name;
pub(crate) use directives::extract_connect_directive_arguments;
pub(crate) use directives::extract_source_directive_arguments;
pub(crate) use schema::ConnectHTTPArguments;
pub(crate) use schema::SourceHTTPArguments;
use strum_macros::EnumIter;

use self::schema::CONNECT_DIRECTIVE_NAME_IN_SPEC;
use self::schema::SOURCE_DIRECTIVE_NAME_IN_SPEC;
use crate::error::FederationError;
use crate::error::SingleFederationError;
use crate::link::Link;
use crate::link::spec::APOLLO_SPEC_DOMAIN;
use crate::link::spec::Identity;
use crate::link::spec::Url;
use crate::link::spec::Version;
use crate::schema::FederationSchema;

/// The known versions of the connect spec
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, EnumIter)]
pub enum ConnectSpec {
    V0_1,
    V0_2,
}

impl PartialOrd for ConnectSpec {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        let self_version: Version = (*self).into();
        let other_version: Version = (*other).into();
        self_version.partial_cmp(&other_version)
    }
}

impl ConnectSpec {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::V0_1 => "0.1",
            Self::V0_2 => "0.2",
        }
    }

    const IDENTITY_NAME: Name = name!("connect");

    pub(crate) fn from_directive(directive: &Directive) -> Result<Option<Self>, FederationError> {
        let Some(url) = directive
            .specified_argument_by_name("url")
            .and_then(|a| a.as_str())
        else {
            return Ok(None);
        };

        let url: Url = url.parse()?;
        Self::identity_matches(&url.identity)
            .then(|| Self::try_from(&url.version))
            .transpose()
            .map_err(FederationError::from)
    }

    pub(crate) fn identity_matches(identity: &Identity) -> bool {
        identity.domain == APOLLO_SPEC_DOMAIN && identity.name == Self::IDENTITY_NAME
    }

    pub(crate) fn identity() -> Identity {
        Identity {
            domain: APOLLO_SPEC_DOMAIN.to_string(),
            name: Self::IDENTITY_NAME,
        }
    }

    pub(crate) fn url(&self) -> Url {
        Url {
            identity: Self::identity(),
            version: (*self).into(),
        }
    }

    pub(crate) fn get_from_schema(schema: &Schema) -> Option<(Self, Link)> {
        let (link, _) = Link::for_identity(schema, &Self::identity())?;
        Self::try_from(&link.url.version)
            .ok()
            .map(|spec| (spec, link))
    }

    pub(crate) fn check_or_add(schema: &mut FederationSchema) -> Result<(), FederationError> {
        let Some(link) = schema
            .metadata()
            .and_then(|metadata| metadata.for_identity(&Self::identity()))
        else {
            return Ok(());
        };

        let spec = Self::try_from(&link.url.version)?;

        type_and_directive_specifications::check_or_add(&link, &spec, schema)
    }

    pub(crate) fn source_directive_name(link: &Link) -> Name {
        link.directive_name_in_schema(&SOURCE_DIRECTIVE_NAME_IN_SPEC)
    }

    pub(crate) fn connect_directive_name(link: &Link) -> Name {
        link.directive_name_in_schema(&CONNECT_DIRECTIVE_NAME_IN_SPEC)
    }

    pub(crate) fn join_directive_application(&self) -> Directive {
        Directive {
            name: name!(join__directive),
            arguments: vec![
                Argument {
                    name: name!("graphs"),
                    value: Value::List(vec![]).into(),
                }
                .into(),
                Argument {
                    name: name!("name"),
                    value: Value::String("link".to_string()).into(),
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

    pub(crate) const fn connect_directive_locations(&self) -> &'static [DirectiveLocation] {
        match self {
            ConnectSpec::V0_1 => CONNECT_V0_1_LOCATIONS,
            ConnectSpec::V0_2 => CONNECT_V0_2_LOCATIONS,
        }
    }

    pub(crate) const fn available() -> &'static [ConnectSpec] {
        if cfg!(any(feature = "connect_v0.2", test)) {
            &[ConnectSpec::V0_1, ConnectSpec::V0_2]
        } else {
            &[ConnectSpec::V0_1]
        }
    }
}

impl TryFrom<&Version> for ConnectSpec {
    type Error = SingleFederationError;
    fn try_from(version: &Version) -> Result<Self, Self::Error> {
        match (version.major, version.minor) {
            (0, 1) => Ok(Self::V0_1),
            #[cfg(any(feature = "connect_v0.2", test))]
            (0, 2) => Ok(Self::V0_2),
            _ => Err(SingleFederationError::UnknownLinkVersion {
                message: format!("Unknown connect version: {version}"),
            }),
        }
    }
}

impl Display for ConnectSpec {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl From<ConnectSpec> for Version {
    fn from(spec: ConnectSpec) -> Self {
        match spec {
            ConnectSpec::V0_1 => Version { major: 0, minor: 1 },
            ConnectSpec::V0_2 => Version { major: 0, minor: 2 },
        }
    }
}

const CONNECT_V0_1_LOCATIONS: &[DirectiveLocation] = &[DirectiveLocation::FieldDefinition];
const CONNECT_V0_2_LOCATIONS: &[DirectiveLocation] = &[
    DirectiveLocation::FieldDefinition,
    DirectiveLocation::Object,
];
