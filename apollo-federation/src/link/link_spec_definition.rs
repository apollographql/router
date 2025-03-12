use std::sync::LazyLock;

use apollo_compiler::ast::DirectiveLocation;
use apollo_compiler::name;
use apollo_compiler::ty;

use crate::link::spec::Identity;
use crate::link::spec::Url;
use crate::link::spec::Version;
use crate::link::spec_definition::SpecDefinition;
use crate::link::spec_definition::SpecDefinitions;
use crate::schema::type_and_directive_specification::ArgumentSpecification;
use crate::schema::type_and_directive_specification::DirectiveArgumentSpecification;
use crate::schema::type_and_directive_specification::DirectiveSpecification;
use crate::schema::type_and_directive_specification::EnumTypeSpecification;
use crate::schema::type_and_directive_specification::EnumValueSpecification;
use crate::schema::type_and_directive_specification::TypeAndDirectiveSpecification;

pub(crate) struct LinkSpecDefinition {
    url: Url,
}

impl LinkSpecDefinition {
    pub(crate) fn new(version: Version, identity: Identity) -> Self {
        Self {
            url: Url { identity, version },
        }
    }

    fn create_definition_argument_specifications(&self) -> Vec<DirectiveArgumentSpecification> {
        vec![
            DirectiveArgumentSpecification {
                base_spec: ArgumentSpecification {
                    name: name!("url"),
                    get_type: |_| Ok(ty!(String)),
                    default_value: None,
                },
                composition_strategy: None, // TODO: Check if this is correct
            },
            DirectiveArgumentSpecification {
                base_spec: ArgumentSpecification {
                    name: name!("as"),
                    get_type: |_| Ok(ty!(String)),
                    default_value: None,
                },
                composition_strategy: None, // TODO: Check if this is correct
            },
            // TODO: Only sometimes
            DirectiveArgumentSpecification {
                base_spec: ArgumentSpecification {
                    name: name!("for"),
                    get_type: |_| Ok(ty!(String)), // TODO: Extract from schema
                    default_value: None,
                },
                composition_strategy: None,
            },
            DirectiveArgumentSpecification {
                base_spec: ArgumentSpecification {
                    name: name!("import"),
                    get_type: |_| Ok(ty!([String])), // TODO: Extract from schema
                    default_value: None,
                },
                composition_strategy: None,
            },
        ]
    }
}

impl SpecDefinition for LinkSpecDefinition {
    fn url(&self) -> &Url {
        &self.url
    }

    fn directive_specs(&self) -> Vec<Box<dyn TypeAndDirectiveSpecification>> {
        vec![Box::new(DirectiveSpecification::new(
            name!("link"), // TODO: This probably needs the name pulled from the bootstrap directive
            &self.create_definition_argument_specifications(),
            true,
            &vec![DirectiveLocation::Schema],
            false,
            None, // TODO: define composition spec
        ))]
    }

    fn type_specs(&self) -> Vec<Box<dyn TypeAndDirectiveSpecification>> {
        vec![Box::new(EnumTypeSpecification {
            name: name!("Purpose"),
            values: vec![
                EnumValueSpecification {
                    name: name!("SECURITY"),
                    description: Some(
                        "`SECURITY` features provide metadata necessary to securely resolve fields.".to_string(),
                    ),
                },
                EnumValueSpecification {
                    name: name!("EXECUTION"),
                    description: Some(
                        "`EXECUTION` features provide metadata necessary for operation execution.".to_string(),
                    ),
                },
            ],
        })]
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
