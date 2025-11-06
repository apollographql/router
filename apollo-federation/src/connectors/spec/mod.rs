//! The GraphQL spec for Connectors. Includes parsing of directives and injection of required definitions.
pub(crate) mod connect;
pub(crate) mod errors;
pub(crate) mod http;
pub(crate) mod source;
mod type_and_directive_specifications;

use std::fmt::Display;
use std::sync::LazyLock;

use apollo_compiler::Name;
use apollo_compiler::Schema;
use apollo_compiler::ast::Argument;
use apollo_compiler::ast::Directive;
use apollo_compiler::ast::Value;
use apollo_compiler::name;
use apollo_compiler::schema::Component;
pub use connect::ConnectHTTPArguments;
pub(crate) use connect::extract_connect_directive_arguments;
use itertools::Itertools;
pub use source::SourceHTTPArguments;
pub(crate) use source::extract_source_directive_arguments;
use strum::IntoEnumIterator;
use strum_macros::EnumIter;

use self::connect::CONNECT_DIRECTIVE_NAME_IN_SPEC;
use self::source::SOURCE_DIRECTIVE_NAME_IN_SPEC;
use crate::connectors::spec::type_and_directive_specifications::directive_specifications;
use crate::connectors::spec::type_and_directive_specifications::type_specifications;
use crate::connectors::validation::Code;
use crate::connectors::validation::Message;
use crate::error::FederationError;
use crate::link::Link;
use crate::link::Purpose;
use crate::link::spec::APOLLO_SPEC_DOMAIN;
use crate::link::spec::Identity;
use crate::link::spec::Url;
use crate::link::spec::Version;
use crate::link::spec_definition::SpecDefinition;
use crate::link::spec_definition::SpecDefinitions;
use crate::schema::type_and_directive_specification::TypeAndDirectiveSpecification;

const CONNECT_IDENTITY_NAME: Name = name!("connect");

/// The `@link` in a subgraph which enables connectors
#[derive(Clone, Debug)]
pub(crate) struct ConnectLink {
    pub(crate) spec: ConnectSpec,
    pub(crate) source_directive_name: Name,
    pub(crate) connect_directive_name: Name,
    pub(crate) directive: Component<Directive>,
    pub(crate) link: Link,
}

impl<'schema> ConnectLink {
    /// Find the connect link, if any, and validate it.
    /// Returns `None` if this is not a connectors subgraph.
    ///
    /// # Errors
    /// - Unknown spec version
    pub(super) fn new(schema: &'schema Schema) -> Option<Result<Self, Message>> {
        let (link, directive) = Link::for_identity(schema, &ConnectSpec::identity())?;

        let spec = match ConnectSpec::try_from(&link.url.version) {
            Err(err) => {
                let message = format!(
                    "{err}; should be one of {available_versions}.",
                    available_versions = ConnectSpec::iter().map(ConnectSpec::as_str).join(", "),
                );
                return Some(Err(Message {
                    code: Code::UnknownConnectorsVersion,
                    message,
                    locations: directive
                        .line_column_range(&schema.sources)
                        .into_iter()
                        .collect(),
                }));
            }
            Ok(spec) => spec,
        };
        let source_directive_name = link.directive_name_in_schema(&SOURCE_DIRECTIVE_NAME_IN_SPEC);
        let connect_directive_name = link.directive_name_in_schema(&CONNECT_DIRECTIVE_NAME_IN_SPEC);
        Some(Ok(Self {
            spec,
            source_directive_name,
            connect_directive_name,
            directive: directive.clone(),
            link,
        }))
    }
}

pub(crate) fn connect_spec_from_schema(schema: &Schema) -> Option<ConnectSpec> {
    let connect_identity = ConnectSpec::identity();
    Link::for_identity(schema, &connect_identity)
        .and_then(|(link, _directive)| ConnectSpec::try_from(&link.url.version).ok())
}

impl Display for ConnectLink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.link)
    }
}

/// The known versions of the connect spec
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, EnumIter)]
pub enum ConnectSpec {
    V0_1,
    V0_2,
    V0_3,
    V0_4,
}

