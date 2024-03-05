use std::{collections::HashMap, sync::Arc};

use apollo_compiler::ast::{Directive, DirectiveLocation, Type};
use apollo_compiler::Schema;

use crate::link::{
    spec::{Identity, Url},
    {Link, LinkError, LinksMetadata, DEFAULT_LINK_NAME},
};

pub fn links_metadata(schema: &Schema) -> Result<Option<LinksMetadata>, LinkError> {
    let mut bootstrap_directives = schema
        .schema_definition
        .directives
        .iter()
        .filter(|d| parse_link_if_bootstrap_directive(schema, d));
    let bootstrap_directive = bootstrap_directives.next();
    if bootstrap_directive.is_none() {
        return Ok(None);
    }
    // We have _a_ bootstrap directives, but 2 is more than we bargained for.
    if bootstrap_directives.next().is_some() {
        return Err(LinkError::BootstrapError(format!(
            "the @link specification itself (\"{}\") is applied multiple times",
            Identity::link_identity()
        )));
    }
    // At this point, we know this schema uses "our" @link. So we now "just" want to validate
    // all of the @link usages (starting with the bootstrapping one) and extract their metadata.
    let link_name_in_schema = &bootstrap_directive.unwrap().name;
    let mut links = Vec::new();
    let mut by_identity = HashMap::new();
    let mut by_name_in_schema = HashMap::new();
    let mut types_by_imported_name = HashMap::new();
    let mut directives_by_imported_name = HashMap::new();
    let link_applications = schema
        .schema_definition
        .directives
        .iter()
        .filter(|d| d.name == *link_name_in_schema);
    for application in link_applications {
        let link = Arc::new(Link::from_directive_application(application)?);
        links.push(Arc::clone(&link));
        if by_identity
            .insert(link.url.identity.clone(), Arc::clone(&link))
            .is_some()
        {
            // TODO: we may want to lessen that limitation at some point. Including the same feature for 2 different major versions should be ok.
            return Err(LinkError::BootstrapError(format!(
                "duplicate @link inclusion of specification \"{}\"",
                link.url.identity
            )));
        }
        let name_in_schema = link.spec_name_in_schema();
        if let Some(other) = by_name_in_schema.insert(name_in_schema.clone(), Arc::clone(&link)) {
            return Err(LinkError::BootstrapError(format!(
                "name conflict: {} and {} are imported under the same name (consider using the `@link(as:)` argument to disambiguate)",
                other.url, link.url,
            )));
        }
    }

    // We do a 2nd pass to collect and validate all the imports (it's a separate path so we
    // know all the names of the spec linked in the schema).
    for link in &links {
        for import in &link.imports {
            let imported_name = import.imported_name();
            let element_map = if import.is_directive {
                // the name of each spec (in the schema) acts as an implicit import for a
                // directive of the same name. So one cannot import a direcitive with the
                // same name than a linked spec.
                if let Some(other) = by_name_in_schema.get(imported_name) {
                    if !Arc::ptr_eq(other, link) {
                        return Err(LinkError::BootstrapError(format!(
                            "import for '{}' of {} conflicts with spec {}",
                            import.imported_display_name(),
                            link.url,
                            other.url
                        )));
                    }
                }
                &mut directives_by_imported_name
            } else {
                &mut types_by_imported_name
            };
            if let Some((other_link, _)) = element_map.insert(
                imported_name.clone(),
                (Arc::clone(link), Arc::clone(import)),
            ) {
                return Err(LinkError::BootstrapError(format!(
                    "name conflict: both {} and {} import {}",
                    link.url,
                    other_link.url,
                    import.imported_display_name()
                )));
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

// Note: currently only recognizing @link, not @core. Doesn't feel worth bothering with @core at
// this point, but the latter uses the "feature" arg instead of "url".
fn parse_link_if_bootstrap_directive(schema: &Schema, directive: &Directive) -> bool {
    if let Some(definition) = schema.directive_definitions.get(&directive.name) {
        let locations = &definition.locations;
        let is_correct_def = definition.repeatable
            && locations.len() == 1
            && locations[0] == DirectiveLocation::Schema;
        let is_correct_def = is_correct_def
            && definition.arguments.iter().any(|arg| {
                arg.name == "as" && matches!(&*arg.ty, Type::Named(name) if name == "String")
            });
        let is_correct_def = is_correct_def && definition.arguments.iter().any(|arg| {
            arg.name == "url" && {
                // The "true" type of `url` in the @link spec is actually `String` (nullable), and this
                // for future-proofing reasons (the idea was that we may introduce later other
                // ways to identify specs that are not urls). But we allow the definition to
                // have a non-nullable type both for convenience and because some early
                // federation previews actually generated that.
                matches!(&*arg.ty, Type::Named(name) | Type::NonNullNamed(name) if name == "String")
            }
        });
        if !is_correct_def {
            return false;
        }
        if let Some(url) = directive
            .argument_by_name("url")
            .and_then(|value| value.as_str())
        {
            let url = url.parse::<Url>();
            let default_link_name = DEFAULT_LINK_NAME;
            let expected_name = directive
                .argument_by_name("as")
                .and_then(|value| value.as_str())
                .unwrap_or(default_link_name.as_str());
            return url.map_or(false, |url| {
                url.identity == Identity::link_identity() && directive.name == expected_name
            });
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use apollo_compiler::name;

    use crate::link::{
        spec::{Version, APOLLO_SPEC_DOMAIN},
        {Import, Purpose},
    };

    use super::*;

    #[test]
    fn explicit_root_directive_import() -> Result<(), LinkError> {
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

        let meta = links_metadata(&schema)?;
        let meta = meta.expect("should have metadata");

        assert!(meta
            .source_link_of_directive(&name!("inaccessible"))
            .is_some());

        Ok(())
    }

    #[test]
    fn url_syntax() -> Result<(), LinkError> {
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

        let meta = links_metadata(&schema)?;
        let meta = meta.expect("should have metadata");

        assert!(meta
            .source_link_of_directive(&name!("myDirective"))
            .is_some());

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

        let meta = links_metadata(&schema)
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

        let fed_spec = meta
            .for_identity(&Identity {
                domain: APOLLO_SPEC_DOMAIN.to_string(),
                name: name!("federation"),
            })
            .unwrap();
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
            let errors = links_metadata(&schema).expect_err("should error");
            // TODO Multiple errors
            insta::assert_snapshot!(errors, @r###"Invalid use of @link in schema: invalid sub-value for @link(import:) argument: values should be either strings or input object values of the form { name: "<importedElement>", as: "<alias>" }."###);
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
            let errors = links_metadata(&schema).expect_err("should error");
            // TODO Multiple errors
            insta::assert_snapshot!(errors, @"Invalid use of @link in schema: invalid alias 'myKey' for import name '@key': should start with '@' since the imported name does");
        }

        // TODO Implement
        /*
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
            let errors = links_metadata(&schema).expect_err("should error");
            insta::assert_snapshot!(errors, @"");
        }
        */
    }
}
