use std::sync::LazyLock;

use apollo_compiler::name;
use apollo_compiler::schema::DirectiveLocation;

use crate::link::Purpose;
use crate::link::federation_spec_definition::FEDERATION_TAG_DIRECTIVE_NAME_IN_SPEC;
use crate::link::spec::Identity;
use crate::link::spec::Url;
use crate::link::spec::Version;
use crate::link::spec_definition::SpecDefinition;
use crate::link::spec_definition::SpecDefinitions;
use crate::schema::type_and_directive_specification::ArgumentSpecification;
use crate::schema::type_and_directive_specification::DirectiveArgumentSpecification;
use crate::schema::type_and_directive_specification::DirectiveSpecification;
use crate::schema::type_and_directive_specification::TypeAndDirectiveSpecification;

pub(crate) struct TagSpecDefinition {
    url: Url,
    minimum_federation_version: Version,
}

impl TagSpecDefinition {
    pub(crate) fn new(version: Version, minimum_federation_version: Version) -> Self {
        Self {
            url: Url {
                identity: Identity::tag_identity(),
                version,
            },
            minimum_federation_version,
        }
    }

    fn directive_locations(&self) -> Vec<DirectiveLocation> {
        // v0.1: FIELD_DEFINITION, OBJECT, INTERFACE, UNION
        // v0.2: + ARGUMENT_DEFINITION, SCALAR, ENUM, ENUM_VALUE, INPUT_OBJECT, INPUT_FIELD_DEFINITION
        // v0.3+: + SCHEMA
        let mut locations = vec![
            DirectiveLocation::FieldDefinition,
            DirectiveLocation::Object,
            DirectiveLocation::Interface,
            DirectiveLocation::Union,
        ];
        if self.url.version != (Version { major: 0, minor: 1 }) {
            locations.extend([
                DirectiveLocation::ArgumentDefinition,
                DirectiveLocation::Scalar,
                DirectiveLocation::Enum,
                DirectiveLocation::EnumValue,
                DirectiveLocation::InputObject,
                DirectiveLocation::InputFieldDefinition,
            ]);
            if self.url.version != (Version { major: 0, minor: 2 }) {
                locations.push(DirectiveLocation::Schema);
            }
        }
        locations
    }

    fn directive_specification(&self) -> Box<dyn TypeAndDirectiveSpecification> {
        Box::new(DirectiveSpecification::new(
            FEDERATION_TAG_DIRECTIVE_NAME_IN_SPEC,
            &[DirectiveArgumentSpecification {
                base_spec: ArgumentSpecification {
                    name: name!("name"),
                    get_type: |_, _| Ok(apollo_compiler::ty!(String!)),
                    default_value: None,
                },
                composition_strategy: None,
            }],
            true, // repeatable
            &self.directive_locations(),
            true, // composes
            Some(&|v| TAG_VERSIONS.get_dyn_minimum_required_version(v)),
            None,
        ))
    }
}

impl SpecDefinition for TagSpecDefinition {
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

pub(crate) static TAG_VERSIONS: LazyLock<SpecDefinitions<TagSpecDefinition>> =
    LazyLock::new(|| {
        let mut definitions = SpecDefinitions::new(Identity::tag_identity());
        definitions.add(TagSpecDefinition::new(
            Version { major: 0, minor: 1 },
            Version { major: 1, minor: 0 },
        ));
        definitions.add(TagSpecDefinition::new(
            Version { major: 0, minor: 2 },
            Version { major: 1, minor: 0 },
        ));
        definitions.add(TagSpecDefinition::new(
            Version { major: 0, minor: 3 },
            Version { major: 2, minor: 0 },
        ));
        definitions
    });
