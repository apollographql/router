use std::sync::LazyLock;

use crate::link::spec::Identity;
use crate::link::spec::Url;
use crate::link::spec::Version;
use crate::link::spec_definition::SpecDefinition;
use crate::link::spec_definition::SpecDefinitions;

pub(crate) struct LinkSpecDefinition {
    url: Url,
}

impl LinkSpecDefinition {
    pub(crate) fn new(version: Version, identity: Identity) -> Self {
        Self {
            url: Url { identity, version },
        }
    }
}

impl SpecDefinition for LinkSpecDefinition {
    fn url(&self) -> &Url {
        &self.url
    }
}

pub(crate) static CORE_VERSIONS: LazyLock<SpecDefinitions<LinkSpecDefinition>> =
    LazyLock::new(|| {
        let mut definitions = SpecDefinitions::new(Identity::core_identity());
        definitions.add(LinkSpecDefinition::new(
            Version { major: 0, minor: 1 },
            Identity::core_identity(),
        ));
        definitions.add(LinkSpecDefinition::new(
            Version { major: 0, minor: 2 },
            Identity::core_identity(),
        ));
        definitions
    });
pub(crate) static LINK_VERSIONS: LazyLock<SpecDefinitions<LinkSpecDefinition>> =
    LazyLock::new(|| {
        let mut definitions = SpecDefinitions::new(Identity::link_identity());
        definitions.add(LinkSpecDefinition::new(
            Version { major: 1, minor: 0 },
            Identity::link_identity(),
        ));
        definitions
    });
