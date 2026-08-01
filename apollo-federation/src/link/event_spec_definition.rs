use std::sync::LazyLock;

use apollo_compiler::Name;
use apollo_compiler::ast::Type;
use apollo_compiler::name;
use apollo_compiler::schema::DirectiveLocation;

use crate::link::Purpose;
use crate::link::spec::Identity;
use crate::link::spec::Url;
use crate::link::spec::Version;
use crate::link::spec_definition::SpecDefinition;
use crate::link::spec_definition::SpecDefinitions;
use crate::schema::type_and_directive_specification::ArgumentSpecification;
use crate::schema::type_and_directive_specification::DirectiveArgumentSpecification;
use crate::schema::type_and_directive_specification::DirectiveCompositionOptions;
use crate::schema::type_and_directive_specification::DirectiveSpecification;
use crate::schema::type_and_directive_specification::TypeAndDirectiveSpecification;

pub(crate) const SUBSCRIBE_DIRECTIVE_NAME_IN_SPEC: Name = name!("subscribe");

#[derive(Clone)]
pub(crate) struct EventSpecDefinition {
    url: Url,
    minimum_federation_version: Version,
}

impl EventSpecDefinition {
    fn new(version: Version, minimum_federation_version: Version) -> Self {
        Self {
            url: Url {
                identity: Identity::event_identity(),
                version,
            },
            minimum_federation_version,
        }
    }

    fn directive_specification(&self) -> Box<dyn TypeAndDirectiveSpecification> {
        Box::new(DirectiveSpecification::new(
            SUBSCRIBE_DIRECTIVE_NAME_IN_SPEC,
            &[
                DirectiveArgumentSpecification {
                    base_spec: ArgumentSpecification {
                        name: name!("source"),
                        get_type: |_schema, _link| Ok(Type::NonNullNamed(name!("String"))),
                        default_value: None,
                    },
                    composition_strategy: None,
                },
                DirectiveArgumentSpecification {
                    base_spec: ArgumentSpecification {
                        name: name!("destinations"),
                        get_type: |_schema, _link| {
                            Ok(Type::NonNullList(Box::new(Type::NonNullNamed(name!(
                                "String"
                            )))))
                        },
                        default_value: None,
                    },
                    composition_strategy: None,
                },
            ],
            false,
            &[DirectiveLocation::FieldDefinition],
            Some(DirectiveCompositionOptions {
                supergraph_specification: &|version| {
                    EVENT_VERSIONS.get_dyn_minimum_required_version(version)
                },
                static_argument_transform: None,
                use_join_directive: true,
            }),
        ))
    }
}

impl SpecDefinition for EventSpecDefinition {
    fn url(&self) -> &Url {
        &self.url
    }

    fn directive_specs(&self) -> Vec<Box<dyn TypeAndDirectiveSpecification>> {
        vec![self.directive_specification()]
    }

    fn type_specs(&self) -> Vec<Box<dyn TypeAndDirectiveSpecification>> {
        Vec::new()
    }

    fn minimum_federation_version(&self) -> &Version {
        &self.minimum_federation_version
    }

    fn purpose(&self) -> Option<Purpose> {
        Some(Purpose::EXECUTION)
    }
}

pub(crate) static EVENT_VERSIONS: LazyLock<SpecDefinitions<EventSpecDefinition>> =
    LazyLock::new(|| {
        let mut definitions = SpecDefinitions::new(Identity::event_identity());
        definitions.add(EventSpecDefinition::new(
            Version { major: 0, minor: 1 },
            Version {
                major: 2,
                minor: 10,
            },
        ));
        definitions
    });
