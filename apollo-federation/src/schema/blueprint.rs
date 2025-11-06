use std::collections::HashSet;

use apollo_compiler::Name;
use apollo_compiler::Node;
use apollo_compiler::ast::Directive;
use apollo_compiler::ty;

use crate::bail;
use crate::error::FederationError;
use crate::error::MultipleFederationErrors;
use crate::error::SingleFederationError;
use crate::error::suggestion::did_you_mean;
use crate::error::suggestion::suggestion_list;
use crate::link::DEFAULT_LINK_NAME;
use crate::link::Link;
use crate::link::federation_spec_definition::FEDERATION_FIELDS_ARGUMENT_NAME;
use crate::link::federation_spec_definition::FEDERATION_KEY_DIRECTIVE_NAME_IN_SPEC;
use crate::link::federation_spec_definition::FEDERATION_PROVIDES_DIRECTIVE_NAME_IN_SPEC;
use crate::link::federation_spec_definition::FEDERATION_REQUIRES_DIRECTIVE_NAME_IN_SPEC;
use crate::link::federation_spec_definition::FEDERATION_VERSIONS;
use crate::link::federation_spec_definition::FederationSpecDefinition;
use crate::link::federation_spec_definition::get_federation_spec_definition_from_subgraph;
use crate::link::link_spec_definition::LinkSpecDefinition;
use crate::link::spec::Identity;
use crate::link::spec_definition::SpecDefinition;
use crate::schema::FederationSchema;
use crate::schema::ValidFederationSchema;
use crate::schema::compute_subgraph_metadata;
use crate::schema::position::DirectiveDefinitionPosition;
use crate::schema::subgraph_metadata::SubgraphMetadata;
use crate::schema::validators::context::validate_context_directives;
use crate::schema::validators::cost::validate_cost_directives;
use crate::schema::validators::external::validate_external_directives;
use crate::schema::validators::from_context::validate_from_context_directives;
use crate::schema::validators::interface_object::validate_interface_object_directives;
use crate::schema::validators::key::validate_key_directives;
use crate::schema::validators::list_size::validate_list_size_directives;
use crate::schema::validators::provides::validate_provides_directives;
use crate::schema::validators::requires::validate_requires_directives;
use crate::schema::validators::shareable::validate_shareable_directives;
use crate::schema::validators::tag::validate_tag_directives;
use crate::supergraph::FEDERATION_ENTITIES_FIELD_NAME;
use crate::supergraph::FEDERATION_SERVICE_FIELD_NAME;

pub(crate) struct FederationBlueprint {}

#[allow(dead_code)]
impl FederationBlueprint {
    pub(crate) fn on_missing_directive_definition(
        schema: &mut FederationSchema,
        directive: &Node<Directive>,
    ) -> Result<Option<DirectiveDefinitionPosition>, FederationError> {
        if directive.name == DEFAULT_LINK_NAME {
            let (alias, imports) =
                LinkSpecDefinition::extract_alias_and_imports_on_missing_link_directive_definition(
                    directive,
                )?;
            LinkSpecDefinition::latest().add_definitions_to_schema(schema, alias, imports)?;
            Ok(schema.get_directive_definition(&directive.name))
        } else {
            Ok(None)
        }
    }

    pub(crate) fn on_directive_definition_and_schema_parsed(
        schema: &mut FederationSchema,
    ) -> Result<(), FederationError> {
        // PORT_NOTE: JS version calls `completeSubgraphSchema`. But, in Rust, it's implemented
        //            directly in this method and `Subgraph::expand_links`.
        let federation_spec = get_federation_spec_definition_from_subgraph(schema)?;
        if federation_spec.is_fed1() {
            Self::remove_federation_definitions_broken_in_known_ways(schema)?;
        }
        federation_spec.add_elements_to_schema(schema)?;
        Self::expand_known_features(schema)
    }

