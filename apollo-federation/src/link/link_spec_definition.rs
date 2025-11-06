use std::sync::Arc;
use std::sync::LazyLock;

use apollo_compiler::Name;
use apollo_compiler::Node;
use apollo_compiler::ast::Argument;
use apollo_compiler::ast::Directive;
use apollo_compiler::ast::DirectiveLocation;
use apollo_compiler::ast::Type;
use apollo_compiler::ast::Value;
use apollo_compiler::name;
use apollo_compiler::schema::Component;
use apollo_compiler::ty;
use itertools::Itertools;

use crate::bail;
use crate::error::FederationError;
use crate::error::MultiTry;
use crate::error::MultiTryAll;
use crate::error::SingleFederationError;
use crate::link::DEFAULT_IMPORT_SCALAR_NAME;
use crate::link::DEFAULT_PURPOSE_ENUM_NAME;
use crate::link::Import;
use crate::link::Link;
use crate::link::Purpose;
use crate::link::argument::directive_optional_list_argument;
use crate::link::argument::directive_optional_string_argument;
use crate::link::spec::Identity;
use crate::link::spec::Url;
use crate::link::spec::Version;
use crate::link::spec_definition::SpecDefinition;
use crate::link::spec_definition::SpecDefinitions;
use crate::schema::FederationSchema;
use crate::schema::SchemaElement;
use crate::schema::position::SchemaDefinitionPosition;
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
pub(crate) const LINK_DIRECTIVE_FEATURE_ARGUMENT_NAME: Name = name!("feature"); // Fed 1's `url` argument

pub(crate) struct LinkSpecDefinition {
    url: Url,
    minimum_federation_version: Version,
}

impl LinkSpecDefinition {
    pub(crate) fn new(
        version: Version,
        identity: Identity,
        minimum_federation_version: Version,
    ) -> Self {
        Self {
            url: Url { identity, version },
            minimum_federation_version,
        }
    }

