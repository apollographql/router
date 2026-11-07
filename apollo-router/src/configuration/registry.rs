//! The `registry` section of `router.yaml`: which OCI registry the router's artifacts come from,
//! how it authenticates to it, and which artifact fills each slot.
//!
//! Every slot is a repository under one registry host and path prefix, so a pull-through cache or
//! a mirror is declared once (`host` and `prefix`) and applies to the graph artifact, the
//! persisted-queries artifact and the entitlement alike. `APOLLO_GRAPH_ARTIFACT_REFERENCE`, the
//! graph slot's original form, keeps working as an override of `artifacts.graph`.
//!
//! Resolution happens on every configuration instance (see [`Registry::resolve`]), so a change to
//! this section on a configuration reload rebuilds the affected pollers.

use std::time::Duration;

use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;

use super::ConfigurationError;
use crate::registry::OciConfig;
use crate::registry::RegistryAuthMode;
use crate::registry::is_apollo_graph_artifact_reference;
use crate::registry::should_use_ssl;
use crate::registry::validate_oci_reference;

/// Apollo's own registry, used when `registry.host` is not set.
pub(crate) const DEFAULT_REGISTRY_HOST: &str = "artifact.api.apollographql.com";

/// How often tag references are checked for a new version when `registry.poll_interval` is not
/// set.
pub(crate) const DEFAULT_POLL_INTERVAL: Duration = Duration::from_secs(30);

/// Where the router fetches its artifacts from.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields, default)]
pub(crate) struct Registry {
    /// The registry host, with an optional port: Apollo's `artifact.api.apollographql.com` when
    /// unset, or a pull-through cache or mirror of it such as `mirror.internal:5000`.
    pub(crate) host: Option<String>,

    /// The path under which the registry mounts Apollo's repositories, for caches that place an
    /// upstream under a prefix (`apollo-remote` makes the graph artifact
    /// `mirror.internal/apollo-remote/<graph>:<variant>`). Unset for a registry that mounts them
    /// at its root, like Apollo's own.
    pub(crate) prefix: Option<String>,

    /// How the router authenticates to the registry. `auto` (the default) sends the graph API key
    /// to an Apollo-hosted registry and uses the Docker credential store for any other host.
    pub(crate) auth: RegistryAuthMode,

    /// How often tag references are checked for a new version, as a duration such as `30s`.
    /// Defaults to 30 seconds.
    #[serde(deserialize_with = "humantime_serde::deserialize", default)]
    #[schemars(with = "String", default)]
    pub(crate) poll_interval: Option<Duration>,

    /// Use plain HTTP instead of HTTPS for this registry (a local or in-cluster cache without
    /// TLS). Never set this for a registry reached over the public internet.
    pub(crate) insecure: bool,

    /// The artifacts to fetch, by slot.
    pub(crate) artifacts: Artifacts,
}

/// The artifact filling each slot. Every reference is relative to `registry.host` and
/// `registry.prefix` unless it names a host itself.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields, default)]
pub(crate) struct Artifacts {
    /// The graph artifact carrying the supergraph schema: `<graph>:<variant>` to follow the
    /// variant, or `<graph>@sha256:<digest>` to pin a launch. The same artifact
    /// `APOLLO_GRAPH_ARTIFACT_REFERENCE` names; the environment variable overrides this slot.
    pub(crate) graph: Option<ArtifactReference>,

    /// The persisted-queries artifact: `persistedqueries/<graph>:<variant>` to follow the
    /// variant's persisted query list, or a digest to pin a revision. Tag references are always
    /// followed, independently of `--hot-reload`. Unset, persisted queries come from Apollo
    /// Uplink (or `persisted_queries.local_manifests`, which takes precedence over both).
    pub(crate) persisted_queries: Option<ArtifactReference>,

    /// The entitlement artifact carrying the router's license. Normally unset: the router reads
    /// the entitlement identifier off the graph artifact's manifest and fetches
    /// `entitlements/<id>:latest` next to it. Set it only to point the license at a different
    /// repository than that derivation gives.
    pub(crate) entitlement: Option<ArtifactReference>,
}

/// One artifact slot.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct ArtifactReference {
    /// An OCI reference: `<repository>:<tag>` or `<repository>@sha256:<digest>`, relative to the
    /// registry's host and prefix. A reference whose first path segment is a host (it contains
    /// a `.` or a `:`, or is `localhost`) is taken as absolute and used as written.
    pub(crate) reference: String,
}