    pub(crate) fn ignore_parsed_field(schema: &FederationSchema, field_name: &str) -> bool {
        // Historically, federation 1 has accepted invalid schema, including some where the Query
        // type included the definition of `_entities` (so `_entities(representations: [_Any!]!):
        // [_Entity]!`) but _without_ defining the `_Any` or `_Entity` type. So while we want to be
        // stricter for fed2 (so this kind of really weird case can be fixed), we want fed2 to
        // accept as much fed1 schema as possible.
        //
        // So, to avoid this problem, we ignore the _entities and _service fields if we parse them
        // from a fed1 input schema. Those will be added back anyway (along with the proper types)
        // post-parsing.
        if !(FEDERATION_OPERATION_FIELDS.iter().any(|f| *f == field_name)) {
            return false;
        }
        if let Some(metadata) = &schema.subgraph_metadata {
            !metadata.is_fed_2_schema()
        } else {
            false
        }
    }

    pub(crate) fn on_constructed(schema: &mut FederationSchema) -> Result<(), FederationError> {
        if schema.subgraph_metadata.is_none() {
            schema.subgraph_metadata = compute_subgraph_metadata(schema)?.map(Box::new);
        }
        Ok(())
    }

    fn on_added_core_feature(
        schema: &mut FederationSchema,
        feature: &Link,
    ) -> Result<(), FederationError> {
        if feature.url.identity == Identity::federation_identity() {
            FEDERATION_VERSIONS
                .find(&feature.url.version)
                .iter()
                .try_for_each(|spec| spec.add_elements_to_schema(schema))?;
        }
        Ok(())
    }

    pub(crate) fn on_validation(
        schema: &ValidFederationSchema,
        meta: &SubgraphMetadata,
        subgraph_name: &str,
    ) -> Result<(), FederationError> {
        let mut error_collector = MultipleFederationErrors { errors: Vec::new() };

        // We skip the rest of validation for fed1 schemas because there is a number of validations that is stricter than what fed 1
        // accepted, and some of those issues are fixed by `SchemaUpgrader`. So insofar as any fed 1 schma is ultimately converted
        // to a fed 2 one before composition, then skipping some validation on fed 1 schema is fine.
        if !meta.is_fed_2_schema() {
            return error_collector.into_result();
        }

        let context_map = validate_context_directives(schema, &mut error_collector)?;
        validate_from_context_directives(
            schema,
            meta,
            &context_map,
            &mut error_collector,
            subgraph_name,
        )?;
        validate_key_directives(schema, meta, &mut error_collector)?;
        validate_provides_directives(schema, meta, &mut error_collector)?;
        validate_requires_directives(schema, meta, &mut error_collector)?;
        validate_external_directives(schema, meta, &mut error_collector)?;
        validate_interface_object_directives(schema, meta, &mut error_collector)?;
        validate_shareable_directives(schema, meta, &mut error_collector)?;
        validate_cost_directives(schema, &mut error_collector)?;
        validate_list_size_directives(schema, &mut error_collector)?;
        validate_tag_directives(schema, &mut error_collector)?;

        error_collector.into_result()
    }

