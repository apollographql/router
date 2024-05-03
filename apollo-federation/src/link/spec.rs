//! Representation of Apollo `@link` specifications.
use std::fmt;
use std::str;

use apollo_compiler::ast::Name;
use apollo_compiler::name;
use thiserror::Error;

use crate::error::FederationError;
use crate::error::SingleFederationError;

pub const APOLLO_SPEC_DOMAIN: &str = "https://specs.apollo.dev";

#[derive(Error, Debug, PartialEq)]
pub enum SpecError {
    #[error("Parse error: {0}")]
    ParseError(String),
}

// TODO: Replace SpecError usages with FederationError.
impl From<SpecError> for FederationError {
    fn from(value: SpecError) -> Self {
        SingleFederationError::InvalidLinkIdentifier {
            message: value.to_string(),
        }
        .into()
    }
}

/// Represents the identity of a `@link` specification, which uniquely identify a specification.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct Identity {
    /// The "domain" of which the specification this identifies is part of.
    /// For instance, `"https://specs.apollo.dev"`.
    pub domain: String,

    /// The name of the specification this identifies.
    /// For instance, "federation".
    pub name: Name,
}

impl fmt::Display for Identity {
    /// Display a specification identity.
    ///
    ///     # use apollo_federation::link::spec::Identity;
    ///     use apollo_compiler::name;
    ///     assert_eq!(
    ///         Identity { domain: "https://specs.apollo.dev".to_string(), name: name!("federation") }.to_string(),
    ///         "https://specs.apollo.dev/federation"
    ///     )
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{}/{}", self.domain, self.name)
    }
}

impl Identity {
    pub fn core_identity() -> Identity {
        Identity {
            domain: APOLLO_SPEC_DOMAIN.to_string(),
            name: name!("core"),
        }
    }

    pub fn link_identity() -> Identity {
        Identity {
            domain: APOLLO_SPEC_DOMAIN.to_string(),
            name: name!("link"),
        }
    }

    pub fn federation_identity() -> Identity {
        Identity {
            domain: APOLLO_SPEC_DOMAIN.to_string(),
            name: name!("federation"),
        }
    }

    pub fn join_identity() -> Identity {
        Identity {
            domain: APOLLO_SPEC_DOMAIN.to_string(),
            name: name!("join"),
        }
    }

    pub fn inaccessible_identity() -> Identity {
        Identity {
            domain: APOLLO_SPEC_DOMAIN.to_string(),
            name: name!("inaccessible"),
        }
    }
}

/// The version of a `@link` specification, in the form of a major and minor version numbers.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub struct Version {
    /// The major number part of the version.
    pub major: u32,

    /// The minor number part of the version.
    pub minor: u32,
}

impl fmt::Display for Version {
    /// Display a specification version number.
    ///
    ///     # use apollo_federation::link::spec::Version;
    ///     assert_eq!(Version { major: 2, minor: 3 }.to_string(), "2.3")
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{}.{}", self.major, self.minor)
    }
}

impl str::FromStr for Version {
    type Err = SpecError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let (major, minor) = s.split_once('.').ok_or(SpecError::ParseError(
            "version number is missing a dot (.)".to_string(),
        ))?;

        let major = major.parse::<u32>().map_err(|_| {
            SpecError::ParseError(format!("invalid major version number '{}'", major))
        })?;
        let minor = minor.parse::<u32>().map_err(|_| {
            SpecError::ParseError(format!("invalid minor version number '{}'", minor))
        })?;

        Ok(Version { major, minor })
    }
}

impl Version {
    /// Whether this version satisfies the provided `required` version.
    ///
    ///     # use apollo_federation::link::spec::Version;
    ///     assert!(&Version { major: 1, minor: 0 }.satisfies(&Version{ major: 1, minor: 0 }));
    ///     assert!(&Version { major: 1, minor: 2 }.satisfies(&Version{ major: 1, minor: 0 }));
    ///
    ///     assert!(!(&Version { major: 2, minor: 0 }.satisfies(&Version{ major: 1, minor: 9 })));
    ///     assert!(!(&Version { major: 0, minor: 9 }.satisfies(&Version{ major: 0, minor: 8 })));
    pub fn satisfies(&self, required: &Version) -> bool {
        if self.major == 0 {
            self == required
        } else {
            self.major == required.major && self.minor >= required.minor
        }
    }

    /// Verifies whether this version satisfies the provided version range.
    ///
    /// # Panics
    /// The `min` and `max` must be the same major version, and `max` minor version must be higher than `min`'s.
    /// Else, you get a panic.
    ///
    /// # Examples
    ///
    ///     # use apollo_federation::link::spec::Version;
    ///     assert!(&Version { major: 1, minor: 1 }.satisfies_range(&Version{ major: 1, minor: 0 }, &Version{ major: 1, minor: 10 }));
    ///
    ///     assert!(!&Version { major: 2, minor: 0 }.satisfies_range(&Version{ major: 1, minor: 0 }, &Version{ major: 1, minor: 10 }));
    pub fn satisfies_range(&self, min: &Version, max: &Version) -> bool {
        assert_eq!(min.major, max.major);
        assert!(min.minor < max.minor);

        self.major == min.major && self.minor >= min.minor && self.minor <= max.minor
    }
}

/// A `@link` specification url, which identifies a specific version of a specification.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Url {
    /// The identity of the `@link` specification pointed by this url.
    pub identity: Identity,

    /// The version of the `@link` specification pointed by this url.
    pub version: Version,
}

