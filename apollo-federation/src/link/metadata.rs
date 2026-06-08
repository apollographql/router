use std::borrow::Cow;
use std::collections::HashMap;
use std::sync::Arc;

use apollo_compiler::Name;
use apollo_compiler::Schema;
use apollo_compiler::collections::HashSet;
use apollo_compiler::collections::IndexMap;

use crate::error::FederationError;
use crate::error::SingleFederationError;
use crate::link::Import;
use crate::link::Link;
use crate::link::federation_spec_definition::FED_1;
use crate::link::federation_spec_definition::FEDERATION_VERSIONS;
use crate::link::federation_spec_definition::FederationSpecDefinition;
use crate::link::link_spec_definition::CORE_VERSIONS;
use crate::link::link_spec_definition::LINK_VERSIONS;
use crate::link::link_spec_definition::LinkSpecDefinition;
use crate::link::spec::Identity;
use crate::link::spec::Url;
use crate::link::spec::Version;
use crate::link::spec_definition::SpecDefinition;

#[derive(Clone, Debug)]
pub struct LinkedElement {
    pub link: Arc<Link>,
    pub import: Option<Arc<Import>>,
    pub name: Name,
    pub name_in_spec: Name,
}

#[derive(Clone, Eq, PartialEq, Debug)]
pub struct LinksMetadata {
    pub(crate) links: Vec<Arc<Link>>,
    pub(crate) by_identity: IndexMap<Identity, Arc<Link>>,
    pub(crate) by_name_in_schema: IndexMap<Name, Arc<Link>>,
    pub(crate) types_by_imported_name: IndexMap<Name, (Arc<Link>, Arc<Import>)>,
    pub(crate) directives_by_imported_name: IndexMap<Name, (Arc<Link>, Arc<Import>)>,
}

impl LinksMetadata {
    /// Create @link metadata from a schema.
    pub fn from_schema(schema: &Schema) -> Result<Option<LinksMetadata>, FederationError> {
        // This finds "bootstrap" uses of @link / @core regardless of order. By spec,
        // the bootstrap directive application must be the first application of @link / @core, but
        // this was not enforced by the JS implementation, so we match it for backward compatibility.
        let mut bootstrap_directives = schema
            .schema_definition
            .directives
            .iter()
            .filter(|d| LinkSpecDefinition::is_bootstrap_directive(schema, d));
        let Some(bootstrap_directive) = bootstrap_directives.next() else {
            return Ok(None);
        };
        // There must be exactly one bootstrap directive.
        if let Some(extraneous_directive) = bootstrap_directives.next() {
            return Err(SingleFederationError::InvalidLinkDirectiveUsage {
                message: format!(
                    "Invalid use of @link in schema: the @link for the link specification itself (\"{}\") is applied multiple times",
                    extraneous_directive
                        .specified_argument_by_name("url")
                        // XXX(@goto-bus-stop): @core compatibility is primarily to support old tests in other projects,
                        // and should be removed when those are updated.
                        .or(extraneous_directive.specified_argument_by_name("feature"))
                        .and_then(|value| value.as_str().map(Cow::Borrowed))
                        .unwrap_or_else(|| Cow::Owned(Identity::link_identity().to_string()))
                )
            }.into());
        }

        // At this point, we know this schema uses "our" @link. So we now "just" want to validate
        // all of the @link usages (starting with the bootstrapping one) and extract their metadata.
        let link_name_in_schema = &bootstrap_directive.name;
        let mut links = Vec::new();
        let mut by_identity = IndexMap::default();
        let mut by_name_in_schema = IndexMap::default();
        let mut types_by_imported_name = IndexMap::default();
        let mut directives_by_imported_name = IndexMap::default();
        let link_applications = schema
            .schema_definition
            .directives
            .iter()
            .filter(|d| d.name == *link_name_in_schema);
        for application in link_applications {
            let link = Arc::new(Link::from_directive_application(application, schema)?);
            links.push(Arc::clone(&link));
            if by_identity
                .insert(link.url.identity.clone(), Arc::clone(&link))
                .is_some()
            {
                // XXX(Sylvain): We may want to loosen this limitation at some point. Including the same feature for 2 different major versions should be ok.
                return Err(SingleFederationError::InvalidLinkDirectiveUsage {
                    message: format!(
                        "Invalid use of @link in schema: duplicate @link inclusion of specification \"{}\"",
                        link.url.identity
                    )
                }.into());
            }
            let name_in_schema = link.spec_name_in_schema();
            if let Some(other) = by_name_in_schema.insert(name_in_schema.clone(), Arc::clone(&link))
            {
                return Err(SingleFederationError::InvalidLinkDirectiveUsage {
                    message: format!(
                        "Invalid use of @link in schema: name conflict: {} and {} are imported under the same name (consider using the `@link(as:)` argument to disambiguate)",
                        other.url, link.url,
                    )
                }.into());
            }
        }

        // We do a 2nd pass to collect and validate all the imports (it's a separate path so we
        // know all the names of the spec linked in the schema).
        for link in &links {
            if link.url.identity == Identity::federation_identity() {
                Self::validate_federation_imports(link)?;
            }

            for import in &link.imports {
                let imported_name = import.imported_name();
                let element_map = if import.is_directive {
                    // The name of each spec (in the schema) acts as an implicit import for a
                    // directive of the same name. So one cannot import a directive with the
                    // same name than a linked spec (unless that implicit import is explicitly
                    // renamed).
                    if let Some(other) = by_name_in_schema.get(imported_name)
                        && !Arc::ptr_eq(other, link)
                        && !other.renames(imported_name)
                    {
                        return Err(SingleFederationError::InvalidLinkDirectiveUsage {
                            message: format!(
                                "Invalid use of @link in schema: import for '{}' of {} conflicts with spec {}",
                                import.imported_display_name(),
                                link.url,
                                other.url
                            )
                        }.into());
                    }
                    &mut directives_by_imported_name
                } else {
                    &mut types_by_imported_name
                };
                // Conflicting imports are not allowed, except for duplicate imports within the same
                // @link application. Although it's odd, JS composition allows it.
                if let Some((other_link, _)) = element_map.insert(
                    imported_name.clone(),
                    (Arc::clone(link), Arc::clone(import)),
                ) && !Arc::ptr_eq(&other_link, link)
                {
                    return Err(SingleFederationError::InvalidLinkDirectiveUsage {
                        message: format!(
                            "Invalid use of @link in schema: name conflict: both {} and {} import {}",
                            link.url,
                            other_link.url,
                            import.imported_display_name()
                        )
                    }.into());
                }
            }
        }

        Ok(Some(LinksMetadata {
            links,
            by_identity,
            by_name_in_schema,
            types_by_imported_name,
            directives_by_imported_name,
        }))
    }

