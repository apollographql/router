use std::sync::Arc;

use apollo_compiler::Name;
use apollo_compiler::Schema;
use apollo_compiler::ast::Directive;
use apollo_compiler::ast::NamedType;
use apollo_compiler::name;
use apollo_compiler::ty;

use crate::bail;
use crate::error::FederationError;
use crate::error::MultiTry;
use crate::error::MultiTryAll;
use crate::link::DEFAULT_LINK_NAME;
use crate::link::Import;
use crate::link::Link;
use crate::link::Purpose;
use crate::link::federation_spec_definition::FEDERATION_FIELDS_ARGUMENT_NAME;
use crate::link::federation_spec_definition::FEDERATION_KEY_DIRECTIVE_NAME_IN_SPEC;
use crate::link::federation_spec_definition::FEDERATION_PROVIDES_DIRECTIVE_NAME_IN_SPEC;
use crate::link::federation_spec_definition::FEDERATION_REQUIRES_DIRECTIVE_NAME_IN_SPEC;
use crate::link::federation_spec_definition::FederationSpecDefinition;
use crate::link::federation_spec_definition::get_federation_spec_definition_from_subgraph;
use crate::link::link_spec_definition::link_spec_latest;
use crate::link::spec::Identity;
use crate::link::spec::Url;
use crate::link::spec::Version;
use crate::link::spec_definition::SpecDefinition;
use crate::schema::FederationSchema;
use crate::schema::compute_subgraph_metadata;
use crate::schema::position::DirectiveDefinitionPosition;

#[allow(dead_code)]
struct CoreFeature {
    url: Url,
    name_in_schema: Name,
    directive: Directive,
    imports: Vec<Import>,
    purpose: Option<Purpose>,
}
#[allow(dead_code)]
struct FederationBlueprint {
    with_root_type_renaming: bool,
}

// PORT_NOTE: From `FAKE_FED1_CORE_FEATURE_TO_RENAME_TYPES` in JS
/**
 * Federation 1 has that specificity that it wasn't using @link to name-space federation elements,
 * and so to "distinguish" the few federation type names, it prefixed those with a `_`. That is,
 * the `FieldSet` type was named `_FieldSet` in federation1. To handle this without too much effort,
 * we use a fake `CoreFeature` with imports for all the fed1 types to use those specific "aliases"
 * and we pass it when adding those types. This allows to reuse the same `TypeSpecification` objects
 * for both fed1 and fed2. Note that in the object below, all that is used is the imports, the rest
 * is just filling the blanks.
 */
fn fake_fed1_link_to_rename_types(fed_spec_def: &FederationSpecDefinition) -> Arc<Link> {
    let fed1_types = fed_spec_def.type_specs();
    let imports = fed1_types
        .iter()
        .map(|type_spec| {
            Arc::new(Import {
                element: type_spec.name().clone(),
                is_directive: false,
                alias: Some(Name::new_unchecked(&format!("_{}", type_spec.name()))),
            })
        })
        .collect();
    // PORT_NOTE: `Link` has no fields to save a directive, while JS version saves a fake one.
    Arc::new(Link {
        url: Url {
            identity: Identity {
                domain: "<fed1>".to_string(),
                name: name!("fed1"),
            },
            version: Version { major: 0, minor: 1 },
        },
        spec_alias: Some(name!("fed1")),
        imports,
        purpose: None,
    })
}

#[allow(dead_code)]
impl FederationBlueprint {
    fn new(with_root_type_renaming: bool) -> Self {
        Self {
            with_root_type_renaming,
        }
    }

    fn on_missing_directive_definition(
        schema: &mut FederationSchema,
        directive: &Directive,
    ) -> Result<Option<DirectiveDefinitionPosition>, FederationError> {
        if directive.name == DEFAULT_LINK_NAME {
            // TODO: pass `alias` and `imports`
            link_spec_latest().add_definitions_to_schema(schema, /*alias*/ None)?;
            Ok(schema.get_directive_definition(&directive.name))
        } else {
            Ok(None)
        }
    }