/// Inputs to resolution that live outside `router.yaml`: the CLI flags and environment variables
/// the router was started with. Not user configuration and not subject to semver.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RegistryOverrides {
    /// `APOLLO_GRAPH_ARTIFACT_REFERENCE` / `--graph-artifact-reference`: a full reference that
    /// overrides `registry.artifacts.graph`.
    pub graph_reference: Option<String>,
    /// `APOLLO_KEY`, sent to Apollo-hosted registries.
    pub apollo_key: Option<String>,
    /// `--hot-reload`: whether a tag reference for the graph artifact is followed.
    pub hot_reload: bool,
    /// A poll interval that wins over `registry.poll_interval` (tests only).
    pub poll_interval: Option<Duration>,
}

/// The per-slot fetch configuration a [`Registry`] resolves to under a set of
/// [`RegistryOverrides`]. Carried on every `Configuration` instance so the pollers it drives are
/// rebuilt when it changes.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct ResolvedRegistry {
    /// The graph artifact, from `APOLLO_GRAPH_ARTIFACT_REFERENCE` or `registry.artifacts.graph`.
    pub(crate) graph: Option<OciConfig>,
    /// The persisted-queries artifact, from `registry.artifacts.persisted_queries`.
    pub(crate) persisted_queries: Option<OciConfig>,
    /// An explicit entitlement artifact, from `registry.artifacts.entitlement`. `None` means the
    /// license is derived from the graph artifact's manifest.
    pub(crate) entitlement: Option<OciConfig>,
}

impl Registry {
    /// Turn the section into per-slot fetch configuration.
    ///
    /// The graph slot takes the environment reference over the declared one; when both are set
    /// they must name the same registry, since the entitlement and the other slots are fetched
    /// from the declared one and a split would be impossible to reason about.
    pub(crate) fn resolve(
        &self,
        overrides: &RegistryOverrides,
    ) -> Result<ResolvedRegistry, ConfigurationError> {
        let declared_graph = self
            .artifacts
            .graph
            .as_ref()
            .map(|slot| self.absolute(&slot.reference));
        let graph_reference = match (&overrides.graph_reference, declared_graph) {
            (Some(from_env), Some(declared)) => {
                let env_registry = registry_of(from_env);
                let declared_registry = registry_of(&declared);
                if env_registry != declared_registry {
                    return Err(invalid(
                        "APOLLO_GRAPH_ARTIFACT_REFERENCE and registry.artifacts.graph name different registries",
                        format!(
                            "the environment names '{env_registry}' and the configuration names '{declared_registry}'; use one or make them agree"
                        ),
                    ));
                }
                Some(from_env.clone())
            }
            (Some(from_env), None) => Some(from_env.clone()),
            (None, Some(declared)) => Some(declared),
            (None, None) => None,
        };

        Ok(ResolvedRegistry {
            graph: graph_reference
                .map(|reference| self.oci_config(reference, overrides, overrides.hot_reload))
                .transpose()?,
            persisted_queries: self
                .artifacts
                .persisted_queries
                .as_ref()
                .map(|slot| self.oci_config(self.absolute(&slot.reference), overrides, true))
                .transpose()?,
            entitlement: self
                .artifacts
                .entitlement
                .as_ref()
                .map(|slot| self.oci_config(self.absolute(&slot.reference), overrides, true))
                .transpose()?,
        })
    }

    /// `host/prefix/` with exactly one `/` between and after each part.
    fn base(&self) -> String {
        let host = self
            .host
            .as_deref()
            .map(|host| host.trim_matches('/'))
            .filter(|host| !host.is_empty())
            .unwrap_or(DEFAULT_REGISTRY_HOST);
        match self
            .prefix
            .as_deref()
            .map(|prefix| prefix.trim_matches('/'))
            .filter(|prefix| !prefix.is_empty())
        {
            Some(prefix) => format!("{host}/{prefix}/"),
            None => format!("{host}/"),
        }
    }

    /// A slot's reference as a full OCI reference.
    fn absolute(&self, reference: &str) -> String {
        if is_absolute_reference(reference) {
            reference.to_string()
        } else {
            format!("{}{}", self.base(), reference.trim_start_matches('/'))
        }
    }

    fn oci_config(
        &self,
        reference: String,
        overrides: &RegistryOverrides,
        hot_reload: bool,
    ) -> Result<OciConfig, ConfigurationError> {
        let (reference, _) = validate_oci_reference(&reference)
            .map_err(|err| invalid("invalid registry artifact reference", err.to_string()))?;

        // Only the key that will actually be sent is carried: `auto` sends it to Apollo-hosted
        // registries (where it is required) and ignores it elsewhere.
        let apollo_key = match self.auth {
            RegistryAuthMode::Auto if is_apollo_graph_artifact_reference(&reference) => {
                Some(require_apollo_key(overrides, &reference)?)
            }
            RegistryAuthMode::Auto => overrides.apollo_key.clone(),
            RegistryAuthMode::ApolloKey => Some(require_apollo_key(overrides, &reference)?),
            RegistryAuthMode::DockerCredentials | RegistryAuthMode::Anonymous => None,
        };

        Ok(OciConfig {
            apollo_key,
            use_ssl: !self.insecure && should_use_ssl(&reference),
            reference,
            hot_reload,
            poll_interval: overrides
                .poll_interval
                .or(self.poll_interval)
                .unwrap_or(DEFAULT_POLL_INTERVAL),
            auth: self.auth,
        })
    }
}

