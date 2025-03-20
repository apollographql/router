use apollo_compiler::Name;
use apollo_compiler::Schema;
use apollo_compiler::ast::Directive;
use apollo_compiler::ast::NamedType;

use crate::JOIN_VERSIONS;
use crate::error::FederationError;
use crate::link::DEFAULT_LINK_NAME;
use crate::link::Import;
use crate::link::Purpose;
use crate::link::federation_spec_definition::FEDERATION_FIELDS_ARGUMENT_NAME;
use crate::link::federation_spec_definition::FEDERATION_KEY_DIRECTIVE_NAME_IN_SPEC;
use crate::link::federation_spec_definition::FEDERATION_PROVIDES_DIRECTIVE_NAME_IN_SPEC;
use crate::link::federation_spec_definition::FEDERATION_REQUIRES_DIRECTIVE_NAME_IN_SPEC;
use crate::link::federation_spec_definition::get_federation_spec_definition_from_subgraph;
use crate::link::link_spec_definition::LINK_VERSIONS;
use crate::link::spec::Identity;
use crate::link::spec::Url;
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
            let latest_version = LINK_VERSIONS.versions().last().unwrap();
            let link_spec = LINK_VERSIONS.find(latest_version).unwrap();
            link_spec.add_elements_to_schema(schema)?;
        }
        Ok(schema.get_directive_definition(&DEFAULT_LINK_NAME))
    }

    fn on_directive_definition_and_schema_parsed(
        schema: &mut FederationSchema,
    ) -> Result<(), FederationError> {
        let federation_spec = get_federation_spec_definition_from_subgraph(schema)?;
        if federation_spec.is_fed1() {
            Self::remove_federation_definitions_broken_in_known_ways(schema)?;
        }
        federation_spec.add_elements_to_schema(schema)?;
        Self::expand_known_features(schema)
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
        for directive_name in &[
            FEDERATION_KEY_DIRECTIVE_NAME_IN_SPEC,
            FEDERATION_PROVIDES_DIRECTIVE_NAME_IN_SPEC,
            FEDERATION_REQUIRES_DIRECTIVE_NAME_IN_SPEC,
        ] {
            if let Some(pos) = schema.get_directive_definition(directive_name) {
                let directive = pos.get(schema.schema())?;
                if directive.arguments.len() == 0
                    || (directive.arguments.len() == 1
                        && directive
                            .argument_by_name(&FEDERATION_FIELDS_ARGUMENT_NAME)
                            .is_some_and(|fields| {
                                fields.ty.inner_named_type() == "String"
                                    || fields.ty.inner_named_type() == "_FieldSet"
                                    || fields.ty.inner_named_type() == "FieldSet"
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

        for link in links_metadata.links.clone() {
            if link.url.identity == Identity::join_identity() {
                let spec = JOIN_VERSIONS.find(&link.url.version).unwrap(); // TODO: Handle error
                spec.add_elements_to_schema(schema)?;
            }
            // TODO: Remaining known features
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
                name!("external"),
                name!("include"),
                name!("key"),
                name!("link"),
                name!("override"),
                name!("provides"),
                name!("requires"),
                name!("shareable"),
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
                name!("composeDirective"),
                name!("deprecated"),
                name!("external"),
                name!("include"),
                name!("key"),
                name!("link"),
                name!("override"),
                name!("provides"),
                name!("requires"),
                name!("shareable"),
                name!("skip"),
                name!("specifiedBy"),
            ]
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
