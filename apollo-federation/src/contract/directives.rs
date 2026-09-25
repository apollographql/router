use super::errors::DirectiveError;
use super::helpers::FilterDirectiveMetadata;
use crate::link::inaccessible_spec_definition::INACCESSIBLE_VERSIONS;
use crate::link::spec::Identity;
use crate::link::spec::Version;
use crate::link::spec_definition::SpecDefinition;
use crate::link::tag_spec_definition::TAG_VERSIONS;
use crate::schema::FederationSchema;

/// Step 1: Add directive definitions for @tag and @inaccessible if not present.
///
/// Ensures that both @tag and @inaccessible directive definitions exist in the schema.
/// If a spec is not linked, adds the `@link` for its latest version. Either way the definitions themselves
/// go through the core spec machinery, so filtering accepts exactly the definitions the
/// rest of `apollo-federation` accepts.
///
/// Returns the [`FilterDirectiveMetadata`] the rest of the pipeline should use. This step is
/// the only one that can add `@link` applications, so it is also the only place that can
/// resolve those names; it does so once, after the links are settled.
pub(crate) fn step1_add_directive_definitions(
    schema: &mut FederationSchema,
) -> Result<FilterDirectiveMetadata, DirectiveError> {
    // Which specs are already linked comes straight off the schema's own link metadata.
    // Building `FilterDirectiveMetadata` here would resolve directive names and clone the
    // whole link map to answer one question, and linking a spec below invalidates all of it.
    match linked_version(schema, &Identity::tag_identity()) {
        // Check against the version the schema actually links, not the one we would have
        // picked: a supergraph from an older composition links an older tag spec, and its
        // definition is correct for that spec.
        Some(version) => {
            let spec =
                TAG_VERSIONS
                    .find(&version)
                    .ok_or_else(|| DirectiveError::UnsupportedVersion {
                        spec: "tag",
                        version: version.to_string(),
                    })?;
            spec.add_elements_to_schema(schema)?;
        }
        None => link_spec(schema, TAG_VERSIONS.latest())?,
    }
    match linked_version(schema, &Identity::inaccessible_identity()) {
        Some(version) => {
            let spec = INACCESSIBLE_VERSIONS.find(&version).ok_or_else(|| {
                DirectiveError::UnsupportedVersion {
                    spec: "inaccessible",
                    version: version.to_string(),
                }
            })?;
            spec.add_elements_to_schema(schema)?;
        }
        None => link_spec(schema, INACCESSIBLE_VERSIONS.latest())?,
    }

    // Resolved once, with every `@link` in place.
    let metadata = FilterDirectiveMetadata::from_schema(schema);

    Ok(metadata)
}

/// The version of `identity` the schema links, if it links it at all.
fn linked_version(schema: &FederationSchema, identity: &Identity) -> Option<Version> {
    schema
        .metadata()?
        .for_identity(identity)
        .map(|link| link.url.version.clone())
}

/// Link `spec` into the schema and add the elements it defines, through the same core
/// helper composition uses.
///
/// The spec supplies its own `for:` purpose, and the `@link` spec definition is read from
/// the schema rather than assumed, which is also what lets the helper reject a spec the
/// schema's `@link` version cannot express. `insert_directive_at` recomputes the schema's
/// link metadata, so the `add_elements_to_schema` call inside resolves the spec just linked.
fn link_spec(
    schema: &mut FederationSchema,
    spec: &'static dyn SpecDefinition,
) -> Result<(), DirectiveError> {
    let Some(link_spec) = schema
        .metadata()
        .map(|metadata| metadata.link_spec_definition())
    else {
        return Err(DirectiveError::MissingSpec { spec: "link" });
    };
    link_spec.apply_feature_to_schema(schema, spec, None, spec.purpose(), None, Err)?;
    Ok(())
}

#[cfg(test)]
mod add_directive_definitions_tests {
    use apollo_compiler::Schema;

    use super::*;
    use crate::contract::errors::DirectiveError;
    use crate::contract::testing::Preamble;
    use crate::contract::testing::Tester;
    use crate::contract::testing::federation_schema;
    use crate::contract::testing::supergraph_with;

    const TAG_URL: &str = "https://specs.apollo.dev/tag/v0.3";
    const INACCESSIBLE_URL: &str = "https://specs.apollo.dev/inaccessible/v0.2";

    /// `@tag` and `@inaccessible` as the tag v0.3 / inaccessible v0.2 specs define them --
    /// the latest versions, which step 1 links when the schema links neither. These pin that
    /// choice: a new spec version should update them.
    /// Step 1 no longer spells these out -- `check_or_add` builds them from the spec.
    const ADDED_TAG: &str = "directive @tag(name: String!) repeatable on FIELD_DEFINITION | OBJECT | INTERFACE | UNION | ARGUMENT_DEFINITION | SCALAR | ENUM | ENUM_VALUE | INPUT_OBJECT | INPUT_FIELD_DEFINITION | SCHEMA";
    const ADDED_INACCESSIBLE: &str = "directive @inaccessible on FIELD_DEFINITION | OBJECT | INTERFACE | UNION | ARGUMENT_DEFINITION | SCALAR | ENUM | ENUM_VALUE | INPUT_OBJECT | INPUT_FIELD_DEFINITION";