    // Allows to intercept some apollo-compiler error messages when we can provide additional
    // guidance to users.
    pub(crate) fn on_invalid_graphql_error(
        schema: &FederationSchema,
        message: String,
    ) -> SingleFederationError {
        // PORT_NOTE: The following comment is from the JS version.
        // For now, the main additional guidance we provide is around directives, where we could
        // provide additional help in 2 main ways:
        // - if a directive name is likely misspelled.
        // - for fed 2 schema, if a federation directive is referred under it's "default" naming
        //   but is not properly imported (not enforced in the method but rather in the
        //   `FederationBlueprint`).
        //
        // Note that intercepting/parsing error messages to modify them is never ideal, but
        // pragmatically, it's probably better than rewriting the relevant rules entirely (in that
        // case, our "copied" rule may not benefit any potential apollo-compiler's improvements for
        // instance). And while such parsing is fragile, in that it'll break if the original
        // message change, we have unit tests to surface any such breakage so it's not really a
        // risk.

        let matcher = regex::Regex::new(r#"^Error: cannot find directive `@([^`]+)`"#).unwrap();
        let Some(capture) = matcher.captures(&message) else {
            // return as-is
            return SingleFederationError::InvalidGraphQL { message };
        };
        let Some(matched) = capture.get(1) else {
            // return as-is
            return SingleFederationError::InvalidGraphQL { message };
        };

        let directive_name = matched.as_str();
        let options: Vec<_> = schema
            .get_directive_definitions()
            .map(|d| d.directive_name.to_string())
            .collect();
        let suggestions = suggestion_list(directive_name, options);
        if suggestions.is_empty() {
            return Self::on_unknown_directive_validation_error(schema, directive_name, &message);
        }

        let did_you_mean = did_you_mean(suggestions.iter().map(|s| format!("@{s}")));
        SingleFederationError::InvalidGraphQL {
            message: format!("{message}{did_you_mean}\n"),
        }
    }

    fn on_unknown_directive_validation_error(
        schema: &FederationSchema,
        unknown_directive_name: &str,
        error_message: &str,
    ) -> SingleFederationError {
        let Some(metadata) = &schema.subgraph_metadata else {
            return SingleFederationError::Internal {
                message: "Missing subgraph metadata".to_string(),
            };
        };
        let is_fed2 = metadata.is_fed_2_schema();
        let all_directive_names = all_default_federation_directive_names();
        if all_directive_names.contains(unknown_directive_name) {
            // The directive name is "unknown" but it is a default federation directive name. So it
            // means one of a few things happened:
            //  1. it's a fed1 schema but the directive is fed2 only (only possible case for
            //     fed1 schema).
            //  2. the directive has not been imported at all (so needs to be prefixed for it to
            //     work).
            //  3. the directive has an `import`, but it's been aliased to another name.

            if !is_fed2 {
                // Case #1.
                return SingleFederationError::InvalidGraphQL {
                    message: format!(
                        r#"{error_message} If you meant the "@{unknown_directive_name}" federation 2 directive, note that this schema is a federation 1 schema. To be a federation 2 schema, it needs to @link to the federation specification v2."#
                    ),
                };
            }

            let Ok(Some(name_in_schema)) = metadata
                .federation_spec_definition()
                .directive_name_in_schema(schema, &Name::new_unchecked(unknown_directive_name))
            else {
                return SingleFederationError::Internal {
                    message: format!(
                        "Unexpectedly could not find directive \"@{unknown_directive_name}\" in schema"
                    ),
                };
            };
            let federation_link_name = &metadata.federation_spec_definition().identity().name;
            let federation_prefix = format!("{federation_link_name}__");
            if name_in_schema.starts_with(&federation_prefix) {
                // Case #2. There is no import for that directive.
                return SingleFederationError::InvalidGraphQL {
                    message: format!(
                        r#"{error_message} If you meant the "@{unknown_directive_name}" federation directive, you should use fully-qualified name "@{name_in_schema}" or add "@{unknown_directive_name}" to the \`import\` argument of the @link to the federation specification."#
                    ),
                };
            } else {
                // Case #3. There's an import, but it's renamed.
                return SingleFederationError::InvalidGraphQL {
                    message: format!(
                        r#"{error_message} If you meant the "@{unknown_directive_name}" federation directive, you should use "@{name_in_schema}" as it is imported under that name in the @link to the federation specification of this schema."#
                    ),
                };
            }
        } else if !is_fed2 {
            // We could get here when a fed1 schema tried to use a fed2 directive but misspelled it.
            let suggestions = suggestion_list(
                unknown_directive_name,
                all_directive_names.iter().map(|name| name.to_string()),
            );
            if !suggestions.is_empty() {
                let did_you_mean = did_you_mean(suggestions.iter().map(|s| format!("@{s}")));
                let note = if suggestions.len() == 1 {
                    "it is a federation 2 directive"
                } else {
                    "they are federation 2 directives"
                };
                return SingleFederationError::InvalidGraphQL {
                    message: format!(
                        "{error_message}{did_you_mean} If so, note that {note} but this schema is a federation 1 one. To be a federation 2 schema, it needs to @link to the federation specification v2."
                    ),
                };
            }
            // fall-through
        }
        SingleFederationError::InvalidGraphQL {
            message: error_message.to_string(),
        }
    }

    fn apply_directives_after_parsing() -> bool {
        true
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
        for feature in schema.all_features()? {
            feature.add_elements_to_schema(schema)?;
        }

        Ok(())
    }
}

pub(crate) const FEDERATION_OPERATION_FIELDS: [Name; 2] = [
    FEDERATION_SERVICE_FIELD_NAME,
    FEDERATION_ENTITIES_FIELD_NAME,
];

fn all_default_federation_directive_names() -> HashSet<Name> {
    FederationSpecDefinition::latest()
        .directive_specs()
        .iter()
        .map(|spec| spec.name().clone())
        .collect()
}