    fn find_federation_spec_for_version<'a>(
        version: &Version,
    ) -> Option<&'a FederationSpecDefinition> {
        if *version == (Version { major: 1, minor: 0 }) {
            Some(&FED_1)
        } else {
            FEDERATION_VERSIONS.find(version)
        }
    }

    fn validate_federation_imports(link: &Link) -> Result<(), FederationError> {
        let Some(federation_spec) = Self::find_federation_spec_for_version(&link.url.version)
        else {
            return Err(SingleFederationError::InvalidLinkDirectiveUsage {
                message: format!(
                    "Unknown import: Unexpected federation version: {}",
                    link.url.version
                ),
            }
            .into());
        };
        let federation_directives: HashSet<_> = federation_spec
            .directive_specs()
            .iter()
            .map(|spec| spec.name().clone())
            .collect();
        let federation_types: HashSet<_> = federation_spec
            .type_specs()
            .iter()
            .map(|spec| spec.name().clone())
            .collect();

        for imp in &link.imports {
            if imp.is_directive && !federation_directives.contains(&imp.element) {
                return Err(SingleFederationError::InvalidLinkDirectiveUsage {
                    message: format!(
                        "Unknown import: Cannot import unknown federation directive \"@{}\".",
                        imp.element,
                    ),
                }
                .into());
            } else if !imp.is_directive && !federation_types.contains(&imp.element) {
                return Err(SingleFederationError::InvalidLinkDirectiveUsage {
                    message: format!(
                        "Unknown import: Cannot import unknown federation element \"{}\".",
                        imp.element,
                    ),
                }
                .into());
            }
        }
        Ok(())
    }

    // PORT_NOTE: Call this as a replacement for `CoreFeatures.coreItself` from JS.
    pub(crate) fn link_spec_definition(
        &self,
    ) -> Result<&'static LinkSpecDefinition, FederationError> {
        if let Some(link_link) = self.for_identity(&Identity::link_identity()) {
            LINK_VERSIONS.find(&link_link.url.version).ok_or_else(|| {
                SingleFederationError::Internal {
                    message: format!("Unexpected link spec version {}", link_link.url.version),
                }
                .into()
            })
        } else if let Some(core_link) = self.for_identity(&Identity::core_identity()) {
            CORE_VERSIONS.find(&core_link.url.version).ok_or_else(|| {
                SingleFederationError::Internal {
                    message: format!("Unexpected core spec version {}", core_link.url.version),
                }
                .into()
            })
        } else {
            Err(SingleFederationError::Internal {
                message: "Unexpectedly could not find core/link spec".to_owned(),
            }
            .into())
        }
    }

    pub fn all_links(&self) -> &[Arc<Link>] {
        self.links.as_ref()
    }

    pub fn for_identity(&self, identity: &Identity) -> Option<Arc<Link>> {
        self.by_identity.get(identity).cloned()
    }

    pub fn source_link_of_type(&self, type_name: &Name) -> Option<LinkedElement> {
        // For types, it's either an imported name or it must be fully qualified
        if let Some((link, import)) = self.types_by_imported_name.get(type_name) {
            return Some(LinkedElement {
                link: Arc::clone(link),
                import: Some(Arc::clone(import)),
                name: type_name.clone(),
                name_in_spec: import.element.clone(),
            });
        }

        type_name
            .split_once("__")
            .and_then(|(spec_name, name_in_spec)| {
                let Ok(name_in_spec) = Name::new(name_in_spec) else {
                    return None;
                };
                self.by_name_in_schema
                    .get(spec_name)
                    .map(|link| LinkedElement {
                        link: Arc::clone(link),
                        import: None,
                        name: type_name.clone(),
                        name_in_spec,
                    })
            })
    }

    pub fn source_link_of_directive(&self, directive_name: &Name) -> Option<LinkedElement> {
        // For directives, it can be either:
        //   1. be an imported name,
        //   2. be the "imported" name of a linked spec (special case of a directive named like the
        //      spec),
        //   3. or it must be fully qualified.
        if let Some((link, import)) = self.directives_by_imported_name.get(directive_name) {
            return Some(LinkedElement {
                link: Arc::clone(link),
                import: Some(Arc::clone(import)),
                name: directive_name.clone(),
                name_in_spec: import.element.clone(),
            });
        }

        if let Some(link) = self.by_name_in_schema.get(directive_name) {
            return Some(LinkedElement {
                link: Arc::clone(link),
                import: None,
                name: directive_name.clone(),
                name_in_spec: link.url.identity.name.clone(),
            });
        }

        directive_name
            .split_once("__")
            .and_then(|(spec_name, name_in_spec)| {
                let Ok(name_in_spec) = Name::new(name_in_spec) else {
                    return None;
                };
                self.by_name_in_schema
                    .get(spec_name)
                    .map(|link| LinkedElement {
                        link: Arc::clone(link),
                        import: None,
                        name: directive_name.clone(),
                        name_in_spec,
                    })
            })
    }

    pub(crate) fn import_to_feature_url_map(&self) -> HashMap<String, Url> {
        let directive_entries = self
            .directives_by_imported_name
            .iter()
            .map(|(name, (link, _))| (name.to_string(), link.url.clone()));
        let type_entries = self
            .types_by_imported_name
            .iter()
            .map(|(name, (link, _))| (name.to_string(), link.url.clone()));

        directive_entries.chain(type_entries).collect()
    }
}