fn require_apollo_key(
    overrides: &RegistryOverrides,
    reference: &str,
) -> Result<String, ConfigurationError> {
    overrides.apollo_key.clone().ok_or_else(|| {
        invalid(
            "APOLLO_KEY is required to fetch from this registry",
            format!(
                "'{reference}' needs the graph API key; set the APOLLO_KEY environment variable"
            ),
        )
    })
}

/// Docker's rule for telling a host from a repository path: the first path segment is a host
/// when it contains a `.` or a `:`, or is `localhost`. A reference with no `/` at all is a bare
/// repository (`my-graph:prod`), never a host.
fn is_absolute_reference(reference: &str) -> bool {
    match reference.split_once('/') {
        Some((first, _)) => first.contains('.') || first.contains(':') || first == "localhost",
        None => false,
    }
}

/// The host part of a full reference, for comparing where two references point.
fn registry_of(reference: &str) -> String {
    reference
        .parse::<oci_client::Reference>()
        .map(|parsed| parsed.registry().to_string())
        .unwrap_or_else(|_| reference.split('/').next().unwrap_or_default().to_string())
}

fn invalid(message: &'static str, error: String) -> ConfigurationError {
    ConfigurationError::InvalidConfiguration { message, error }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn slot(reference: &str) -> Option<ArtifactReference> {
        Some(ArtifactReference {
            reference: reference.to_string(),
        })
    }

    fn overrides() -> RegistryOverrides {
        RegistryOverrides {
            graph_reference: None,
            apollo_key: Some("key".to_string()),
            hot_reload: true,
            poll_interval: None,
        }
    }

    #[test]
    fn relative_references_resolve_under_the_default_host() {
        let registry = Registry {
            artifacts: Artifacts {
                graph: slot("my-graph:prod"),
                persisted_queries: slot("persistedqueries/my-graph:prod"),
                entitlement: None,
            },
            ..Default::default()
        };
        let resolved = registry.resolve(&overrides()).unwrap();
        let graph = resolved.graph.unwrap();
        assert_eq!(
            graph.reference,
            "artifact.api.apollographql.com/my-graph:prod"
        );
        assert_eq!(graph.apollo_key.as_deref(), Some("key"));
        assert!(graph.hot_reload);
        assert!(graph.use_ssl);
        assert_eq!(graph.poll_interval, DEFAULT_POLL_INTERVAL);
        let persisted_queries = resolved.persisted_queries.unwrap();
        assert_eq!(
            persisted_queries.reference,
            "artifact.api.apollographql.com/persistedqueries/my-graph:prod"
        );
        // Tag references for persisted queries are always followed.
        assert!(persisted_queries.hot_reload);
        assert!(resolved.entitlement.is_none());
    }

    #[test]
    fn host_and_prefix_apply_to_every_slot() {
        let registry = Registry {
            host: Some("mirror.internal:5000".to_string()),
            prefix: Some("/apollo-remote/".to_string()),
            poll_interval: Some(Duration::from_secs(5)),
            insecure: true,
            artifacts: Artifacts {
                graph: slot("my-graph:prod"),
                persisted_queries: slot("persistedqueries/my-graph:prod"),
                entitlement: slot("entitlements/abc:latest"),
            },
            ..Default::default()
        };
        let resolved = registry.resolve(&overrides()).unwrap();
        assert_eq!(
            resolved.graph.as_ref().unwrap().reference,
            "mirror.internal:5000/apollo-remote/my-graph:prod"
        );
        assert_eq!(
            resolved.persisted_queries.as_ref().unwrap().reference,
            "mirror.internal:5000/apollo-remote/persistedqueries/my-graph:prod"
        );
        assert_eq!(
            resolved.entitlement.as_ref().unwrap().reference,
            "mirror.internal:5000/apollo-remote/entitlements/abc:latest"
        );
        let graph = resolved.graph.unwrap();
        assert!(!graph.use_ssl);
        assert_eq!(graph.poll_interval, Duration::from_secs(5));
    }

    #[test]
    fn an_absolute_reference_is_used_as_written() {
        let registry = Registry {
            host: Some("mirror.internal".to_string()),
            artifacts: Artifacts {
                graph: slot("other.example.com/team/my-graph:prod"),
                persisted_queries: slot("localhost:5000/persistedqueries/my-graph:prod"),
                entitlement: None,
            },
            ..Default::default()
        };
        let resolved = registry.resolve(&overrides()).unwrap();
        assert_eq!(
            resolved.graph.unwrap().reference,
            "other.example.com/team/my-graph:prod"
        );
        assert_eq!(
            resolved.persisted_queries.unwrap().reference,
            "localhost:5000/persistedqueries/my-graph:prod"
        );
    }

    #[test]
    fn the_environment_reference_overrides_the_declared_graph_on_the_same_registry() {
        let registry = Registry {
            artifacts: Artifacts {
                graph: slot("my-graph:prod"),
                ..Default::default()
            },
            ..Default::default()
        };
        let resolved = registry
            .resolve(&RegistryOverrides {
                graph_reference: Some(
                    "artifact.api.apollographql.com/my-graph:staging".to_string(),
                ),
                ..overrides()
            })
            .unwrap();
        assert_eq!(
            resolved.graph.unwrap().reference,
            "artifact.api.apollographql.com/my-graph:staging"
        );
    }

    #[test]
    fn the_environment_reference_must_agree_with_the_declared_registry() {
        let registry = Registry {
            host: Some("mirror.internal".to_string()),
            artifacts: Artifacts {
                graph: slot("my-graph:prod"),
                ..Default::default()
            },
            ..Default::default()
        };
        let err = registry
            .resolve(&RegistryOverrides {
                graph_reference: Some("artifact.api.apollographql.com/my-graph:prod".to_string()),
                ..overrides()
            })
            .unwrap_err();
        assert!(
            err.to_string().contains("name different registries"),
            "{err}"
        );
    }

    #[test]
    fn an_empty_section_with_only_the_environment_reference_is_the_legacy_setup() {
        let resolved = Registry::default()
            .resolve(&RegistryOverrides {
                graph_reference: Some("artifact.api.apollographql.com/my-graph:prod".to_string()),
                hot_reload: false,
                ..overrides()
            })
            .unwrap();
        let graph = resolved.graph.unwrap();
        assert_eq!(
            graph.reference,
            "artifact.api.apollographql.com/my-graph:prod"
        );
        assert!(!graph.hot_reload);
        assert!(resolved.persisted_queries.is_none());
    }

    #[test]
    fn auth_modes_decide_whether_the_key_is_carried() {
        let declared = |auth, host: &str| Registry {
            host: Some(host.to_string()),
            auth,
            artifacts: Artifacts {
                graph: slot("my-graph:prod"),
                ..Default::default()
            },
            ..Default::default()
        };
        let key_for = |registry: Registry, apollo_key: Option<&str>| {
            registry
                .resolve(&RegistryOverrides {
                    apollo_key: apollo_key.map(str::to_string),
                    ..overrides()
                })
                .map(|resolved| resolved.graph.unwrap().apollo_key)
        };

        // auto: required for Apollo, carried but unused elsewhere.
        assert_eq!(
            key_for(
                declared(RegistryAuthMode::Auto, "artifact.api.apollographql.com"),
                Some("k")
            )
            .unwrap(),
            Some("k".to_string())
        );
        assert!(
            key_for(
                declared(RegistryAuthMode::Auto, "artifact.api.apollographql.com"),
                None
            )
            .is_err()
        );
        assert_eq!(
            key_for(declared(RegistryAuthMode::Auto, "mirror.internal"), None).unwrap(),
            None
        );
        // apollo_key: required regardless of host, for a proxy in front of Apollo's registry.
        assert_eq!(
            key_for(
                declared(RegistryAuthMode::ApolloKey, "mirror.internal"),
                Some("k")
            )
            .unwrap(),
            Some("k".to_string())
        );
        assert!(
            key_for(
                declared(RegistryAuthMode::ApolloKey, "mirror.internal"),
                None
            )
            .is_err()
        );
        // docker_credentials / anonymous: never carried.
        assert_eq!(
            key_for(
                declared(
                    RegistryAuthMode::DockerCredentials,
                    "artifact.api.apollographql.com"
                ),
                Some("k")
            )
            .unwrap(),
            None
        );
        assert_eq!(
            key_for(
                declared(RegistryAuthMode::Anonymous, "mirror.internal"),
                Some("k")
            )
            .unwrap(),
            None
        );
    }

    #[test]
    fn an_invalid_reference_is_a_configuration_error() {
        let registry = Registry {
            artifacts: Artifacts {
                graph: slot("my-graph:prod@sha256:nope"),
                ..Default::default()
            },
            ..Default::default()
        };
        assert!(registry.resolve(&overrides()).is_err());
    }
}