    fn on_directive_definition_and_schema_parsed(
        schema: &mut FederationSchema,
    ) -> Result<(), FederationError> {
        // PORT_NOTE: Mirrors a part of `completeSubgraphSchema`
        let federation_spec = get_federation_spec_definition_from_subgraph(schema)?;
        if federation_spec.is_fed1() {
            // PORT_NOTE: Mirrors a part of `completeFed1SubgraphSchema`
            Self::remove_federation_definitions_broken_in_known_ways(schema)?;
            let fed1_types = federation_spec.type_specs();
            let fed1_directives = federation_spec.directive_specs();
            let fake_fed1_link = fake_fed1_link_to_rename_types(federation_spec);
            Ok(())
                .and_try(
                    fed1_types.iter().try_for_all(|type_spec| {
                        type_spec.check_or_add(schema, Some(&fake_fed1_link))
                    }),
                )
                .and_try(
                    fed1_directives
                        .iter()
                        .try_for_all(|directive_spec| directive_spec.check_or_add(schema, None)),
                )?;
            Self::expand_known_features(schema)
        } else {
            link_spec_latest().add_to_schema(schema, /*alias*/ None)?;
            // PORT_NOTE: Mirrors a part of `completeFed2SubgraphSchema`?
            federation_spec.add_elements_to_schema(schema)?;
            Self::expand_known_features(schema)
        }
    }

    fn ignore_parsed_field(_type: NamedType, _field_name: &str) -> bool {
        todo!()
    }

    fn on_constructed(schema: &mut FederationSchema) -> Result<(), FederationError> {
        if schema.subgraph_metadata.is_none() {
            schema.subgraph_metadata = compute_subgraph_metadata(schema)?.map(Box::new);
        }
        Ok(())
    }

    fn on_added_core_feature(_schema: &mut Schema, _feature: &CoreFeature) {
        todo!()
    }

    fn on_invalidation(_: &Schema) {
        todo!()
    }

    fn on_validation(_schema: &Schema) -> Result<(), FederationError> {
        todo!()
    }

    fn on_apollo_rs_validation_error(
        _error: apollo_compiler::validation::WithErrors<Schema>,
    ) -> FederationError {
        todo!()
    }

    fn on_unknown_directive_validation_error(
        _schema: &Schema,
        _unknown_directive_name: &str,
        _error: FederationError,
    ) -> FederationError {
        todo!()
    }

    fn apply_directives_after_parsing() -> bool {
        todo!()
    }

    fn remove_federation_definitions_broken_in_known_ways(
        schema: &mut FederationSchema,
    ) -> Result<(), FederationError> {
        // We special case @key, @requires and @provides because we've seen existing user schemas where those
        // have been defined in an invalid way, but in a way that fed1 wasn't rejecting. So for convenience,
        // if we detect one of those case, we just remove the definition and let the code afteward add the
        // proper definition back.
        // Note that, in a perfect world, we'd do this within the `SchemaUpgrader`. But the way the code
        // is organised, this method is called before we reach the `SchemaUpgrader`, and it doesn't seem
        // worth refactoring things drastically for that minor convenience.
        for directive_name in &[
            FEDERATION_KEY_DIRECTIVE_NAME_IN_SPEC,
            FEDERATION_PROVIDES_DIRECTIVE_NAME_IN_SPEC,
            FEDERATION_REQUIRES_DIRECTIVE_NAME_IN_SPEC,
        ] {
            if let Some(pos) = schema.get_directive_definition(directive_name) {
                let directive = pos.get(schema.schema())?;
                // We shouldn't have applications at the time of this writing because `completeSubgraphSchema`, which calls this,
                // is only called:
                // 1. during schema parsing, by `FederationBluePrint.onDirectiveDefinitionAndSchemaParsed`, and that is called
                //   before we process any directive applications.
                // 2. by `setSchemaAsFed2Subgraph`, but as the name imply, this trickles to `completeFed2SubgraphSchema`, not
                //   this one method.
                // In other words, there is currently no way to create a full fed1 schema first, and get that method called
                // second. If that changes (no real reason but...), we'd have to modify this because when we remove the
                // definition to re-add the "correct" version, we'd have to re-attach existing applications (doable but not
                // done). This assert is so we notice it quickly if that ever happens (again, unlikely, because fed1 schema
                // is a backward compatibility thing and there is no reason to expand that too much in the future).
                if schema.referencers().get_directive(directive_name)?.len() > 0 {
                    bail!(
                        "Subgraph has applications of @{directive_name} but we are trying to remove the definition."
                    );
                }

                // The patterns we recognize and "correct" (by essentially ignoring the definition) are:
                //  1. if the definition has no arguments at all.
                //  2. if the `fields` argument is declared as nullable.
                //  3. if the `fields` argument type is named "FieldSet" instead of "_FieldSet".
                // All of these correspond to things we've seen in user schemas.
                //
                // To be on the safe side, we check that `fields` is the only argument. That's because
                // fed2 accepts the optional `resolvable` arg for @key, fed1 only ever had one arguemnt.
                // If the user had defined more arguments _and_ provided values for the extra argument,
                // removing the definition would create validation errors that would be hard to understand.
                if directive.arguments.is_empty()
                    || (directive.arguments.len() == 1
                        && directive
                            .argument_by_name(&FEDERATION_FIELDS_ARGUMENT_NAME)
                            .is_some_and(|fields| {
                                *fields.ty == ty!(String)
                                    || *fields.ty == ty!(_FieldSet)
                                    || *fields.ty == ty!(FieldSet)
                            }))
                {
                    pos.remove(schema)?;
                }
            }
        }
        Ok(())
    }

