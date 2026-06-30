//! Representation of spec identities, versions, and URLs.
use std::fmt;
use std::str;
use std::sync::Arc;

use crate::error::FederationError;
use crate::error::SingleFederationError;

/// Represents the identity of a `@link` specification, which uniquely identify a specification.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct Identity {
    /// The "domain" of which the specification this identifies is part of. For instance,
    /// `"https://specs.apollo.dev"`.
    pub domain: String,

    /// The name of the specification this identifies. For instance, "federation". This isn't
    /// guaranteed to a valid GraphQL name.
    pub name: Arc<str>,
}

impl fmt::Display for Identity {
    /// Display a specification identity.
    ///
    ///     # use apollo_federation::link::spec::Identity;
    ///     use apollo_compiler::name;
    ///     assert_eq!(
    ///         Identity { domain: "https://specs.apollo.dev".to_string(), name: name!("federation").into() }.to_string(),
    ///         "https://specs.apollo.dev/federation"
    ///     )
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{}/{}", self.domain, self.name)
    }
}

/// The version of a `@link` specification, in the form of a major and minor version numbers.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Debug, Hash)]
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
    type Err = FederationError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let (major, minor) =
            s.split_once('.')
                .ok_or(SingleFederationError::InvalidLinkIdentifier {
                    message: format!(
                        r#"Version number `{s}` in @link(url:) argument is missing a period (.)"#
                    ),
                })?;

        let major =
            major
                .parse::<u32>()
                .map_err(|_| SingleFederationError::InvalidLinkIdentifier {
                    message: format!(
                        r#"Major version `{major}` in @link(url:) argument is not a valid number"#
                    ),
                })?;
        let minor =
            minor
                .parse::<u32>()
                .map_err(|_| SingleFederationError::InvalidLinkIdentifier {
                    message: format!(
                        r#"Minor version `{minor}` in @link(url:) argument is not a valid number"#
                    ),
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
#[derive(Clone, PartialEq, Eq, Debug, Hash)]
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
    ///           identity: Identity { domain: "https://specs.apollo.dev".to_string(), name: name!("federation").into() },
    ///           version: Version { major: 2, minor: 3 }
    ///         }.to_string(),
    ///         "https://specs.apollo.dev/federation/v2.3"
    ///     )
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{}/v{}", self.identity, self.version)
    }
}

impl str::FromStr for Url {
    type Err = FederationError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match url::Url::parse(s) {
            Ok(url) => {
                let mut segments =
                    url.path_segments()
                        .ok_or(SingleFederationError::InvalidLinkIdentifier {
                            message: format!(
                                r#"@link(url:) argument `"{s}"` missing leading slash after scheme"#
                            ),
                        })?;
                let version =
                    segments
                        .next_back()
                        .ok_or(SingleFederationError::InvalidLinkIdentifier {
                            message: format!(
                                r#"@link(url:) argument `"{s}"` missing specification version"#
                            ),
                        })?;
                if !version.starts_with('v') {
                    return Err(SingleFederationError::InvalidLinkIdentifier {
                        message: format!(r#"Last path segment of @link(url:) argument `"{s}"` should start with 'v' to indicate version"#),
                    }.into());
                }
                let version = version.strip_prefix('v').unwrap().parse::<Version>()?;
                let name = segments
                    .next_back()
                    .ok_or(SingleFederationError::InvalidLinkIdentifier {
                        message: format!(
                            r#"@link(url:) argument `"{s}"` missing specification name"#
                        ),
                    })
                    .map(Arc::from)?;
                if !url.scheme().starts_with("http") {
                    return Err(SingleFederationError::InvalidLinkIdentifier {
                        message: format!(
                            r#"@link(url:) argument `"{s}"` must use the HTTP(S) scheme"#
                        ),
                    }
                    .into());
                }
                let origin = url.origin();
                if !origin.is_tuple() {
                    return Err(SingleFederationError::InvalidLinkIdentifier {
                        message: format!(r#"@link(url:) argument `"{s}"` must have a host"#),
                    }
                    .into());
                }
                let origin = origin.ascii_serialization();
                let path_remainder = segments.collect::<Vec<&str>>();
                let domain = if path_remainder.is_empty() {
                    origin
                } else {
                    format!("{origin}/{}", path_remainder.join("/"))
                };
                Ok(Url {
                    identity: Identity { domain, name },
                    version,
                })
            }
            Err(e) => Err(SingleFederationError::InvalidLinkIdentifier {
                message: format!(r#"@link(url:) argument `"{s}"` is not a valid URL: {e}"#),
            }
            .into()),
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
        "foo"
            .parse::<Version>()
            .expect_err(r#"Version number `foo` in @link(url:) argument is missing a period (.)"#);
        "foo.bar"
            .parse::<Version>()
            .expect_err(r#"Major version `foo` in @link(url:) argument is not a valid number"#);
        "0.bar"
            .parse::<Version>()
            .expect_err(r#"Minor version `bar` in @link(url:) argument is not a valid number"#);
        "0.12-foo"
            .parse::<Version>()
            .expect_err(r#"Minor version `12-foo` in @link(url:) argument is not a valid number"#);
        "0.12.2"
            .parse::<Version>()
            .expect_err(r#"Minor version `12.2` in @link(url:) argument is not a valid number"#);
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
                    name: name!("federation").into(),
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
                    name: name!("my_spec_name").into(),
                },
                version: Version { major: 0, minor: 1 }
            }
        );

        // Non-default port is preserved in the identity domain.
        assert_eq!(
            "http://localhost:8080/foo/v1.0".parse::<Url>().unwrap(),
            Url {
                identity: Identity {
                    domain: "http://localhost:8080".to_string(),
                    name: name!("foo").into(),
                },
                version: Version { major: 1, minor: 0 }
            }
        );

        // Non-default port is preserved alongside a path remainder.
        assert_eq!(
            "http://localhost:8080/extra/foo/v1.0"
                .parse::<Url>()
                .unwrap(),
            Url {
                identity: Identity {
                    domain: "http://localhost:8080/extra".to_string(),
                    name: name!("foo").into(),
                },
                version: Version { major: 1, minor: 0 }
            }
        );

        // Default https port is omitted, so existing identities are unaffected.
        assert_eq!(
            "https://specs.apollo.dev:443/federation/v2.3"
                .parse::<Url>()
                .unwrap(),
            Url {
                identity: Identity {
                    domain: "https://specs.apollo.dev".to_string(),
                    name: name!("federation").into(),
                },
                version: Version { major: 2, minor: 3 }
            }
        );
    }
}
