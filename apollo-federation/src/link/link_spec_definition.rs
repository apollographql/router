use std::sync::Arc;
use std::sync::LazyLock;

use apollo_compiler::Name;
use apollo_compiler::Node;
use apollo_compiler::Schema;
use apollo_compiler::ast::Argument;
use apollo_compiler::ast::Directive;
use apollo_compiler::ast::DirectiveDefinition;
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

pub(crate) const LINK_DIRECTIVE_NAME_IN_SPEC: Name = name!("link");
pub(crate) const LINK_DIRECTIVE_AS_ARGUMENT_NAME: Name = name!("as");
pub(crate) const LINK_DIRECTIVE_URL_ARGUMENT_NAME: Name = name!("url");
pub(crate) const LINK_DIRECTIVE_FOR_ARGUMENT_NAME: Name = name!("for");
pub(crate) const LINK_DIRECTIVE_IMPORT_ARGUMENT_NAME: Name = name!("import");
pub(crate) const LINK_DIRECTIVE_FEATURE_ARGUMENT_NAME: Name = name!("feature"); // Fed 1's `url` argument

pub(crate) const IMPORT_TYPE_NAME_IN_SPEC: Name = name!("Import");
pub(crate) const IMPORT_TYPE_NAME_FIELD_NAME: Name = name!("name");
pub(crate) const IMPORT_TYPE_AS_FIELD_NAME: Name = name!("as");

pub(crate) const PURPOSE_TYPE_NAME_IN_SPEC: Name = name!("Purpose");

impl TryFrom<&Value> for Purpose {
    type Error = FederationError;

    fn try_from(value: &Value) -> Result<Self, Self::Error> {
        let Some(purpose) = value.as_enum() else {
            return Err(SingleFederationError::InvalidLinkDirectiveUsage {
                message: format!(
                    r#"@link(for:) argument `{}` must be an enum value"#,
                    value.serialize().no_indent()
                ),
            }
            .into());
        };
        match purpose.as_str() {
            "SECURITY" => Ok(Purpose::SECURITY),
            "EXECUTION" => Ok(Purpose::EXECUTION),
            _ => Err(SingleFederationError::InvalidLinkDirectiveUsage {
                message: format!(
                    r#"@link(for:) argument `{}` is not a known enum value"#,
                    value.serialize().no_indent()
                ),
            }
            .into()),
        }
    }
}

impl From<&Purpose> for Value {
    fn from(value: &Purpose) -> Self {
        match value {
            Purpose::SECURITY => Value::Enum(name!("SECURITY")),
            Purpose::EXECUTION => Value::Enum(name!("EXECUTION")),
        }
    }
}

impl TryFrom<&Value> for Import {
    type Error = FederationError;