    fn expand_known_features(schema: &mut FederationSchema) -> Result<(), FederationError> {
        let Some(links_metadata) = schema.metadata() else {
            return Ok(());
        };

        for _link in links_metadata.links.clone() {
            // TODO: Pick out known features by link identity and call `add_elements_to_schema`.
            // JS calls coreFeatureDefinitionIfKnown here, but we don't have a feature registry yet.
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use apollo_compiler::Node;
    use apollo_compiler::ast::Argument;
    use apollo_compiler::name;
    use apollo_compiler::schema::Component;

    use super::*;
    use crate::error::FederationError;
    use crate::schema::FederationSchema;
    use crate::schema::ValidFederationSchema;

    #[test]
    fn detects_federation_1_subgraphs_correctly() {
        let schema = Schema::parse(
            r#"
                type Query {
                    s: String
                }"#,
            "empty-fed1-schema.graphqls",
        )
        .expect("valid schema");
        let subgraph = build_subgraph(&schema, true).expect("builds subgraph");
        let metadata = subgraph.subgraph_metadata().expect("has metadata");

        assert!(!metadata.is_fed_2_schema());
    }

    #[test]
    fn detects_federation_2_subgraphs_correctly() {
        let schema = Schema::parse(
            r#"
                extend schema @link(url: "https://specs.apollo.dev/federation/v2.0")

                type Query {
                    s: String
                }"#,
            "empty-fed2-schema.graphqls",
        )
        .expect("valid schema");
        let subgraph = build_subgraph(&schema, true).expect("builds subgraph");
        let metadata = subgraph.subgraph_metadata().expect("has metadata");

        assert!(metadata.is_fed_2_schema());
    }

    #[test]
    fn injects_missing_directive_definitions_fed_1_0() {
        let schema = Schema::parse(
            r#"
                type Query {
                    s: String
                }"#,
            "empty-fed1-schema.graphqls",
        )
        .expect("valid schema");
        let subgraph = build_subgraph(&schema, true).expect("builds subgraph");

        let mut defined_directive_names = subgraph
            .schema()
            .directive_definitions
            .keys()
            .cloned()
            .collect::<Vec<_>>();
        defined_directive_names.sort();

        assert_eq!(
            defined_directive_names,
            vec![
                name!("deprecated"),
                name!("extends"),
                name!("external"),
                name!("include"),
                name!("key"),
                name!("provides"),
                name!("requires"),
                name!("skip"),
                name!("specifiedBy"),
            ]
        );
    }

    #[test]
    fn injects_missing_directive_definitions_fed_2_0() {
        let schema = Schema::parse(
            r#"
                extend schema @link(url: "https://specs.apollo.dev/federation/v2.0")

                type Query {
                    s: String
                }"#,
            "empty-fed2-schema.graphqls",
        )
        .expect("valid schema");
        let subgraph = build_subgraph(&schema, true).expect("builds subgraph");

        let mut defined_directive_names = subgraph
            .schema()
            .directive_definitions
            .keys()
            .cloned()
            .collect::<Vec<_>>();
        defined_directive_names.sort();

        assert_eq!(
            defined_directive_names,
            vec![
                name!("deprecated"),
                name!("federation__external"),
                name!("federation__key"),
                name!("federation__override"),
                name!("federation__provides"),
                name!("federation__requires"),
                name!("federation__shareable"),
                name!("include"),
                name!("link"),
                name!("skip"),
                name!("specifiedBy"),
            ]
        );
    }

    #[test]
    fn injects_missing_directive_definitions_fed_2_1() {
        let schema = Schema::parse(
            r#"
                extend schema @link(url: "https://specs.apollo.dev/federation/v2.1")

                type Query {
                    s: String
                }"#,
            "empty-fed2-schema.graphqls",
        )
        .expect("valid schema");
        let subgraph = build_subgraph(&schema, true).expect("builds subgraph");

        let mut defined_directive_names = subgraph
            .schema()
            .directive_definitions
            .keys()
            .cloned()
            .collect::<Vec<_>>();
        defined_directive_names.sort();

        assert_eq!(
            defined_directive_names,
            vec![
                name!("deprecated"),
                name!("federation__composeDirective"),
                name!("federation__external"),
                name!("federation__key"),
                name!("federation__override"),
                name!("federation__provides"),
                name!("federation__requires"),
                name!("federation__shareable"),
                name!("include"),
                name!("link"),
                name!("skip"),
                name!("specifiedBy"),
            ]
        );
    }

    #[test]
    fn replaces_known_bad_definitions_from_fed1() {
        let schema = Schema::parse(
            r#"
                directive @key(fields: String) repeatable on OBJECT | INTERFACE
                directive @provides(fields: _FieldSet) repeatable on FIELD_DEFINITION
                directive @requires(fields: FieldSet) repeatable on FIELD_DEFINITION

                scalar _FieldSet
                scalar FieldSet

                type Query {
                    s: String
                }"#,
            "empty-fed1-schema.graphqls",
        )
        .expect("valid schema");
        let subgraph = build_subgraph(&schema, true).expect("builds subgraph");

        let key_definition = subgraph
            .schema()
            .directive_definitions
            .get(&name!("key"))
            .unwrap();
        assert_eq!(key_definition.arguments.len(), 2);
        assert_eq!(
            key_definition
                .argument_by_name(&name!("fields"))
                .unwrap()
                .ty
                .inner_named_type(),
            "_FieldSet"
        );
        assert!(
            key_definition
                .argument_by_name(&name!("resolvable"))
                .is_some()
        );

        let provides_definition = subgraph
            .schema()
            .directive_definitions
            .get(&name!("provides"))
            .unwrap();
        assert_eq!(provides_definition.arguments.len(), 1);
        assert_eq!(
            provides_definition
                .argument_by_name(&name!("fields"))
                .unwrap()
                .ty
                .inner_named_type(),
            "_FieldSet"
        );

        let requires_definition = subgraph
            .schema()
            .directive_definitions
            .get(&name!("requires"))
            .unwrap();
        assert_eq!(requires_definition.arguments.len(), 1);
        assert_eq!(
            requires_definition
                .argument_by_name(&name!("fields"))
                .unwrap()
                .ty
                .inner_named_type(),
            "_FieldSet"
        );
    }

    fn build_subgraph(
        source: &Schema,
        with_root_type_renaming: bool,
    ) -> Result<ValidFederationSchema, FederationError> {
        let blueprint = FederationBlueprint::new(with_root_type_renaming);
        let subgraph = build_schema(source, &blueprint)?;
        subgraph.validate_or_return_self().map_err(|(_, err)| err)
    }

    fn build_schema(
        schema: &Schema,
        _blueprint: &FederationBlueprint,
    ) -> Result<FederationSchema, FederationError> {
        let mut federation_schema = FederationSchema::new_uninitialized(schema.clone())?;

        // First, copy types over from the underlying schema AST to make sure we have built-ins that directives may reference
        federation_schema.collect_shallow_references();

        // Backfill missing directive definitions. This is primarily making sure we have a definition for `@link`.
        for directive in &schema.schema_definition.directives {
            if federation_schema
                .get_directive_definition(&directive.name)
                .is_none()
            {
                FederationBlueprint::on_missing_directive_definition(
                    &mut federation_schema,
                    directive,
                )?;
            }
        }

        // If there's a use of `@link`, and we successfully added its definition, add the bootstrap directive
        // TODO: We may need to do the same for `@core` on Fed 1 schemas.
        if federation_schema
            .get_directive_definition(&name!("link"))
            .is_some()
        {
            federation_schema
                .schema
                .schema_definition
                .make_mut()
                .directives
                .insert(
                    0,
                    Component::new(Directive {
                        name: name!("link"),
                        arguments: vec![Node::new(Argument {
                            name: name!("url"),
                            value: "https://specs.apollo.dev/link/v1.0".into(),
                        })],
                    }),
                );
        }

        // Now that we have the definition for `@link` and an application, the bootstrap directive detection should work.
        federation_schema.collect_links_metadata()?;

        FederationBlueprint::on_directive_definition_and_schema_parsed(&mut federation_schema)?;

        // Also, the backfilled definitions mean we can collect deep references.
        federation_schema.collect_deep_references()?;

        // TODO: In JS this happens inside the schema constructor; we should consider if that's the right thing to do here
        // Right now, this is down here because it eagerly evaluates directive usages for SubgraphMetadata, whereas the JS
        // code was lazy and we could call this hook to lazily use federation directives before actually adding their
        // definitions.
        FederationBlueprint::on_constructed(&mut federation_schema)?;

        Ok(federation_schema)
    }
}
