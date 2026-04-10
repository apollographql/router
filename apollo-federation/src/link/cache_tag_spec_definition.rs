// This `cacheTag` spec is a supergraph-only feature spec to indicate that some of the subgraphs
// use the `@cacheTag` directive. The `@cacheTag` directive itself is not used in supergraph
// schema, since `@cacheTag` directive applications are composed using the `@join__directive`
// directive.
// PORT_NOTE: Ported from internals-js/src/specs/cacheTagSpec.ts (federation PR #3274).
use std::sync::LazyLock;

use crate::link::Purpose;
use crate::link::spec::Identity;
use crate::link::spec::Url;
use crate::link::spec::Version;
use crate::link::spec_definition::SpecDefinition;
use crate::link::spec_definition::SpecDefinitions;
use crate::schema::type_and_directive_specification::TypeAndDirectiveSpecification;

pub(crate) struct CacheTagSpecDefinition {
    url: Url,
    minimum_federation_version: Version,
}

impl CacheTagSpecDefinition {
    pub(crate) fn new(version: Version, minimum_federation_version: Version) -> Self {
        Self {
            url: Url {
                identity: Identity::cache_tag_identity(),
                version,
            },
            minimum_federation_version,
        }
    }
}

impl SpecDefinition for CacheTagSpecDefinition {
    fn url(&self) -> &Url {
        &self.url
    }

    fn directive_specs(&self) -> Vec<Box<dyn TypeAndDirectiveSpecification>> {
        vec![]
    }

    fn type_specs(&self) -> Vec<Box<dyn TypeAndDirectiveSpecification>> {
        vec![]
    }

    fn minimum_federation_version(&self) -> &Version {
        &self.minimum_federation_version
    }

    fn purpose(&self) -> Option<Purpose> {
        Some(Purpose::EXECUTION)
    }
}

pub(crate) static CACHE_TAG_VERSIONS: LazyLock<SpecDefinitions<CacheTagSpecDefinition>> =
    LazyLock::new(|| {
        let mut definitions = SpecDefinitions::new(Identity::cache_tag_identity());
        definitions.add(CacheTagSpecDefinition::new(
            Version { major: 0, minor: 1 },
            Version {
                major: 2,
                minor: 12,
            },
        ));
        definitions
    });