    fn create_definition_argument_specifications(&self) -> Vec<DirectiveArgumentSpecification> {
        let mut specs = vec![
            DirectiveArgumentSpecification {
                base_spec: ArgumentSpecification {
                    name: self.url_arg_name(),
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

    pub(crate) fn url_arg_name(&self) -> Name {
        if self.url.identity.name == Identity::core_identity().name {
            LINK_DIRECTIVE_FEATURE_ARGUMENT_NAME
        } else {
            LINK_DIRECTIVE_URL_ARGUMENT_NAME
        }
    }

    /// Add `self` (the @link spec definition) and a directive application of it to the schema.
    // Note: we may want to allow some `import` as argument to this method. When we do, we need to
    // watch for imports of `Purpose` and `Import` and add the types under their imported name.
    pub(crate) fn add_to_schema(
        &self,
        schema: &mut FederationSchema,
        alias: Option<Name>,
    ) -> Result<(), FederationError> {
        self.add_definitions_to_schema(schema, alias.clone(), vec![])?;

        // This adds `@link(url: "https://specs.apollo.dev/link/v1.0")` to the "schema" definition.
        // And we have a choice to add it either the main definition, or to an `extend schema`.
        //
        // In theory, always adding it to the main definition should be safe since even if some
        // root operations can be defined in extensions, you shouldn't have an extension without a
        // definition, and so we should never be in a case where _all_ root operations are defined
        // in extensions (which would be a problem for printing the definition itself since it's
        // syntactically invalid to have a schema definition with no operations).
        //
        // In practice however, graphQL-js has historically accepted extensions without definition
        // for schema, and we even abuse this a bit with federation out of convenience, so we could
        // end up in the situation where if we put the directive on the definition, it cannot be
        // printed properly due to the user having defined all its root operations in an extension.
        //
        // We could always add the directive to an extension, and that could kind of work but:
        // 1. the core/link spec says that the link-to-link application should be the first `@link`
        //   of the schema, but if user put some `@link` on their schema definition but we always
        //   put the link-to-link on an extension, then we're kind of not respecting our own spec
        //   (in practice, our own code can actually handle this as it does not strongly rely on
        //   that "it should be the first" rule, but that would set a bad example).
        // 2. earlier versions (pre-#1875) were always putting that directive on the definition,
        //   and we wanted to avoid surprising users by changing that for not reason.
        //
        // So instead, we put the directive on the schema definition unless some extensions exists
        // but no definition does (that is, no non-extension elements are populated).
        //
        // Side-note: this test must be done _before_ we call `insert_directive`, otherwise it
        // would take it into account.

        let name = alias.as_ref().unwrap_or(&self.url.identity.name).clone();
        let mut arguments = vec![Node::new(Argument {
            name: self.url_arg_name(),
            value: self.url.to_string().into(),
        })];
        if let Some(alias) = alias {
            arguments.push(Node::new(Argument {
                name: LINK_DIRECTIVE_AS_ARGUMENT_NAME,
                value: alias.to_string().into(),
            }));
        }

        let schema_definition = SchemaDefinitionPosition.get(schema.schema());
        SchemaDefinitionPosition.insert_directive_at(
            schema,
            Component {
                origin: schema_definition.origin_to_use(),
                node: Node::new(Directive { name, arguments }),
            },
            0, // @link to link spec should be first
        )?;
        Ok(())
    }

    pub(crate) fn extract_alias_and_imports_on_missing_link_directive_definition(
        application: &Node<Directive>,
    ) -> Result<(Option<Name>, Vec<Arc<Import>>), FederationError> {
        // PORT_NOTE: This is really logic encapsulated from onMissingDirectiveDefinition() in the
        // JS codebase's FederationBlueprint, but moved here since it's all link-specific. The logic
        // itself has a lot of problems, but we're porting it as-is for now, and we'll address the
        // problems with it in a later version bump.
        let url =
            directive_optional_string_argument(application, &LINK_DIRECTIVE_URL_ARGUMENT_NAME)?;
        if let Some(url) = url
            && url.starts_with(&LinkSpecDefinition::latest().url.identity.to_string())
        {
            let alias =
                directive_optional_string_argument(application, &LINK_DIRECTIVE_AS_ARGUMENT_NAME)?
                    .map(Name::new)
                    .transpose()?;
            let imports = directive_optional_list_argument(
                application,
                &LINK_DIRECTIVE_IMPORT_ARGUMENT_NAME,
            )?
            .into_iter()
            .flatten()
            .map(|value| Ok::<_, FederationError>(Arc::new(Import::from_value(value)?)))
            .process_results(|r| r.collect::<Vec<_>>())?;
            return Ok((alias, imports));
        }
        Ok((None, vec![]))
    }

    pub(crate) fn add_definitions_to_schema(
        &self,
        schema: &mut FederationSchema,
        alias: Option<Name>,
        imports: Vec<Arc<Import>>,
    ) -> Result<(), FederationError> {
        if let Some(metadata) = schema.metadata() {
            let link_spec_def = metadata.link_spec_definition()?;
            if link_spec_def.url.identity == *self.identity() {
                // Already exists with the same version, let it be.
                return Ok(());
            }
            let self_fmt = format!("{}/{}", self.identity(), self.version());
            return Err(SingleFederationError::InvalidLinkDirectiveUsage {
                message: format!(
                    "Cannot add link spec {self_fmt} to the schema, it already has {existing_def}",
                    existing_def = link_spec_def.url
                ),
            }
            .into());
        }

        // The @link spec is special in that it is the one that bootstrap everything, and by the
        // time this method is called, the `schema` may not yet have any `schema.metadata()` set up
        // yet. To have `check_or_add` calls below still work, we pass a mock link object with the
        // proper information.
        let mock_link = Arc::new(Link {
            url: self.url.clone(),
            spec_alias: alias,
            imports,
            purpose: None,
        });
        Ok(())
            .and_try(
                self.type_specs()
                    .into_iter()
                    .try_for_all(|spec| spec.check_or_add(schema, Some(&mock_link))),
            )
            .and_try(
                self.directive_specs()
                    .into_iter()
                    .try_for_all(|spec| spec.check_or_add(schema, Some(&mock_link))),
            )
    }

    pub(crate) fn apply_feature_to_schema(
        &self,
        schema: &mut FederationSchema,
        feature: &dyn SpecDefinition,
        alias: Option<Name>,
        purpose: Option<Purpose>,
        imports: Option<Vec<Import>>,
    ) -> Result<(), FederationError> {
        let mut directive = Directive::new(self.url.identity.name.clone());
        directive.arguments.push(Node::new(Argument {
            name: self.url_arg_name(),
            value: Node::new(feature.to_string().into()),
        }));
        if let Some(alias) = alias {
            directive.arguments.push(Node::new(Argument {
                name: LINK_DIRECTIVE_AS_ARGUMENT_NAME,
                value: Node::new(alias.to_string().into()),
            }));
        }
        if let Some(purpose) = &purpose {
            if self.supports_purpose() {
                directive.arguments.push(Node::new(Argument {
                    name: LINK_DIRECTIVE_FOR_ARGUMENT_NAME,
                    value: Node::new(Value::Enum(purpose.into())),
                }));
            } else {
                return Err(SingleFederationError::InvalidLinkDirectiveUsage {
                    message: format!(
                        "Cannot apply feature {} with purpose since the schema's @core/@link version does not support it.", feature.to_string()
                    ),
                }.into());
            }
        }
        if let Some(imports) = imports
            && !imports.is_empty()
        {
            if self.supports_import() {
                directive.arguments.push(Node::new(Argument {
                    name: LINK_DIRECTIVE_IMPORT_ARGUMENT_NAME,
                    value: Node::new(Value::List(
                        imports.into_iter().map(|i| Node::new(i.into())).collect(),
                    )),
                }))
            } else {
                return Err(SingleFederationError::InvalidLinkDirectiveUsage {
                        message: format!(
                            "Cannot apply feature {} with imports since the schema's @core/@link version does not support it.",
                            feature.to_string()
                        ),
                    }.into());
            }
        }

        SchemaDefinitionPosition.insert_directive(schema, Component::new(directive))?;
        feature.add_elements_to_schema(schema)?;

        Ok(())
    }

    #[allow(unused)]
    pub(crate) fn fed1_latest() -> &'static Self {
        // Note: The `unwrap()` calls won't panic, since `CORE_VERSIONS` will always have at
        // least one version.
        let latest_version = CORE_VERSIONS.versions().last().unwrap();
        CORE_VERSIONS.find(latest_version).unwrap()
    }

    /// PORT_NOTE: This is a port of the `linkSpec`, which is defined as `LINK_VERSIONS.latest()`.
    pub(crate) fn latest() -> &'static Self {
        // Note: The `unwrap()` calls won't panic, since `LINK_VERSIONS` will always have at
        // least one version.
        let latest_version = LINK_VERSIONS.versions().last().unwrap();
        LINK_VERSIONS.find(latest_version).unwrap()
    }
}

impl SpecDefinition for LinkSpecDefinition {
    fn url(&self) -> &Url {
        &self.url
    }

    fn directive_specs(&self) -> Vec<Box<dyn TypeAndDirectiveSpecification>> {
        vec![Box::new(DirectiveSpecification::new(
            self.url().identity.name.clone(),
            &self.create_definition_argument_specifications(),
            true,
            &[DirectiveLocation::Schema],
            false,
            None,
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

    fn minimum_federation_version(&self) -> &Version {
        &self.minimum_federation_version
    }

    fn add_elements_to_schema(
        &self,
        _schema: &mut FederationSchema,
    ) -> Result<(), FederationError> {
        // Link is special and the @link directive is added in `add_to_schema` above
        Ok(())
    }

    fn purpose(&self) -> Option<Purpose> {
        None
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
            Version { major: 1, minor: 0 },
        ));
        definitions.add(LinkSpecDefinition::new(
            Version { major: 0, minor: 2 },
            Identity::core_identity(),
            Version { major: 2, minor: 0 },
        ));
        definitions
    });
pub(crate) static LINK_VERSIONS: LazyLock<SpecDefinitions<LinkSpecDefinition>> =
    LazyLock::new(|| {
        let mut definitions = SpecDefinitions::new(Identity::link_identity());
        definitions.add(LinkSpecDefinition::new(
            Version { major: 1, minor: 0 },
            Identity::link_identity(),
            Version { major: 2, minor: 0 },
        ));
        definitions
    });