#[cfg(test)]
mod tests {
    use apollo_compiler::name;

    use super::*;
    use crate::link::Import;
    use crate::link::Purpose;
    use crate::link::spec::Version;

    #[test]
    fn explicit_root_directive_import() -> Result<(), FederationError> {
        let schema = r#"
          extend schema
            @link(url: "https://specs.apollo.dev/link/v1.0", import: ["Import"])
            @link(url: "https://specs.apollo.dev/inaccessible/v0.2", import: ["@inaccessible"])

          type Query { x: Int }

          enum link__Purpose {
            SECURITY
            EXECUTION
          }

          scalar Import

          directive @link(url: String, as: String, import: [Import], for: link__Purpose) repeatable on SCHEMA
        "#;

        let schema = Schema::parse(schema, "root_directive.graphqls").unwrap();

        let meta = LinksMetadata::from_schema(&schema)?;
        let meta = meta.expect("should have metadata");

        assert!(
            meta.source_link_of_directive(&name!("inaccessible"))
                .is_some()
        );

        Ok(())
    }

    #[test]
    fn renamed_link_directive() -> Result<(), FederationError> {
        let schema = r#"
          extend schema
            @lonk(url: "https://specs.apollo.dev/link/v1.0", as: "lonk")
            @lonk(url: "https://specs.apollo.dev/inaccessible/v0.2")

          type Query { x: Int }

          enum lonk__Purpose {
            SECURITY
            EXECUTION
          }

          scalar lonk__Import

          directive @lonk(url: String!, as: String, import: [lonk__Import], for: lonk__Purpose) repeatable on SCHEMA
        "#;

        let schema = Schema::parse(schema, "lonk.graphqls").unwrap();

        let meta = LinksMetadata::from_schema(&schema)?.expect("should have metadata");
        assert!(
            meta.source_link_of_directive(&name!("inaccessible"))
                .is_some()
        );

        Ok(())
    }

