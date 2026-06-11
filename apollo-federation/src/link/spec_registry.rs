use std::collections::BTreeSet;
use std::collections::HashMap;
use std::str;
use std::sync::LazyLock;

use apollo_compiler::name;

use crate::connectors::spec::CONNECT_VERSIONS;
use crate::link::Import;
use crate::link::Link;
use crate::link::authenticated_spec_definition::AUTHENTICATED_VERSIONS;
use crate::link::cache_tag_spec_definition::CACHE_TAG_VERSIONS;
use crate::link::context_spec_definition::CONTEXT_VERSIONS;
use crate::link::cost_spec_definition::COST_VERSIONS;
use crate::link::federation_spec_definition::FEDERATION_VERSIONS;
use crate::link::inaccessible_spec_definition::INACCESSIBLE_VERSIONS;
use crate::link::join_spec_definition::JOIN_VERSIONS;
use crate::link::policy_spec_definition::POLICY_VERSIONS;
use crate::link::requires_scopes_spec_definition::REQUIRES_SCOPES_VERSIONS;
use crate::link::spec::Identity;
use crate::link::spec::Url;
use crate::link::spec::Version;
use crate::link::spec_definition::SpecDefinition;
use crate::link::spec_definition::SpecDefinitions;
use crate::link::tag_spec_definition::TAG_VERSIONS;
use crate::schema::type_and_directive_specification::DirectiveCompositionSpecification;
use crate::schema::type_and_directive_specification::DirectiveSpecification;

pub(crate) const APOLLO_SPEC_DOMAIN: &str = "https://specs.apollo.dev";

impl Identity {
    pub fn authenticated_identity() -> Identity {
        Identity {
            domain: APOLLO_SPEC_DOMAIN.to_string(),
            name: name!("authenticated"),
        }
    }

    pub fn cache_tag_identity() -> Identity {
        Identity {
            domain: APOLLO_SPEC_DOMAIN.to_string(),
            name: name!("cacheTag"),
        }
    }

    pub fn connect_identity() -> Identity {
        Identity {
            domain: APOLLO_SPEC_DOMAIN.to_string(),
            name: name!("connect"),
        }
    }

    pub fn context_identity() -> Identity {
        Identity {
            domain: APOLLO_SPEC_DOMAIN.to_string(),
            name: name!("context"),
        }
    }

    pub fn core_identity() -> Identity {
        Identity {
            domain: APOLLO_SPEC_DOMAIN.to_string(),
            name: name!("core"),
        }
    }

    pub fn cost_identity() -> Identity {
        Identity {
            domain: APOLLO_SPEC_DOMAIN.to_string(),
            name: name!("cost"),
        }
    }

    pub fn federation_identity() -> Identity {
        Identity {
            domain: APOLLO_SPEC_DOMAIN.to_string(),
            name: name!("federation"),
        }
    }

    pub fn inaccessible_identity() -> Identity {
        Identity {
            domain: APOLLO_SPEC_DOMAIN.to_string(),
            name: name!("inaccessible"),
        }
    }

    pub fn join_identity() -> Identity {
        Identity {
            domain: APOLLO_SPEC_DOMAIN.to_string(),
            name: name!("join"),
        }
    }

    pub fn link_identity() -> Identity {
        Identity {
            domain: APOLLO_SPEC_DOMAIN.to_string(),
            name: name!("link"),
        }
    }

    pub fn policy_identity() -> Identity {
        Identity {
            domain: APOLLO_SPEC_DOMAIN.to_string(),
            name: name!("policy"),
        }
    }

    pub fn requires_scopes_identity() -> Identity {
        Identity {
            domain: APOLLO_SPEC_DOMAIN.to_string(),
            name: name!("requiresScopes"),
        }
    }

    pub fn source_identity() -> Identity {
        Identity {
            domain: APOLLO_SPEC_DOMAIN.to_string(),
            name: name!("source"),
        }
    }

    pub fn tag_identity() -> Identity {
        Identity {
            domain: APOLLO_SPEC_DOMAIN.to_string(),
            name: name!("tag"),
        }
    }
}

pub(crate) static SPEC_REGISTRY: LazyLock<SpecRegistry> = LazyLock::new(|| {
    let mut registry = SpecRegistry::new();
    registry.extend(&AUTHENTICATED_VERSIONS);
    registry.extend(&CACHE_TAG_VERSIONS);
    registry.extend(&CONNECT_VERSIONS);
    registry.extend(&CONTEXT_VERSIONS);
    registry.extend(&COST_VERSIONS);
    registry.extend(&FEDERATION_VERSIONS);
    registry.extend(&INACCESSIBLE_VERSIONS);
    registry.extend(&POLICY_VERSIONS);
    registry.extend(&REQUIRES_SCOPES_VERSIONS);
    registry.extend(&TAG_VERSIONS);
    registry.extend(&JOIN_VERSIONS);
    registry
});

pub(crate) struct SpecRegistry {
    definitions_by_url: HashMap<Url, &'static (dyn SpecDefinition + Sync)>,
    available_versions_by_identity: HashMap<Identity, BTreeSet<Version>>,
}

impl SpecRegistry {
    fn new() -> Self {
        Self {
            definitions_by_url: HashMap::new(),
            available_versions_by_identity: HashMap::new(),
        }
    }

    fn extend<T: SpecDefinition + Sync>(&mut self, definitions: &'static SpecDefinitions<T>) {
        for (v, spec) in definitions.iter() {
            self.definitions_by_url.insert(spec.url().clone(), spec);
            self.available_versions_by_identity
                .entry(spec.url().identity.clone())
                .or_default()
                .insert(v.clone());
        }
    }

    pub(crate) fn get_definition(&self, url: &Url) -> Option<&&(dyn SpecDefinition + Sync)> {
        self.definitions_by_url.get(url)
    }

    pub(crate) fn get_versions(&self, identity: &Identity) -> Option<&BTreeSet<Version>> {
        self.available_versions_by_identity.get(identity)
    }

    /// Generates the composition spec for an imported directive. Currently, this generates the
    /// entire spec, then loops over available directive specifications and clones the applicable
    /// directive. An alternative would be to mark everything as `Sync` and store them on the
    /// individual feature specs, but we have omitted this for now due to a non-trivial (~10%)
    /// increase in heap usage that affects query planning.
    pub(crate) fn get_composition_spec(
        &self,
        source: &Link,
        directive_import: &Import,
    ) -> Option<DirectiveCompositionSpecification> {
        let specs = self.get_definition(&source.url)?.directive_specs();
        let spec = specs
            .iter()
            .find(|s| *s.name() == directive_import.element)?;
        let directive_spec: DirectiveSpecification = spec.as_any().downcast_ref().cloned()?;
        directive_spec.composition
    }
}