    fn try_from(value: &Value) -> Result<Self, Self::Error> {
        match value {
            Value::String(str) => {
                if let Some(directive_name) = str.strip_prefix('@') {
                    Ok(Import {
                        element: Name::new(directive_name).map_err(|_| SingleFederationError::InvalidLinkDirectiveUsage {
                            message: format!(r#"`{}` in @link(import:) argument is not a valid GraphQL name"#, value.serialize().no_indent()),
                        })?,
                        is_directive: true,
                        alias: None,
                    })
                } else {
                    Ok(Import {
                        element: Name::new(str).map_err(|_| SingleFederationError::InvalidLinkDirectiveUsage {
                            message: format!(r#"`{}` in @link(import:) argument is not a valid GraphQL name"#, value.serialize().no_indent()),
                        })?,
                        is_directive: false,
                        alias: None,
                    })
                }
            }
            Value::Object(fields) => {
                let mut name: Option<&str> = None;
                let mut alias: Option<&str> = None;
                for (k, v) in fields {
                    match k.as_str() {
                        "name" => {
                            name = Some(v.as_str().ok_or_else(|| SingleFederationError::InvalidLinkDirectiveUsage {
                                message: format!(r#"For `{}` in @link(import:) argument, value for field "name" must be a string"#, value.serialize().no_indent()),
                            })?)
                        },
                        "as" => {
                            alias = Some(v.as_str().ok_or_else(|| SingleFederationError::InvalidLinkDirectiveUsage {
                                message: format!(r#"For `{}` in @link(import:) argument, value for field "as" must be a string"#, value.serialize().no_indent()),
                            })?)
                        },
                        _ => {
                            return Err(SingleFederationError::InvalidLinkDirectiveUsage {
                                message: format!(r#"For `{}` in @link(import:) argument, field "{k}" is not a known field"#, value.serialize().no_indent()),
                            }.into());
                        }
                    }
                }
                let Some(element) = name else {
                    return Err(SingleFederationError::InvalidLinkDirectiveUsage {
                        message: format!(r#"For `{}` in @link(import:) argument, missing required field "name""#, value.serialize().no_indent()),
                    }.into());
                };
                if let Some(directive_name) = element.strip_prefix('@') {
                    if let Some(alias_str) = alias.as_ref() {
                        let Some(alias_str) = alias_str.strip_prefix('@') else {
                            return Err(SingleFederationError::InvalidLinkDirectiveUsage {
                                message: format!(r#"For `{}` in @link(import:) argument, value for field "as" must start with "@" since value for field "name" does ("@" indicates a directive import)"#, value.serialize().no_indent()),
                            }.into());
                        };
                        alias = Some(alias_str);
                    }
                    Ok(Import {
                        element: Name::new(directive_name).map_err(|_| SingleFederationError::InvalidLinkDirectiveUsage {
                            message: format!(r#"For `{}` in @link(import:) argument, value for field "name" is not a valid GraphQL name"#, value.serialize().no_indent()),
                        })?,
                        is_directive: true,
                        alias: alias.map(|alias| Name::new(alias).map_err(|_| SingleFederationError::InvalidLinkDirectiveUsage {
                            message: format!(r#"For `{}` in @link(import:) argument, value for field "as" is not a valid GraphQL name"#, value.serialize().no_indent()),
                        })).transpose()?,
                    })
                } else {
                    if let Some(alias) = &alias
                        && alias.starts_with('@')
                    {
                        return Err(SingleFederationError::InvalidLinkDirectiveUsage {
                            message: format!(r#"For `{}` in @link(import:) argument, value for field "as" must not start with "@" since value for field "name" does not ("@" indicates a directive import)"#, value.serialize().no_indent()),
                        }.into());
                    }
                    Ok(Import {
                        element: Name::new(element).map_err(|_| SingleFederationError::InvalidLinkDirectiveUsage {
                            message: format!(r#"For `{}` in @link(import:) argument, value for field "name" is not a valid GraphQL name"#, value.serialize().no_indent()),
                        })?,
                        is_directive: false,
                        alias: alias.map(|alias| Name::new(alias).map_err(|_| SingleFederationError::InvalidLinkDirectiveUsage {
                            message: format!(r#"For `{}` in @link(import:) argument, value for field "as" is not a valid GraphQL name"#, value.serialize().no_indent()),
                        })).transpose()?,
                    })
                }
            }
            _ => Err(SingleFederationError::InvalidLinkDirectiveUsage {
                message: format!(r#"`{}` in @link(import:) argument must either be a string `"<importedElement>"` or an object `{{ name: "<importedElement>", as: "<alias>" }}`"#, value.serialize().no_indent()),
            }.into()),
        }
    }
}

impl From<&Import> for Value {
    fn from(value: &Import) -> Self {
        let element_string = value.element_name_in_spec().to_string();

        if value.alias.is_some() {
            let alias_string = value.element_name_in_schema().to_string();
            Value::Object(vec![
                (
                    IMPORT_TYPE_NAME_FIELD_NAME,
                    Node::new(Value::String(element_string)),
                ),
                (
                    IMPORT_TYPE_AS_FIELD_NAME,
                    Node::new(Value::String(alias_string)),
                ),
            ])
        } else {
            Value::String(element_string)
        }
    }
}

#[derive(Debug)]
pub(crate) struct LinkSpecDefinition {
    url: Url,
    name: Name,
    minimum_federation_version: Version,
}

impl LinkSpecDefinition {
    pub(crate) fn new(
        version: Version,
        minimum_federation_version: Version,
        is_link: bool,
    ) -> Self {
        Self {
            url: Url {
                identity: if is_link {
                    Identity::link_identity()
                } else {
                    Identity::core_identity()
                },
                version,
            },
            name: if is_link {
                Identity::LINK_NAME
            } else {
                Identity::CORE_NAME
            },
            minimum_federation_version,
        }
    }

    pub(crate) fn name(&self) -> &Name {
        &self.name
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
                                "Type {PURPOSE_TYPE_NAME_IN_SPEC} shouldn't be added without being attached to a @link spec"
                            )
                        };
                        Ok(Type::Named(link.type_name_in_schema(&PURPOSE_TYPE_NAME_IN_SPEC)))
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
                                "Type {IMPORT_TYPE_NAME_IN_SPEC} shouldn't be added without being attached to a @link spec"
                            )
                        };
                        Ok(Type::List(Box::new(Type::Named(
                            link.type_name_in_schema(&IMPORT_TYPE_NAME_IN_SPEC),
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
        if self.name == Identity::CORE_NAME {
            LINK_DIRECTIVE_FEATURE_ARGUMENT_NAME
        } else {
            LINK_DIRECTIVE_URL_ARGUMENT_NAME
        }
    }

    pub(crate) fn link_from_directive(
        &self,
        directive: &Node<Directive>,
        schema: &Schema,
    ) -> Result<Link, FederationError> {
        let url = if let Some(value) = directive.specified_argument_by_name(&self.url_arg_name()) {
            value
        } else {
            return Err(SingleFederationError::InvalidLinkDirectiveUsage {
                message: format!(
                    r#"`{}` missing required argument "{}""#,
                    directive.serialize().no_indent(),
                    self.url_arg_name(),
                ),
            }
            .into());
        };

        let url = url
            .as_str()
            .ok_or_else(|| SingleFederationError::InvalidLinkDirectiveUsage {
                message: format!(
                    r#"@{}({}:) argument `{}` must be a string"#,
                    self.name,
                    self.url_arg_name(),
                    url.serialize().no_indent()
                ),
            })?;
        let url: Url = url.parse::<Url>()?;

        let spec_alias = directive
            .specified_argument_by_name("as")
            .take_if(|arg| !arg.is_null())
            .map(|arg| {
                arg.as_str()
                    .ok_or_else(|| SingleFederationError::InvalidLinkDirectiveUsage {
                        message: format!(
                            r#"@{}(as:) argument `{}` must be a string or null"#,
                            self.name,
                            arg.serialize().no_indent()
                        ),
                    })
            })
            .transpose()?
            .map(Name::new)
            .transpose()?;

        let purpose = if self.supports_purpose() {
            directive
                .specified_argument_by_name("for")
                .take_if(|arg| !arg.is_null())
                .map(|arg| Purpose::try_from(arg.as_ref()))
                .transpose()?
        } else {
            None
        };

        let imports = if self.supports_import() {
            directive
                .specified_argument_by_name("import")
                .take_if(|arg| !arg.is_null())
                .map(|arg| {
                    // Note that list input coercion rules mandate that when the value is not
                    // a list and not null then it is wrapped into a list of size one.
                    if let Value::List(value) = arg.as_ref() {
                        value
                    } else {
                        std::slice::from_ref(arg)
                    }
                })
                .unwrap_or(&[])
                .iter()
                .map(|value| Ok(Arc::new(Import::try_from(value.as_ref())?)))
                .collect::<Result<Vec<Arc<Import>>, FederationError>>()?
        } else {
            Default::default()
        };

        Ok(Link {
            url,
            spec_alias,
            imports,
            purpose,
            line_column_range: directive.line_column_range(&schema.sources),
        })
    }

    pub(crate) fn directive_from_link(&self, link: &Link) -> Directive {
        let mut arguments = Vec::new();
        arguments.push(Node::new(Argument {
            name: self.url_arg_name().clone(),
            value: Node::new(Value::String(link.url.to_string())),
        }));
        if let Some(spec_alias) = &link.spec_alias {
            arguments.push(Node::new(Argument {
                name: LINK_DIRECTIVE_AS_ARGUMENT_NAME.clone(),
                value: Node::new(Value::String(spec_alias.to_string())),
            }));
        }
        if self.supports_import() && !link.imports.is_empty() {
            arguments.push(Node::new(Argument {
                name: LINK_DIRECTIVE_IMPORT_ARGUMENT_NAME.clone(),
                value: Node::new(Value::List(
                    link.imports
                        .iter()
                        .map(|import| Node::new(Value::from(import.as_ref())))
                        .collect(),
                )),
            }));
        }
        if self.supports_purpose()
            && let Some(purpose) = &link.purpose
        {
            arguments.push(Node::new(Argument {
                name: LINK_DIRECTIVE_FOR_ARGUMENT_NAME.clone(),
                value: Node::new(Value::from(purpose)),
            }));
        }
        Directive {
            name: self.name.clone(),
            arguments,
        }
    }

    /// Returns whether a given directive is the @link or @core directive that imports the @link or
    /// @core spec.
    pub(super) fn is_bootstrap_directive(schema: &Schema, directive: &Directive) -> bool {
        let Some(definition) = schema.directive_definitions.get(&directive.name) else {
            return false;
        };
        if Self::is_link_directive_definition(definition) {
            if let Some(url) = directive
                .specified_argument_by_name("url")
                .and_then(|value| value.as_str())
            {
                let url = url.parse::<Url>();
                let default_link_name = LINK_DIRECTIVE_NAME_IN_SPEC;
                let expected_name = directive
                    .specified_argument_by_name("as")
                    .and_then(|value| value.as_str())
                    .unwrap_or(default_link_name.as_str());
                return url.is_ok_and(|url| {
                    url.identity == Identity::link_identity() && directive.name == expected_name
                });
            }
        } else if Self::is_core_directive_definition(definition) {
            // XXX(@goto-bus-stop): @core compatibility is primarily to support old tests--should be
            // removed when those are updated.
            if let Some(url) = directive
                .specified_argument_by_name("feature")
                .and_then(|value| value.as_str())
            {
                let url = url.parse::<Url>();
                let expected_name = directive
                    .specified_argument_by_name("as")
                    .and_then(|value| value.as_str())
                    .unwrap_or("core");
                return url.is_ok_and(|url| {
                    url.identity == Identity::core_identity() && directive.name == expected_name
                });
            }
        };
        false
    }

    /// Returns true if the given definition matches the @link definition.
    ///
    /// Either of these definitions are accepted:
    /// ```graphql
    /// directive @_ANY_NAME_(url: String!, as: String) repeatable on SCHEMA
    /// directive @_ANY_NAME_(url: String, as: String) repeatable on SCHEMA
    /// directive @_ANY_NAME_(url: String!) repeatable on SCHEMA
    /// directive @_ANY_NAME_(url: String) repeatable on SCHEMA
    /// ```
    fn is_link_directive_definition(definition: &DirectiveDefinition) -> bool {
        definition.repeatable
            && definition.locations == [DirectiveLocation::Schema]
            && definition.argument_by_name("url").is_some_and(|argument| {
                // The "true" type of `url` in the @link spec is actually `String` (nullable), and this
                // for future-proofing reasons (the idea was that we may introduce later other
                // ways to identify specs that are not urls). But we allow the definition to
                // have a non-nullable type both for convenience and because some early
                // federation previews actually generated that.
                *argument.ty == ty!(String!) || *argument.ty == ty!(String)
            })
            && definition
                .argument_by_name("as")
                .is_none_or(|argument| *argument.ty == ty!(String))
    }

    /// Returns true if the given definition matches the @core definition.
    ///
    /// Either of these definitions are accepted:
    /// ```graphql
    /// directive @_ANY_NAME_(feature: String!, as: String) repeatable on SCHEMA
    /// directive @_ANY_NAME_(feature: String, as: String) repeatable on SCHEMA
    /// directive @_ANY_NAME_(feature: String!) repeatable on SCHEMA
    /// directive @_ANY_NAME_(feature: String) repeatable on SCHEMA
    /// ```
    fn is_core_directive_definition(definition: &DirectiveDefinition) -> bool {
        // XXX(@goto-bus-stop): @core compatibility is primarily to support old tests--should be
        // removed when those are updated.
        definition.repeatable
            && definition.locations == [DirectiveLocation::Schema]
            && definition
                .argument_by_name("feature")
                .is_some_and(|argument| {
                    // The "true" type of `url` in the @core spec is actually `String` (nullable), and this
                    // for future-proofing reasons (the idea was that we may introduce later other
                    // ways to identify specs that are not urls). But we allow the definition to
                    // have a non-nullable type both for convenience and because some early
                    // federation previews actually generated that.
                    *argument.ty == ty!(String!) || *argument.ty == ty!(String)
                })
            && definition
                .argument_by_name("as")
                .is_none_or(|argument| *argument.ty == ty!(String))
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

        let name = alias.as_ref().unwrap_or(&self.name).clone();
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
        application: &Component<Directive>,
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
            .map(|value| Ok::<_, FederationError>(Arc::new(Import::try_from(value.as_ref())?)))
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
            let link_spec_def = metadata.link_spec_definition();
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
            line_column_range: None,
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
        mut on_apply_error: impl FnMut(FederationError) -> Result<(), FederationError>,
    ) -> Result<(), FederationError> {
        let Some(metadata) = schema.metadata() else {
            bail!("Schema unexpectedly not a link schema (add @link first)");
        };
        if metadata.link_itself().url != self.url {
            bail!(
                "Cannot use this version of @link ({}), the schema uses version {}",
                self.url,
                metadata.link_itself().url,
            );
        }
        let mut directive = Directive::new(metadata.link_itself().spec_name_in_schema());
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
                    value: Node::new(purpose.into()),
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
                        imports
                            .into_iter()
                            .map(|i| Node::new((&i).into()))
                            .collect(),
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

        if let Err(error) =
            SchemaDefinitionPosition.insert_directive(schema, Component::new(directive))
        {
            on_apply_error(error)?;
        };
        feature.add_elements_to_schema(schema)?;

        Ok(())
    }

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
            self.name().clone(),
            &self.create_definition_argument_specifications(),
            true,
            &[DirectiveLocation::Schema],
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
        name: PURPOSE_TYPE_NAME_IN_SPEC,
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
        name: IMPORT_TYPE_NAME_IN_SPEC,
    }
}

pub(crate) static CORE_VERSIONS: LazyLock<SpecDefinitions<LinkSpecDefinition>> =
    LazyLock::new(|| {
        let mut definitions = SpecDefinitions::new(Identity::core_identity());
        definitions.add(LinkSpecDefinition::new(
            Version { major: 0, minor: 1 },
            Version { major: 1, minor: 0 },
            false,
        ));
        definitions.add(LinkSpecDefinition::new(
            Version { major: 0, minor: 2 },
            Version { major: 2, minor: 0 },
            false,
        ));
        definitions
    });
pub(crate) static LINK_VERSIONS: LazyLock<SpecDefinitions<LinkSpecDefinition>> =
    LazyLock::new(|| {
        let mut definitions = SpecDefinitions::new(Identity::link_identity());
        definitions.add(LinkSpecDefinition::new(
            Version { major: 1, minor: 0 },
            Version { major: 2, minor: 0 },
            true,
        ));
        definitions
    });