    #[test]
    fn renamed_core_directive() -> Result<(), FederationError> {
        let schema = r#"
          extend schema
            @care(feature: "https://specs.apollo.dev/core/v0.2", as: "care")
            @care(feature: "https://specs.apollo.dev/join/v0.2", for: EXECUTION)

          directive @care(feature: String!, as: String, for: core__Purpose) repeatable on SCHEMA
          directive @join__field(graph: join__Graph!, requires: join__FieldSet, provides: join__FieldSet, type: String, external: Boolean) repeatable on FIELD_DEFINITION | INPUT_FIELD_DEFINITION
          directive @join__graph(name: String!, url: String!) on ENUM_VALUE
          directive @join__implements(graph: join__Graph!, interface: String!) repeatable on OBJECT | INTERFACE
          directive @join__type(graph: join__Graph!, key: join__FieldSet, extension: Boolean! = false, resolvable: Boolean! = true) repeatable on OBJECT | INTERFACE | UNION | ENUM | INPUT_OBJECT | SCALAR

          type Query { x: Int }

          enum care__Purpose {
            SECURITY
            EXECUTION
          }

          scalar care__Import

          scalar join__FieldSet

          enum join__Graph {
            USERS @join__graph(name: "users", url: "http://localhost:4001")
          }
        "#;

        let schema = Schema::parse(schema, "care.graphqls").unwrap();

        let meta = LinksMetadata::from_schema(&schema)?.expect("should have metadata");
        assert!(
            meta.source_link_of_directive(&name!("join__graph"))
                .is_some()
        );

        Ok(())
    }

    #[test]
    fn url_syntax() -> Result<(), FederationError> {
        let schema = r#"
            extend schema
              @link(url: "https://specs.apollo.dev/link/v1.0")
              @link(url: "https://specs.apollo.dev/join/v0.3", for: EXECUTION)
              @link(url: "https://example.com/my-directive/v1.0", import: ["@myDirective"])

          type Query { x: Int }

            directive @myDirective on FIELD_DEFINITION | ARGUMENT_DEFINITION | INPUT_FIELD_DEFINITION

            directive @join__enumValue(graph: join__Graph!) repeatable on ENUM_VALUE

            directive @join__field(graph: join__Graph, requires: join__FieldSet, provides: join__FieldSet, type: String, external: Boolean, override: String, usedOverridden: Boolean) repeatable on FIELD_DEFINITION | INPUT_FIELD_DEFINITION

            directive @join__graph(name: String!, url: String!) on ENUM_VALUE

            directive @join__implements(graph: join__Graph!, interface: String!) repeatable on OBJECT | INTERFACE

            directive @join__type(graph: join__Graph!, key: join__FieldSet, extension: Boolean! = false, resolvable: Boolean! = true, isInterfaceObject: Boolean! = false) repeatable on OBJECT | INTERFACE | UNION | ENUM | INPUT_OBJECT | SCALAR

            directive @join__unionMember(graph: join__Graph!, member: String!) repeatable on UNION

            directive @link(url: String, as: String, for: link__Purpose, import: [link__Import]) repeatable on SCHEMA
        "#;

        let schema = Schema::parse(schema, "url_dash.graphqls").unwrap();

        let meta = LinksMetadata::from_schema(&schema)?;
        let meta = meta.expect("should have metadata");

        assert!(
            meta.source_link_of_directive(&name!("myDirective"))
                .is_some()
        );

        Ok(())
    }