    fn add_directive_definitions(preamble: &Preamble, sdl: &str) -> Result<Schema, DirectiveError> {
        let mut schema = federation_schema(supergraph_with(preamble, sdl));
        step1_add_directive_definitions(&mut schema)?;
        Ok(schema.into_inner())
    }

    /// Neither spec is linked and neither definition is present.
    fn without_specs() -> Preamble {
        Preamble {
            use_tag_spec: false,
            use_tag_spec_elements: false,
            use_inaccessible_spec: false,
            use_inaccessible_spec_elements: false,
            ..Preamble::default()
        }
    }

    #[test]
    fn adds_missing_tag_and_inaccessible_specs_and_definitions() {
        let schema = add_directive_definitions(&without_specs(), "type Query { foo: String }")
            .expect("step 1 should succeed");

        let tester = Tester::new(&schema);
        assert_eq!(
            tester.link(TAG_URL),
            r#"@link(url: "https://specs.apollo.dev/tag/v0.3")"#,
        );
        // The inaccessible spec is linked `for: SECURITY` from the start.
        assert_eq!(
            tester.link(INACCESSIBLE_URL),
            r#"@link(url: "https://specs.apollo.dev/inaccessible/v0.2", for: SECURITY)"#,
        );
        assert_eq!(tester.directive("tag"), ADDED_TAG);
        assert_eq!(tester.directive("inaccessible"), ADDED_INACCESSIBLE);
    }

    #[test]
    fn does_not_clobber_existing_tag_and_inaccessible_specs() {
        let schema = add_directive_definitions(&Preamble::default(), "type Query { foo: String }")
            .expect("step 1 should succeed");

        let tester = Tester::new(&schema);
        assert_eq!(
            tester.link(TAG_URL),
            r#"@link(url: "https://specs.apollo.dev/tag/v0.3")"#,
        );
        // Left as the preamble declared it.
        assert_eq!(
            tester.link(INACCESSIBLE_URL),
            r#"@link(url: "https://specs.apollo.dev/inaccessible/v0.2")"#,
        );
        // The preamble's descriptions survive, which they would not have had the
        // definitions been replaced.
        assert!(
            tester
                .directive("tag")
                .contains("Composition @tag definition"),
            "expected the preamble's @tag definition to survive, got: {}",
            tester.directive("tag"),
        );
        assert!(
            tester
                .directive("inaccessible")
                .contains("Composition @inaccessible definition"),
            "expected the preamble's @inaccessible definition to survive, got: {}",
            tester.directive("inaccessible"),
        );
    }

    /// The `@link` applications step 1 adds must use the schema's name for `@link`.
    #[test]
    fn adds_missing_specs_when_the_link_directive_is_renamed() {
        let preamble = Preamble {
            use_core_spec: false,
            use_core_spec_elements: false,
            use_join_spec: false,
            ..without_specs()
        };
        let schema = add_directive_definitions(
            &preamble,
            r#"
            directive @lonk(url: String, as: String, for: lonk__Purpose, import: [lonk__Import]) repeatable on SCHEMA

            enum lonk__Purpose { SECURITY EXECUTION }
            scalar lonk__Import

            extend schema
              @lonk(url: "https://specs.apollo.dev/link/v1.0", as: "lonk")
              @lonk(url: "https://specs.apollo.dev/join/v2.0")

            type Query { foo: String }
            "#,
        )
        .expect("step 1 should succeed");

        let tester = Tester::new(&schema);
        assert_eq!(
            tester.link(TAG_URL),
            r#"@lonk(url: "https://specs.apollo.dev/tag/v0.3")"#,
        );
        assert_eq!(
            tester.link(INACCESSIBLE_URL),
            r#"@lonk(url: "https://specs.apollo.dev/inaccessible/v0.2", for: SECURITY)"#,
        );
        assert_eq!(tester.directive("tag"), ADDED_TAG);
        assert_eq!(tester.directive("inaccessible"), ADDED_INACCESSIBLE);
    }

    #[test]
    fn does_not_clobber_existing_specs_when_the_link_directive_is_renamed() {
        let preamble = Preamble {
            use_core_spec: false,
            use_core_spec_elements: false,
            use_join_spec: false,
            use_tag_spec: false,
            use_inaccessible_spec: false,
            ..Preamble::default()
        };
        let schema = add_directive_definitions(
            &preamble,
            r#"
            directive @lonk(url: String, as: String, for: lonk__Purpose, import: [lonk__Import]) repeatable on SCHEMA

            enum lonk__Purpose { SECURITY EXECUTION }
            scalar lonk__Import

            extend schema
              @lonk(url: "https://specs.apollo.dev/link/v1.0", as: "lonk")
              @lonk(url: "https://specs.apollo.dev/join/v2.0")
              @lonk(url: "https://specs.apollo.dev/tag/v0.3")
              @lonk(url: "https://specs.apollo.dev/inaccessible/v0.2")

            type Query { foo: String }
            "#,
        )
        .expect("step 1 should succeed");

        let tester = Tester::new(&schema);
        assert_eq!(
            tester.link(INACCESSIBLE_URL),
            r#"@lonk(url: "https://specs.apollo.dev/inaccessible/v0.2")"#,
        );
        assert!(
            tester
                .directive("tag")
                .contains("Composition @tag definition"),
            "expected the preamble's @tag definition to survive",
        );
    }