impl PartialOrd for ConnectSpec {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        let self_version: Version = (*self).into();
        let other_version: Version = (*other).into();
        self_version.partial_cmp(&other_version)
    }
}

impl ConnectSpec {
    /// Returns the most recently released [`ConnectSpec`]. Used only in tests
    /// because using it production code leads to sudden accidental upgrades.
    #[cfg(test)]
    pub(crate) fn latest() -> Self {
        Self::V0_2
    }

    /// Returns the next version of the [`ConnectSpec`] to be released.
    /// Test-only!
    #[cfg(test)]
    pub(crate) fn next() -> Self {
        Self::V0_3
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::V0_1 => "0.1",
            Self::V0_2 => "0.2",
            Self::V0_3 => "0.3",
            Self::V0_4 => "0.4",
        }
    }

    pub(crate) fn identity() -> Identity {
        Identity {
            domain: APOLLO_SPEC_DOMAIN.to_string(),
            name: CONNECT_IDENTITY_NAME,
        }
    }

    pub(crate) fn url(&self) -> Url {
        Url {
            identity: Self::identity(),
            version: (*self).into(),
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
}

impl TryFrom<&Version> for ConnectSpec {
    type Error = String;
    fn try_from(version: &Version) -> Result<Self, Self::Error> {
        match (version.major, version.minor) {
            (0, 1) => Ok(Self::V0_1),
            (0, 2) => Ok(Self::V0_2),
            (0, 3) => Ok(Self::V0_3),
            (0, 4) => Ok(Self::V0_4),
            _ => Err(format!("Unknown connect version: {version}")),
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
            ConnectSpec::V0_3 => Version { major: 0, minor: 3 },
            ConnectSpec::V0_4 => Version { major: 0, minor: 4 },
        }
    }
}

pub(crate) struct ConnectSpecDefinition {
    minimum_federation_version: Version,
    url: Url,
}

impl ConnectSpecDefinition {
    pub(crate) fn new(version: Version, minimum_federation_version: Version) -> Self {
        Self {
            url: Url {
                identity: ConnectSpec::identity(),
                version,
            },
            minimum_federation_version,
        }
    }

    pub(crate) fn from_directive(
        directive: &Directive,
    ) -> Result<Option<&'static Self>, FederationError> {
        let Some(url) = directive
            .specified_argument_by_name("url")
            .and_then(|a| a.as_str())
        else {
            return Ok(None);
        };

        let url: Url = url.parse()?;
        if url.identity.domain != APOLLO_SPEC_DOMAIN || url.identity.name != CONNECT_IDENTITY_NAME {
            return Ok(None);
        }

        Ok(CONNECT_VERSIONS.find(&url.version))
    }
}

impl SpecDefinition for ConnectSpecDefinition {
    fn url(&self) -> &Url {
        &self.url
    }

    fn directive_specs(&self) -> Vec<Box<dyn TypeAndDirectiveSpecification>> {
        directive_specifications()
    }

    fn type_specs(&self) -> Vec<Box<dyn TypeAndDirectiveSpecification>> {
        type_specifications()
    }

    fn minimum_federation_version(&self) -> &Version {
        &self.minimum_federation_version
    }

    fn purpose(&self) -> Option<Purpose> {
        Some(Purpose::EXECUTION)
    }
}

pub(crate) static CONNECT_VERSIONS: LazyLock<SpecDefinitions<ConnectSpecDefinition>> =
    LazyLock::new(|| {
        let mut definitions = SpecDefinitions::new(Identity::connect_identity());
        definitions.add(ConnectSpecDefinition::new(
            Version { major: 0, minor: 1 },
            Version {
                major: 2,
                minor: 10,
            },
        ));
        definitions.add(ConnectSpecDefinition::new(
            Version { major: 0, minor: 2 },
            Version {
                major: 2,
                minor: 11,
            },
        ));
        definitions.add(ConnectSpecDefinition::new(
            Version { major: 0, minor: 3 },
            Version {
                major: 2,
                minor: 12,
            },
        ));
        definitions.add(ConnectSpecDefinition::new(
            Version { major: 0, minor: 4 },
            Version {
                major: 2,
                minor: 13,
            },
        ));
        definitions
    });