    #[test]
    fn computes_link_metadata() {
        let schema = r#"
          extend schema
            @link(url: "https://specs.apollo.dev/link/v1.0", import: ["Import"])
            @link(url: "https://specs.apollo.dev/federation/v2.3", import: ["@key", { name: "@tag", as: "@myTag" }])
            @link(url: "https://custom.com/someSpec/v0.2", as: "mySpec")
            @link(url: "https://megacorp.com/auth/v1.0", for: SECURITY)

          type Query {
            x: Int
          }

          enum link__Purpose {
            SECURITY
            EXECUTION
          }

          scalar Import

          directive @link(url: String, as: String, import: [Import], for: link__Purpose) repeatable on SCHEMA
        "#;

        let schema = Schema::parse(schema, "testSchema").unwrap();

        let meta = LinksMetadata::from_schema(&schema)
            // TODO: error handling?
            .unwrap()
            .unwrap();
        let names_in_schema = meta
            .all_links()
            .iter()
            .map(|l| l.spec_name_in_schema())
            .collect::<Vec<_>>();
        assert_eq!(names_in_schema.len(), 4);
        assert_eq!(names_in_schema[0], "link");
        assert_eq!(names_in_schema[1], "federation");
        assert_eq!(names_in_schema[2], "mySpec");
        assert_eq!(names_in_schema[3], "auth");

        let link_spec = meta.for_identity(&Identity::link_identity()).unwrap();
        assert_eq!(
            link_spec.imports.first().unwrap().as_ref(),
            &Import {
                element: name!("Import"),
                is_directive: false,
                alias: None
            }
        );

        let fed_spec = meta.for_identity(&Identity::federation_identity()).unwrap();
        assert_eq!(fed_spec.url.version, Version { major: 2, minor: 3 });
        assert_eq!(fed_spec.purpose, None);

        let imports = &fed_spec.imports;
        assert_eq!(imports.len(), 2);
        assert_eq!(
            imports.first().unwrap().as_ref(),
            &Import {
                element: name!("key"),
                is_directive: true,
                alias: None
            }
        );
        assert_eq!(
            imports.get(1).unwrap().as_ref(),
            &Import {
                element: name!("tag"),
                is_directive: true,
                alias: Some(name!("myTag"))
            }
        );

        let auth_spec = meta
            .for_identity(&Identity {
                domain: "https://megacorp.com".to_string(),
                name: name!("auth"),
            })
            .unwrap();
        assert_eq!(auth_spec.purpose, Some(Purpose::SECURITY));

        let import_source = meta.source_link_of_type(&name!("Import")).unwrap();
        assert_eq!(import_source.link.url.identity.name, "link");
        assert!(!import_source.import.as_ref().unwrap().is_directive);
        assert_eq!(import_source.import.as_ref().unwrap().alias, None);

        // Purpose is not imported, so it should only be accessible in fql form
        assert!(meta.source_link_of_type(&name!("Purpose")).is_none());

        let purpose_source = meta.source_link_of_type(&name!("link__Purpose")).unwrap();
        assert_eq!(purpose_source.link.url.identity.name, "link");
        assert_eq!(purpose_source.import, None);

        let key_source = meta.source_link_of_directive(&name!("key")).unwrap();
        assert_eq!(key_source.link.url.identity.name, "federation");
        assert!(key_source.import.as_ref().unwrap().is_directive);
        assert_eq!(key_source.import.as_ref().unwrap().alias, None);

        // tag is imported under an alias, so "tag" itself should not match
        assert!(meta.source_link_of_directive(&name!("tag")).is_none());

        let tag_source = meta.source_link_of_directive(&name!("myTag")).unwrap();
        assert_eq!(tag_source.link.url.identity.name, "federation");
        assert_eq!(tag_source.import.as_ref().unwrap().element, "tag");
        assert!(tag_source.import.as_ref().unwrap().is_directive);
        assert_eq!(
            tag_source.import.as_ref().unwrap().alias,
            Some(name!("myTag"))
        );
    }

    mod link_import {
        use super::*;

