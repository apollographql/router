use std::sync::LazyLock;

use apollo_compiler::schema::DirectiveLocation;

use super::federation_spec_definition::FEDERATION_CACHE_TAG_DIRECTIVE_NAME_IN_SPEC;
use super::federation_spec_definition::FEDERATION_FORMAT_ARGUMENT_NAME;
use crate::link::Purpose;
use crate::link::spec::Identity;
use crate::link::spec::Url;
use crate::link::spec::Version;
use crate::link::spec_definition::SpecDefinition;
use crate::link::spec_definition::SpecDefinitions;
use crate::schema::type_and_directive_specification::ArgumentSpecification;
use crate::schema::type_and_directive_specification::DirectiveArgumentSpecification;
use crate::schema::type_and_directive_specification::DirectiveSpecification;
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

    fn directive_locations(&self) -> Vec<DirectiveLocation> {
        vec![
            DirectiveLocation::FieldDefinition,
            DirectiveLocation::Object,
        ]
    }

    fn directive_specification(&self) -> Box<dyn TypeAndDirectiveSpecification> {
        // TODO: Port the JS federation PR (#3274), once Rust composition is implemented.
        Box::new(DirectiveSpecification::new(
            FEDERATION_CACHE_TAG_DIRECTIVE_NAME_IN_SPEC,
            &[DirectiveArgumentSpecification {
                base_spec: ArgumentSpecification {
                    name: FEDERATION_FORMAT_ARGUMENT_NAME,
                    get_type: |_, _| Ok(apollo_compiler::ty!(String!)),
                    default_value: None,
                },
                composition_strategy: None,
            }],
            true, // repeatable
            &self.directive_locations(),
            true, // composes
            Some(&|v| CACHE_TAG_VERSIONS.get_dyn_minimum_required_version(v)),
            None,
        ))
    }
}

impl SpecDefinition for CacheTagSpecDefinition {
    fn url(&self) -> &Url {
        &self.url
    }

    fn directive_specs(&self) -> Vec<Box<dyn TypeAndDirectiveSpecification>> {
        vec![self.directive_specification()]
    }

    fn type_specs(&self) -> Vec<Box<dyn TypeAndDirectiveSpecification>> {
        vec![]
    }

    fn minimum_federation_version(&self) -> &Version {
        &self.minimum_federation_version
    }

    fn purpose(&self) -> Option<Purpose> {
        None
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
