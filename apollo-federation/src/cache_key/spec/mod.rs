use std::fmt::Display;

use apollo_compiler::Name;
use apollo_compiler::ast::Argument;
use apollo_compiler::ast::Directive;
use apollo_compiler::ast::Value;
use apollo_compiler::name;
use strum_macros::EnumIter;

use crate::error::FederationError;
use crate::error::SingleFederationError;
use crate::link::spec::APOLLO_SPEC_DOMAIN;
use crate::link::spec::Identity;
use crate::link::spec::Url;
use crate::link::spec::Version;
use crate::schema::FederationSchema;

pub(crate) mod schema;
mod type_and_directive_specifications;

/// The known versions of the cacheKey spec
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, EnumIter)]
pub enum CacheKeySpec {
    V0_1,
}

impl PartialOrd for CacheKeySpec {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        let self_version: Version = (*self).into();
        let other_version: Version = (*other).into();
        self_version.partial_cmp(&other_version)
    }
}

impl CacheKeySpec {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::V0_1 => "0.1",
        }
    }

    const IDENTITY_NAME: Name = name!("cacheKey");

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

    // pub(crate) fn get_from_schema(schema: &Schema) -> Option<(Self, Link)> {
    //     let (link, _) = Link::for_identity(schema, &Self::identity())?;
    //     Self::try_from(&link.url.version)
    //         .ok()
    //         .map(|spec| (spec, link))
    // }

    pub(crate) fn check_or_add(schema: &mut FederationSchema) -> Result<(), FederationError> {
        let Some(link) = schema
            .metadata()
            .and_then(|metadata| metadata.for_identity(&Self::identity()))
        else {
            return Ok(());
        };

        type_and_directive_specifications::check_or_add(&link, schema)
    }

    // pub(crate) fn cache_key_directive_name(link: &Link) -> Name {
    //     link.directive_name_in_schema(&CACHE_KEY_DIRECTIVE_NAME_IN_SPEC)
    // }

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
                    value: Value::String("cacheKey".to_string()).into(),
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

impl TryFrom<&Version> for CacheKeySpec {
    type Error = SingleFederationError;
    fn try_from(version: &Version) -> Result<Self, Self::Error> {
        match (version.major, version.minor) {
            (0, 1) => Ok(Self::V0_1),
            _ => Err(SingleFederationError::UnknownLinkVersion {
                message: format!("Unknown cacheKey version: {version}"),
            }),
        }
    }
}

impl Display for CacheKeySpec {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl From<CacheKeySpec> for Version {
    fn from(spec: CacheKeySpec) -> Self {
        match spec {
            CacheKeySpec::V0_1 => Version { major: 0, minor: 1 },
        }
    }
}
