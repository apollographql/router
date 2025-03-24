use std::sync::Arc;
use std::sync::LazyLock;

use apollo_compiler::Name;
use apollo_compiler::ast::DirectiveLocation;
use apollo_compiler::ast::Type;
use apollo_compiler::name;
use apollo_compiler::ty;

use crate::bail;
use crate::error::FederationError;
use crate::error::MultiTry;
use crate::error::MultiTryAll;
use crate::error::SingleFederationError;
use crate::link::DEFAULT_IMPORT_SCALAR_NAME;
use crate::link::DEFAULT_LINK_NAME;
use crate::link::DEFAULT_PURPOSE_ENUM_NAME;
use crate::link::Link;
use crate::link::spec::Identity;
use crate::link::spec::Url;
use crate::link::spec::Version;
use crate::link::spec_definition::SpecDefinition;
use crate::link::spec_definition::SpecDefinitions;
use crate::schema::FederationSchema;
use crate::schema::type_and_directive_specification::ArgumentSpecification;
use crate::schema::type_and_directive_specification::DirectiveArgumentSpecification;
use crate::schema::type_and_directive_specification::DirectiveSpecification;
use crate::schema::type_and_directive_specification::EnumTypeSpecification;
use crate::schema::type_and_directive_specification::EnumValueSpecification;
use crate::schema::type_and_directive_specification::ScalarTypeSpecification;
use crate::schema::type_and_directive_specification::TypeAndDirectiveSpecification;

pub(crate) const LINK_DIRECTIVE_AS_ARGUMENT_NAME: Name = name!("as");
pub(crate) const LINK_DIRECTIVE_URL_ARGUMENT_NAME: Name = name!("url");
pub(crate) const LINK_DIRECTIVE_FOR_ARGUMENT_NAME: Name = name!("for");
pub(crate) const LINK_DIRECTIVE_IMPORT_ARGUMENT_NAME: Name = name!("import");

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
        let mut specs = vec![
            DirectiveArgumentSpecification {
                base_spec: ArgumentSpecification {
                    name: LINK_DIRECTIVE_URL_ARGUMENT_NAME,
                    get_type: |_, _| Ok(ty!(String)),
                    default_value: None,
                },
                composition_strategy: None,
            },
            DirectiveArgumentSpecification {
                base_spec: ArgumentSpecification {
                    name: LINK_DIRECTIVE_AS_ARGUMENT_NAME,
                    get_type: |_, _| Ok(ty!(String)),
                    default_value: None,
                },
                composition_strategy: None,
            },
        ];
        if self.supports_purpose() {
            specs.push(DirectiveArgumentSpecification {
                base_spec: ArgumentSpecification {
                    name: LINK_DIRECTIVE_FOR_ARGUMENT_NAME,
                    get_type: |_schema, link| {
                        let Some(link) = link else {
                            bail!(
                                "Type {DEFAULT_PURPOSE_ENUM_NAME} shouldn't be added without being attached to a @link spec"
                            )
                        };
                        Ok(Type::Named(link.type_name_in_schema(&DEFAULT_PURPOSE_ENUM_NAME)))
                    },
                    default_value: None,
                },
                composition_strategy: None,
            });
        }
        if self.supports_import() {
            specs.push(DirectiveArgumentSpecification {
                base_spec: ArgumentSpecification {
                    name: LINK_DIRECTIVE_IMPORT_ARGUMENT_NAME,
                    get_type: |_, link| {
                        let Some(link) = link else {
                            bail!(
                                "Type {DEFAULT_IMPORT_SCALAR_NAME} shouldn't be added without being attached to a @link spec"
                            )
                        };
                        Ok(Type::List(Box::new(Type::Named(
                            link.type_name_in_schema(&DEFAULT_IMPORT_SCALAR_NAME),
                        ))))
                    },
                    default_value: None,
                },
                composition_strategy: None,
            });
        }
        specs
    }

    fn supports_purpose(&self) -> bool {
        self.version().gt(&Version { major: 0, minor: 1 })
    }

    fn supports_import(&self) -> bool {
        self.version().satisfies(&Version { major: 1, minor: 0 })
    }

    #[allow(dead_code)]
    fn add_to_schema(
        &self,
        alias: Option<Name>,
        schema: &mut FederationSchema,
    ) -> Result<(), FederationError> {
        self.add_definitions_to_schema(schema, alias)?;

        // TODO: port `SchemaDefinition::hasExtensionElements/hasNonExtensionElements`
        todo!()
    }

    fn add_definitions_to_schema(
        &self,
        schema: &mut FederationSchema,
        alias: Option<Name>,
        // imports: Vec<Import>, // TODO
    ) -> Result<(), FederationError> {
        if let Some(_metadata) = schema.metadata() {
            // TODO: port `existingCore.coreItself.url.identity === this.identity`
            let self_fmt = format!("{}/{}", self.identity(), self.version());
            return Err(SingleFederationError::InvalidLinkDirectiveUsage {
                message: format!(
                    "Cannot add feature {self_fmt} to the schema, it already uses (unknown/TODO)",
                ),
            }
            .into());
        }

        // The @link spec is special in that it is the one that bootstrap everything, and by the
        // time this method is called, the `schema` may not yet have any `schema.metadata()` set up
        // yet. To have `check_or_add` calls below still work, we pass a mock link object with the
        // proper information (not that the `Directive` we pass is not complete and not even
        // attached to the schema, but that is not used in practice so unused).
        let mock_link = Arc::new(Link {
            url: self.url.clone(),
            spec_alias: alias,
            imports: vec![], // TODO
            purpose: None,
        });
        Ok(())
            .and_try(create_link_purpose_type_spec().check_or_add(schema, Some(&mock_link)))
            .and_try(create_link_import_type_spec().check_or_add(schema, Some(&mock_link)))
            .and_try(
                self.directive_specs()
                    .into_iter()
                    .try_for_all(|spec| spec.check_or_add(schema, Some(&mock_link))),
            )
    }
}