        #[test]
        fn errors_on_malformed_values() {
            let schema = r#"
                extend schema @link(url: "https://specs.apollo.dev/link/v1.0")
                extend schema @link(
                  url: "https://specs.apollo.dev/federation/v2.0",
                  import: [
                    2,
                    { foo: "bar" },
                    { name: "@key", badName: "foo"},
                    { name: 42 },
                    { as: "bar" },
                   ]
                )

                type Query {
                  q: Int
                }

                directive @link(url: String, as: String, import: [Import], for: link__Purpose) repeatable on SCHEMA
            "#;

            let schema = Schema::parse(schema, "testSchema").unwrap();
            let errors = LinksMetadata::from_schema(&schema).expect_err("should error");
            // TODO Multiple errors
            insta::assert_snapshot!(errors, @r###"`2` in @link(import:) argument must either be a string `"<importedElement>"` or an object `{ name: "<importedElement>", as: "<alias>" }`"###);
        }

        #[test]
        fn errors_on_mismatch_between_name_and_alias() {
            let schema = r#"
                extend schema @link(url: "https://specs.apollo.dev/link/v1.0")
                extend schema @link(
                  url: "https://specs.apollo.dev/federation/v2.0",
                  import: [
                    { name: "@key", as: "myKey" },
                    { name: "FieldSet", as: "@fieldSet" },
                  ]
                )

                type Query {
                  q: Int
                }

                directive @link(url: String, as: String, import: [Import], for: link__Purpose) repeatable on SCHEMA
            "#;

            let schema = Schema::parse(schema, "testSchema").unwrap();
            let errors = LinksMetadata::from_schema(&schema).expect_err("should error");
            // TODO Multiple errors
            insta::assert_snapshot!(errors, @r###"For `{name: "@key", as: "myKey"}` in @link(import:) argument, value for field "as" must start with "@" since value for field "name" does ("@" indicates a directive import)"###);
        }

        #[test]
        fn errors_on_importing_unknown_elements_for_known_features() {
            let schema = r#"
                extend schema @link(url: "https://specs.apollo.dev/link/v1.0")
                extend schema @link(
                  url: "https://specs.apollo.dev/federation/v2.0",
                  import: [ "@foo", "key", { name: "@sharable" } ]
                )

                type Query {
                  q: Int
                }

                directive @link(url: String, as: String, import: [Import], for: link__Purpose) repeatable on SCHEMA
            "#;

            let schema = Schema::parse(schema, "testSchema").unwrap();
            let errors = LinksMetadata::from_schema(&schema).expect_err("should error");
            insta::assert_snapshot!(errors, @"Unknown import: Cannot import unknown federation directive \"@foo\".");

            // TODO Support multiple errors, in the meantime we'll just clone the code and run again
            let schema = r#"
                extend schema @link(url: "https://specs.apollo.dev/link/v1.0")
                extend schema @link(
                url: "https://specs.apollo.dev/federation/v2.0",
                import: [ "key", { name: "@sharable" } ]
                )

                type Query {
                q: Int
                }

                directive @link(url: String, as: String, import: [Import], for: link__Purpose) repeatable on SCHEMA
            "#;

            let schema = Schema::parse(schema, "testSchema").unwrap();
            let errors = LinksMetadata::from_schema(&schema).expect_err("should error");
            insta::assert_snapshot!(errors, @"Unknown import: Cannot import unknown federation element \"key\".");

            let schema = r#"
                extend schema @link(url: "https://specs.apollo.dev/link/v1.0")
                extend schema @link(
                url: "https://specs.apollo.dev/federation/v2.0",
                import: [ { name: "@sharable" } ]
                )

                type Query {
                q: Int
                }

                directive @link(url: String, as: String, import: [Import], for: link__Purpose) repeatable on SCHEMA
            "#;

            let schema = Schema::parse(schema, "testSchema").unwrap();
            let errors = LinksMetadata::from_schema(&schema).expect_err("should error");
            insta::assert_snapshot!(errors, @"Unknown import: Cannot import unknown federation directive \"@sharable\".");
        }
    }

    #[test]
    fn allowed_link_directive_definitions() -> Result<(), FederationError> {
        let link_defs = [
            "directive @link(url: String!, as: String) repeatable on SCHEMA",
            "directive @link(url: String, as: String) repeatable on SCHEMA",
            "directive @link(url: String!) repeatable on SCHEMA",
            "directive @link(url: String) repeatable on SCHEMA",
        ];
        let schema_prefix = r#"
          extend schema @link(url: "https://specs.apollo.dev/link/v1.0")
          type Query { x: Int }
        "#;
        for link_def in link_defs {
            let schema_doc = format!("{schema_prefix}\n{link_def}");
            let schema = Schema::parse(&schema_doc, "test.graphql").unwrap();
            let meta = LinksMetadata::from_schema(&schema)?;
            assert!(meta.is_some(), "should have metadata for: {link_def}");
        }
        Ok(())
    }
}