    /// `@link(as:)` renames the directives; the existing definitions under those names
    /// must be left alone.
    #[test]
    fn does_not_clobber_existing_specs_when_tag_and_inaccessible_are_renamed() {
        let schema = add_directive_definitions(
            &without_specs(),
            r#"
            "Composition @tog definition"
            directive @tog(name: String!) repeatable on FIELD_DEFINITION | INTERFACE | OBJECT | UNION | ARGUMENT_DEFINITION | SCALAR | ENUM | ENUM_VALUE | INPUT_OBJECT | INPUT_FIELD_DEFINITION | SCHEMA

            "Composition @onaccessible definition"
            directive @onaccessible on FIELD_DEFINITION | INTERFACE | OBJECT | UNION | ARGUMENT_DEFINITION | SCALAR | ENUM | ENUM_VALUE | INPUT_OBJECT | INPUT_FIELD_DEFINITION

            extend schema
              @link(url: "https://specs.apollo.dev/tag/v0.3", as: "tog")
              @link(url: "https://specs.apollo.dev/inaccessible/v0.2", as: "onaccessible")

            type Query { foo: String }
            "#,
        )
        .expect("step 1 should succeed");

        let tester = Tester::new(&schema);
        // The `as:` arguments survive, and inaccessible is still not `for: SECURITY`.
        assert_eq!(
            tester.link(TAG_URL),
            r#"@link(url: "https://specs.apollo.dev/tag/v0.3", as: "tog")"#,
        );
        assert_eq!(
            tester.link(INACCESSIBLE_URL),
            r#"@link(url: "https://specs.apollo.dev/inaccessible/v0.2", as: "onaccessible")"#,
        );
        assert!(
            tester
                .directive("tog")
                .contains("Composition @tog definition"),
            "expected the @tog definition to survive",
        );
        assert!(
            tester
                .directive("onaccessible")
                .contains("Composition @onaccessible definition"),
            "expected the @onaccessible definition to survive",
        );
    }

    /// A schema that links a spec but omits its definition gets the definition supplied,
    /// rather than an error: the core spec machinery adds what is missing and only rejects
    /// a definition that is present and wrong.
    #[test]
    fn supplies_a_definition_when_the_tag_spec_is_linked_without_one() {
        let preamble = Preamble {
            use_tag_spec_elements: false,
            ..Preamble::default()
        };
        let schema = add_directive_definitions(&preamble, "type Query { foo: String }")
            .expect("step 1 should succeed");

        assert_eq!(Tester::new(&schema).directive("tag"), ADDED_TAG);
    }

    #[test]
    fn supplies_a_definition_when_the_inaccessible_spec_is_linked_without_one() {
        let preamble = Preamble {
            use_inaccessible_spec_elements: false,
            ..Preamble::default()
        };
        let schema = add_directive_definitions(&preamble, "type Query { foo: String }")
            .expect("step 1 should succeed");

        assert_eq!(
            Tester::new(&schema).directive("inaccessible"),
            ADDED_INACCESSIBLE
        );
    }

    #[test]
    fn fails_when_the_inaccessible_definition_has_arguments() {
        let preamble = Preamble {
            use_inaccessible_spec_elements: false,
            ..Preamble::default()
        };
        let error = add_directive_definitions(
            &preamble,
            r#"
            directive @inaccessible(name: String!) on FIELD_DEFINITION | INTERFACE | OBJECT | UNION | ARGUMENT_DEFINITION | SCALAR | ENUM | ENUM_VALUE | INPUT_OBJECT | INPUT_FIELD_DEFINITION

            type Query { foo: String }
            "#,
        )
        .expect_err("step 1 should fail");

        // Reported by the core spec machinery rather than by a contract-local check.
        assert!(matches!(&error, DirectiveError::Federation(_)), "{error}");
        assert!(
            error
                .to_string()
                .contains(r#"unknown/unsupported argument "name""#),
            "{error}"
        );
    }

    #[test]
    fn fails_when_the_inaccessible_definition_is_repeatable() {
        let preamble = Preamble {
            use_inaccessible_spec_elements: false,
            ..Preamble::default()
        };
        let error = add_directive_definitions(
            &preamble,
            r#"
            directive @inaccessible repeatable on FIELD_DEFINITION | INTERFACE | OBJECT | UNION | ARGUMENT_DEFINITION | SCALAR | ENUM | ENUM_VALUE | INPUT_OBJECT | INPUT_FIELD_DEFINITION

            type Query { foo: String }
            "#,
        )
        .expect_err("step 1 should fail");

        assert!(matches!(&error, DirectiveError::Federation(_)), "{error}");
        assert!(
            error
                .to_string()
                .contains(r#""@inaccessible" should not be repeatable"#),
            "{error}"
        );
    }
}