impl SpecDefinition for LinkSpecDefinition {
    fn url(&self) -> &Url {
        &self.url
    }

    fn directive_specs(&self) -> Vec<Box<dyn TypeAndDirectiveSpecification>> {
        vec![Box::new(DirectiveSpecification::new(
            DEFAULT_LINK_NAME,
            &self.create_definition_argument_specifications(),
            true,
            &[DirectiveLocation::Schema],
            false,
            None,
        ))]
    }

    fn type_specs(&self) -> Vec<Box<dyn TypeAndDirectiveSpecification>> {
        let mut specs: Vec<Box<dyn TypeAndDirectiveSpecification>> = Vec::with_capacity(2);
        if self.supports_purpose() {
            specs.push(Box::new(create_link_purpose_type_spec()))
        }
        if self.supports_import() {
            specs.push(Box::new(create_link_import_type_spec()))
        }
        specs
    }

    fn add_elements_to_schema(
        &self,
        _schema: &mut FederationSchema,
    ) -> Result<(), FederationError> {
        // Link is special and the @link directive is added in `add_to_schema` above
        Ok(())
    }
}

fn create_link_purpose_type_spec() -> EnumTypeSpecification {
    EnumTypeSpecification {
        name: DEFAULT_PURPOSE_ENUM_NAME,
        values: vec![
            EnumValueSpecification {
                name: name!("SECURITY"),
                description: Some(
                    "`SECURITY` features provide metadata necessary to securely resolve fields."
                        .to_string(),
                ),
            },
            EnumValueSpecification {
                name: name!("EXECUTION"),
                description: Some(
                    "`EXECUTION` features provide metadata necessary for operation execution."
                        .to_string(),
                ),
            },
        ],
    }
}

fn create_link_import_type_spec() -> ScalarTypeSpecification {
    ScalarTypeSpecification {
        name: DEFAULT_IMPORT_SCALAR_NAME,
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
