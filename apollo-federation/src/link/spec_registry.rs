use std::collections::BTreeSet;
use std::collections::HashMap;
use std::str;
use std::sync::LazyLock;

use apollo_compiler::Name;
use apollo_compiler::name;

use crate::connectors::spec::CONNECT_VERSIONS;
use crate::link::Link;
use crate::link::authenticated_spec_definition::AUTHENTICATED_VERSIONS;
use crate::link::cache_tag_spec_definition::CACHE_TAG_VERSIONS;
use crate::link::context_spec_definition::CONTEXT_VERSIONS;
use crate::link::cost_spec_definition::COST_VERSIONS;
use crate::link::event_spec_definition::EVENT_VERSIONS;
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
    pub const AUTHENTICATED_NAME: Name = name!("authenticated");
    pub fn authenticated_identity() -> Identity {
        Identity {
            domain: APOLLO_SPEC_DOMAIN.to_string(),
            name: Self::AUTHENTICATED_NAME.into(),
        }
    }

    pub const CACHE_TAG_NAME: Name = name!("cacheTag");
    pub fn cache_tag_identity() -> Identity {
        Identity {
            domain: APOLLO_SPEC_DOMAIN.to_string(),
            name: Self::CACHE_TAG_NAME.into(),
        }
    }

    pub const CONNECT_NAME: Name = name!("connect");
    pub fn connect_identity() -> Identity {
        Identity {
            domain: APOLLO_SPEC_DOMAIN.to_string(),
            name: Self::CONNECT_NAME.into(),
        }
    }

    pub const CONTEXT_NAME: Name = name!("context");
    pub fn context_identity() -> Identity {
        Identity {
            domain: APOLLO_SPEC_DOMAIN.to_string(),
            name: Self::CONTEXT_NAME.into(),
        }
    }

    pub const CORE_NAME: Name = name!("core");
    pub fn core_identity() -> Identity {
        Identity {
            domain: APOLLO_SPEC_DOMAIN.to_string(),
            name: Self::CORE_NAME.into(),
        }
    }

    pub const COST_NAME: Name = name!("cost");
    pub fn cost_identity() -> Identity {
        Identity {
            domain: APOLLO_SPEC_DOMAIN.to_string(),
            name: Self::COST_NAME.into(),
        }
    }

    pub const FEDERATION_NAME: Name = name!("federation");
    pub fn federation_identity() -> Identity {
        Identity {
            domain: APOLLO_SPEC_DOMAIN.to_string(),
            name: Self::FEDERATION_NAME.into(),
        }
    }

    pub const EVENT_NAME: Name = name!("event");
    pub fn event_identity() -> Identity {
        Identity {
            domain: APOLLO_SPEC_DOMAIN.to_string(),
            name: Self::EVENT_NAME.into(),
        }
    }

    pub const INACCESSIBLE_NAME: Name = name!("inaccessible");
    pub fn inaccessible_identity() -> Identity {
        Identity {
            domain: APOLLO_SPEC_DOMAIN.to_string(),
            name: Self::INACCESSIBLE_NAME.into(),
        }
    }

    pub const JOIN_NAME: Name = name!("join");
    pub fn join_identity() -> Identity {
        Identity {
            domain: APOLLO_SPEC_DOMAIN.to_string(),
            name: Self::JOIN_NAME.into(),
        }
    }

    pub const LINK_NAME: Name = name!("link");
    pub fn link_identity() -> Identity {
        Identity {
            domain: APOLLO_SPEC_DOMAIN.to_string(),
            name: Self::LINK_NAME.into(),
        }
    }

    pub const POLICY_NAME: Name = name!("policy");
    pub fn policy_identity() -> Identity {
        Identity {
            domain: APOLLO_SPEC_DOMAIN.to_string(),
            name: Self::POLICY_NAME.into(),
        }
    }

    pub const REQUIRES_SCOPES_NAME: Name = name!("requiresScopes");
    pub fn requires_scopes_identity() -> Identity {
        Identity {
            domain: APOLLO_SPEC_DOMAIN.to_string(),
            name: Self::REQUIRES_SCOPES_NAME.into(),
        }
    }

    pub const SOURCE_NAME: Name = name!("source");
    pub fn source_identity() -> Identity {
        Identity {
            domain: APOLLO_SPEC_DOMAIN.to_string(),
            name: Self::SOURCE_NAME.into(),
        }
    }

    pub const TAG_NAME: Name = name!("tag");
    pub fn tag_identity() -> Identity {
        Identity {
            domain: APOLLO_SPEC_DOMAIN.to_string(),
            name: Self::TAG_NAME.into(),
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
    registry.extend(&EVENT_VERSIONS);
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
        directive_name_in_spec: &Name,
    ) -> Option<DirectiveCompositionSpecification> {
        let specs = self.get_definition(&source.url)?.directive_specs();
        let spec = specs.iter().find(|s| s.name() == directive_name_in_spec)?;
        let directive_spec: DirectiveSpecification = spec.as_any().downcast_ref().cloned()?;
        directive_spec.composition
    }
}