impl fmt::Display for Url {
    /// Display a specification url.
    ///
    ///     # use apollo_federation::link::spec::*;
    ///     use apollo_compiler::name;
    ///     assert_eq!(
    ///         Url {
    ///           identity: Identity { domain: "https://specs.apollo.dev".to_string(), name: name!("federation") },
    ///           version: Version { major: 2, minor: 3 }
    ///         }.to_string(),
    ///         "https://specs.apollo.dev/federation/v2.3"
    ///     )
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{}/v{}", self.identity, self.version)
    }
}

impl str::FromStr for Url {
    type Err = SpecError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match url::Url::parse(s) {
            Ok(url) => {
                let mut segments = url.path_segments().ok_or(SpecError::ParseError(
                    "invalid `@link` specification url".to_string(),
                ))?;
                let version = segments.next_back().ok_or(SpecError::ParseError(
                    "invalid `@link` specification url: missing specification version".to_string(),
                ))?;
                if !version.starts_with('v') {
                    return Err(SpecError::ParseError("invalid `@link` specification url: the last element of the path should be the version starting with a 'v'".to_string()));
                }
                let version = version.strip_prefix('v').unwrap().parse::<Version>()?;
                let name = segments
                    .next_back()
                    .ok_or(SpecError::ParseError(
                        "invalid `@link` specification url: missing specification name".to_string(),
                    ))
                    // Note this is SUPER wrong, but the JS federation implementation didn't check
                    // if the name was valid, and customers are actively using URLs with for example dashes.
                    // So we pretend that it's fine. You can't reference an imported element by the
                    // namespaced name because it's not valid GraphQL to do so--but you can
                    // explicitly import elements from a spec with an invalid name.
                    .map(|segment| Name::new_unchecked(segment.into()))?;
                let scheme = url.scheme();
                if !scheme.starts_with("http") {
                    return Err(SpecError::ParseError("invalid `@link` specification url: only http(s) urls are supported currently".to_string()));
                }
                let url_domain = url.domain().ok_or(SpecError::ParseError(
                    "invalid `@link` specification url".to_string(),
                ))?;
                let path_remainder = segments.collect::<Vec<&str>>();
                let domain = if path_remainder.is_empty() {
                    format!("{}://{}", scheme, url_domain)
                } else {
                    format!("{}://{}/{}", scheme, url_domain, path_remainder.join("/"))
                };
                Ok(Url {
                    identity: Identity { domain, name },
                    version,
                })
            }
            Err(e) => Err(SpecError::ParseError(format!(
                "invalid specification url: {}",
                e
            ))),
        }
    }
}

#[cfg(test)]
mod tests {
    use apollo_compiler::name;

    use super::*;

    #[test]
    fn versions_compares_correctly() {
        assert!(Version { major: 0, minor: 0 } < Version { major: 0, minor: 1 });
        assert!(Version { major: 1, minor: 1 } < Version { major: 1, minor: 4 });
        assert!(Version { major: 1, minor: 4 } < Version { major: 2, minor: 0 });

        assert_eq!(
            Version { major: 0, minor: 0 },
            Version { major: 0, minor: 0 }
        );
        assert_eq!(
            Version { major: 2, minor: 3 },
            Version { major: 2, minor: 3 }
        );
    }

    #[test]
    fn valid_versions_can_be_parsed() {
        assert_eq!(
            "0.0".parse::<Version>().unwrap(),
            Version { major: 0, minor: 0 }
        );
        assert_eq!(
            "0.5".parse::<Version>().unwrap(),
            Version { major: 0, minor: 5 }
        );
        assert_eq!(
            "2.49".parse::<Version>().unwrap(),
            Version {
                major: 2,
                minor: 49
            }
        );
    }

    #[test]
    fn invalid_versions_strings_return_menaingful_errors() {
        assert_eq!(
            "foo".parse::<Version>(),
            Err(SpecError::ParseError(
                "version number is missing a dot (.)".to_string()
            ))
        );
        assert_eq!(
            "foo.bar".parse::<Version>(),
            Err(SpecError::ParseError(
                "invalid major version number 'foo'".to_string()
            ))
        );
        assert_eq!(
            "0.bar".parse::<Version>(),
            Err(SpecError::ParseError(
                "invalid minor version number 'bar'".to_string()
            ))
        );
        assert_eq!(
            "0.12-foo".parse::<Version>(),
            Err(SpecError::ParseError(
                "invalid minor version number '12-foo'".to_string()
            ))
        );
        assert_eq!(
            "0.12.2".parse::<Version>(),
            Err(SpecError::ParseError(
                "invalid minor version number '12.2'".to_string()
            ))
        );
    }

    #[test]
    fn valid_urls_can_be_parsed() {
        assert_eq!(
            "https://specs.apollo.dev/federation/v2.3"
                .parse::<Url>()
                .unwrap(),
            Url {
                identity: Identity {
                    domain: "https://specs.apollo.dev".to_string(),
                    name: name!("federation")
                },
                version: Version { major: 2, minor: 3 }
            }
        );

        assert_eq!(
            "http://something.com/more/path/my_spec_name/v0.1?k=2"
                .parse::<Url>()
                .unwrap(),
            Url {
                identity: Identity {
                    domain: "http://something.com/more/path".to_string(),
                    name: name!("my_spec_name")
                },
                version: Version { major: 0, minor: 1 }
            }
        );
    }
}
