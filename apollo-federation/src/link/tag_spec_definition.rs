use std::sync::Arc;
use std::sync::LazyLock;

use apollo_compiler::name;
use apollo_compiler::schema::DirectiveLocation;

use crate::link::Purpose;
use crate::link::federation_spec_definition::FEDERATION_TAG_DIRECTIVE_NAME_IN_SPEC;
use crate::link::spec::Identity;
use crate::link::spec::Url;
use crate::link::spec::Version;
use crate::link::spec_definition::SpecDefinition;
use crate::link::spec_definition::SpecDefinitionLookup;
use crate::link::spec_definition::SpecDefinitions;
use crate::schema::type_and_directive_specification::ArgumentSpecification;
use crate::schema::type_and_directive_specification::DirectiveArgumentSpecification;
use crate::schema::type_and_directive_specification::DirectiveSpecification;

pub(crate) struct TagSpecDefinition {
    url: Url,
    minimum_federation_version: Version,
    specs: SpecDefinitionLookup,
}

impl TagSpecDefinition {
    pub(crate) fn new(version: Version, minimum_federation_version: Version) -> Self {
        let tag_directive_spec = Self::directive_specification(&version);
        Self {
            url: Url {
                identity: Identity::tag_identity(),
                version,
            },
            minimum_federation_version,
            specs: SpecDefinitionLookup::from([(
                tag_directive_spec.name().clone(),
                Arc::new(tag_directive_spec.into()),
            )]),
        }
    }

    fn directive_locations(version: &Version) -> Vec<DirectiveLocation> {
        // v0.1: FIELD_DEFINITION, OBJECT, INTERFACE, UNION
        // v0.2: + ARGUMENT_DEFINITION, SCALAR, ENUM, ENUM_VALUE, INPUT_OBJECT, INPUT_FIELD_DEFINITION
        // v0.3+: + SCHEMA
        let mut locations = vec![
            DirectiveLocation::FieldDefinition,
            DirectiveLocation::Object,
            DirectiveLocation::Interface,
            DirectiveLocation::Union,
        ];
        if *version != (Version { major: 0, minor: 1 }) {
            locations.extend([
                DirectiveLocation::ArgumentDefinition,
                DirectiveLocation::Scalar,
                DirectiveLocation::Enum,
                DirectiveLocation::EnumValue,
                DirectiveLocation::InputObject,
                DirectiveLocation::InputFieldDefinition,
            ]);
            if *version != (Version { major: 0, minor: 2 }) {
                locations.push(DirectiveLocation::Schema);
            }
        }
        locations
    }

    fn directive_specification(version: &Version) -> DirectiveSpecification {
        DirectiveSpecification::new(
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
            &Self::directive_locations(version),
            true, // composes
            Some(&|v| TAG_VERSIONS.get_dyn_minimum_required_version(v)),
            None,
        )
    }
}

impl SpecDefinition for TagSpecDefinition {
    fn url(&self) -> &Url {
        &self.url
    }

    fn minimum_federation_version(&self) -> &Version {
        &self.minimum_federation_version
    }

    fn purpose(&self) -> Option<Purpose> {
        None
    }

    fn specs(&self) -> &SpecDefinitionLookup {
        &self.specs
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
