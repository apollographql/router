use apollo_compiler::Schema;
use apollo_compiler::name;

use crate::LinkSpecDefinition;
use crate::ValidFederationSchema;
use crate::error::FederationError;
use crate::internal_error;
use crate::link::federation_spec_definition::add_fed1_link_to_schema;
use crate::schema::FederationSchema;
use crate::schema::KeyDirective;
use crate::schema::blueprint::FederationBlueprint;
use crate::schema::compute_subgraph_metadata;
use crate::schema::subgraph_metadata::SubgraphMetadata;
use crate::subgraph::SubgraphError;

#[derive(Clone, Debug)]
pub struct Raw {
    schema: Schema,
}

#[derive(Clone, Debug)]
pub struct Expanded {
    schema: FederationSchema,
    metadata: SubgraphMetadata,
}

#[derive(Clone, Debug)]
pub struct Validated {
    schema: ValidFederationSchema,
    metadata: SubgraphMetadata,
}

trait SubgraphMetadataState {
    fn metadata(&self) -> &SubgraphMetadata;
    fn schema(&self) -> &FederationSchema;
}

impl SubgraphMetadataState for Expanded {
    fn metadata(&self) -> &SubgraphMetadata {
        &self.metadata
    }

    fn schema(&self) -> &FederationSchema {
        &self.schema
    }
}

impl SubgraphMetadataState for Validated {
    fn metadata(&self) -> &SubgraphMetadata {
        &self.metadata
    }

    fn schema(&self) -> &FederationSchema {
        &self.schema
    }
}

/// A subgraph represents a schema and its associated metadata. Subgraphs are updated through the
/// composition pipeline, such as when links are expanded or when fed 1 subgraphs are upgraded to fed 2.
/// We aim to encode these state transitions using the [typestate pattern](https://cliffle.com/blog/rust-typestate).
///
/// ```text
///   (expand)     (validate)
/// Raw ──► Expanded ──► Validated
///            ▲             │
///            └────────────┘
///          (mutate/invalidate)
///  ```
///
/// Subgraph states and their invariants:
/// - `Raw`: The initial state, containing a raw schema. This provides no guarantees about the schema, other than
///   that it can be parsed.
/// - `Expanded`: The schema's links have been expanded to include missing directive definitions and subgraph
///   metadata has been computed.
/// - `Validated`: The schema has been validated according to Federation rules. Iterators over directives are
///   infallible at this stage.
#[derive(Clone, Debug)]
pub struct Subgraph<S> {
    pub name: String,
    pub url: String,
    pub state: S,
}

impl Subgraph<Raw> {
    pub fn new(name: &str, url: &str, schema: Schema) -> Subgraph<Raw> {
        Subgraph {
            name: name.to_string(),
            url: url.to_string(),
            state: Raw { schema },
        }
    }

    pub fn parse(
        name: &'static str,
        url: &str,
        schema_str: &str,
    ) -> Result<Subgraph<Raw>, FederationError> {
        let schema = Schema::builder()
            .adopt_orphan_extensions()
            .parse(schema_str, name)
            .build()?;

        Ok(Self::new(name, url, schema))
    }

    pub fn assume_expanded(self) -> Result<Subgraph<Expanded>, FederationError> {
        let schema = FederationSchema::new(self.state.schema)?;
        let metadata = compute_subgraph_metadata(&schema)?.ok_or_else(|| {
            internal_error!(
                "Unable to detect federation version used in subgraph '{}'",
                self.name
            )
        })?;

        Ok(Subgraph {
            name: self.name,
            url: self.url,
            state: Expanded { schema, metadata },
        })
    }

    pub fn expand_links(self) -> Result<Subgraph<Expanded>, FederationError> {
        let mut schema = FederationSchema::new_uninitialized(self.state.schema)?;
        // First, copy types over from the underlying schema AST to make sure we have built-ins that directives may reference
        schema.collect_shallow_references();

        // Backfill missing directive definitions. This is primarily making sure we have a definition for `@link`.
        for directive in &schema.schema().schema_definition.directives.clone() {
            if schema.get_directive_definition(&directive.name).is_none() {
                FederationBlueprint::on_missing_directive_definition(&mut schema, directive)?;
            }
        }

        // If there's a use of `@link`, and we successfully added its definition, add the bootstrap directive
        if schema.get_directive_definition(&name!("link")).is_some() {
            LinkSpecDefinition::latest().add_to_schema(&mut schema, /*alias*/ None)?;
        } else {
            // This must be a Fed 1 schema.
            LinkSpecDefinition::fed1_latest().add_to_schema(&mut schema, /*alias*/ None)?;

            // PORT_NOTE: JS doesn't actually add the 1.0 federation spec link to the schema. In
            //            Rust, we add it, so that fed 1 and fed 2 can be processed the same way.
            add_fed1_link_to_schema(&mut schema)?;
        }

        // Now that we have the definition for `@link` and an application, the bootstrap directive detection should work.
        schema.collect_links_metadata()?;

        FederationBlueprint::on_directive_definition_and_schema_parsed(&mut schema)?;

        // Also, the backfilled definitions mean we can collect deep references.
        schema.collect_deep_references()?;

        // TODO: Remove this and use metadata from this Subgraph instead of FederationSchema
        FederationBlueprint::on_constructed(&mut schema)?;

        let metadata = compute_subgraph_metadata(&schema)?.ok_or_else(|| {
            internal_error!(
                "Unable to detect federation version used in subgraph '{}'",
                self.name
            )
        })?;

        Ok(Subgraph {
            name: self.name,
            url: self.url,
            state: Expanded { schema, metadata },
        })
    }
}

impl Subgraph<Expanded> {
    pub fn upgrade(&mut self) -> Result<Self, SubgraphError> {
        todo!("Implement upgrade logic for expanded subgraphs");
    }

    pub fn validate(
        mut self,
        rename_root_types: bool,
    ) -> Result<Subgraph<Validated>, SubgraphError> {
        let blueprint = FederationBlueprint::new(rename_root_types);
        blueprint
            .on_validation(&mut self.state.schema)
            .map_err(|e| SubgraphError::new(self.name.clone(), e))?;
        let schema = self
            .state
            .schema
            .validate_or_return_self()
            .map_err(|t| SubgraphError::new(self.name.clone(), t.1))?;

        Ok(Subgraph {
            name: self.name,
            url: self.url,
            state: Validated {
                schema,
                metadata: self.state.metadata,
            },
        })
    }

    #[allow(dead_code)]
    pub(crate) fn key_directive_applications(
        &self,
    ) -> Result<Vec<Result<KeyDirective, FederationError>>, FederationError> {
        self.state.schema.key_directive_applications()
    }
}

impl Subgraph<Validated> {
    pub fn invalidate(self) -> Subgraph<Expanded> {
        Subgraph {
            name: self.name,
            url: self.url,
            state: Expanded {
                // Other holders may still need the data in the `Arc`, so we clone the contents to allow mutation later
                schema: (*self.state.schema).clone(),
                metadata: self.state.metadata,
            },
        }
    }

    #[allow(dead_code)]
    pub(crate) fn key_directive_applications(&self) -> Vec<KeyDirective<'_>> {
        todo!("Validated @key directives should be made available after validation")
    }
}

#[allow(private_bounds)]
impl<S: SubgraphMetadataState> Subgraph<S> {
    pub(crate) fn metadata(&self) -> &SubgraphMetadata {
        self.state.metadata()
    }

    pub(crate) fn schema(&self) -> &FederationSchema {
        self.state.schema()
    }
}

#[cfg(test)]
mod tests {
    use apollo_compiler::ast::OperationType;
    use apollo_compiler::name;

    use super::*;

    #[test]
    fn detects_federation_1_subgraphs_correctly() {
        let subgraph = Subgraph::parse(
            "S",
            "",
            r#"
                type Query {
                    s: String
                }"#,
        )
        .expect("valid schema")
        .expand_links()
        .expect("expands subgraph");

        assert!(!subgraph.state.metadata.is_fed_2_schema());
    }

    #[test]
    fn detects_federation_2_subgraphs_correctly() {
        let schema = Subgraph::parse(
            "S",
            "",
            r#"
                extend schema @link(url: "https://specs.apollo.dev/federation/v2.0")

                type Query {
                    s: String
                }"#,
        )
        .expect("valid schema")
        .expand_links()
        .expect("expands subgraph");

        assert!(schema.state.metadata.is_fed_2_schema());
    }

    #[test]
    fn injects_missing_directive_definitions_fed_1_0() {
        let subgraph = Subgraph::parse(
            "S",
            "",
            r#"
                type Query {
                    s: String
                }"#,
        )
        .expect("valid schema")
        .expand_links()
        .expect("expands subgraph");

        let mut defined_directive_names = subgraph
            .state
            .schema
            .schema()
            .directive_definitions
            .keys()
            .cloned()
            .collect::<Vec<_>>();
        defined_directive_names.sort();

        assert_eq!(
            defined_directive_names,
            vec![
                name!("core"),
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
        let subgraph = Subgraph::parse(
            "S",
            "",
            r#"
                extend schema @link(url: "https://specs.apollo.dev/federation/v2.0")

                type Query {
                    s: String
                }"#,
        )
        .expect("valid schema")
        .expand_links()
        .expect("expands subgraph");

        let mut defined_directive_names = subgraph
            .schema()
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
        let subgraph = Subgraph::parse(
            "S",
            "",
            r#"
                extend schema @link(url: "https://specs.apollo.dev/federation/v2.1")

                type Query {
                    s: String
                }"#,
        )
        .expect("valid schema")
        .expand_links()
        .expect("expands subgraph");

        let mut defined_directive_names = subgraph
            .schema()
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
        let subgraph = Subgraph::parse(
            "S",
            "",
            r#"
                directive @key(fields: String) repeatable on OBJECT | INTERFACE
                directive @provides(fields: _FieldSet) repeatable on FIELD_DEFINITION
                directive @requires(fields: FieldSet) repeatable on FIELD_DEFINITION

                scalar _FieldSet
                scalar FieldSet

                type Query {
                    s: String
                }"#,
        )
        .expect("valid schema")
        .expand_links()
        .expect("expands subgraph");

        let key_definition = subgraph
            .schema()
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

    #[test]
    fn rejects_non_root_use_of_default_query_name() {
        let error = Subgraph::parse(
            "S",
            "",
            r#"
            schema {
                query: MyQuery
            }

            type MyQuery {
                f: Int
            }

            type Query {
                g: Int
            }
            "#,
        )
        .expect("parses schema")
        .expand_links()
        .expect("expands links")
        .validate(true)
        .expect_err("fails validation");

        assert_eq!(
            error.to_string(),
            r#"[S] The schema has a type named "Query" but it is not set as the query root type ("MyQuery" is instead): this is not supported by federation. If a root type does not use its default name, there should be no other type with that default name."#
        );
    }

    #[test]
    fn rejects_non_root_use_of_default_mutation_name() {
        let error = Subgraph::parse(
            "S",
            "",
            r#"
            schema {
                mutation: MyMutation
            }

            type MyMutation {
                f: Int
            }

            type Mutation {
                g: Int
            }
            "#,
        )
        .expect("parses schema")
        .expand_links()
        .expect("expands links")
        .validate(true)
        .expect_err("fails validation");

        assert_eq!(
            error.to_string(),
            r#"[S] The schema has a type named "Mutation" but it is not set as the mutation root type ("MyMutation" is instead): this is not supported by federation. If a root type does not use its default name, there should be no other type with that default name."#,
        );
    }

    #[test]
    fn rejects_non_root_use_of_default_subscription_name() {
        let error = Subgraph::parse(
            "S",
            "",
            r#"
            schema {
                subscription: MySubscription
            }

            type MySubscription {
                f: Int
            }

            type Subscription {
                g: Int
            }
            "#,
        )
        .expect("parses schema")
        .expand_links()
        .expect("expands links")
        .validate(true)
        .expect_err("fails validation");

        assert_eq!(
            error.to_string(),
            r#"[S] The schema has a type named "Subscription" but it is not set as the subscription root type ("MySubscription" is instead): this is not supported by federation. If a root type does not use its default name, there should be no other type with that default name."#,
        );
    }

    #[test]
    fn renames_root_operations_to_default_names() {
        let subgraph = Subgraph::parse(
            "S",
            "",
            r#"
            schema {
                query: MyQuery
                mutation: MyMutation
                subscription: MySubscription
            }

            type MyQuery {
                f: Int
            }

            type MyMutation {
                g: Int
            }

            type MySubscription {
                h: Int
            }
            "#,
        )
        .expect("parses schema")
        .expand_links()
        .expect("expands links")
        .validate(true)
        .expect("is valid");

        assert_eq!(
            subgraph
                .state
                .schema
                .schema()
                .root_operation(OperationType::Query),
            Some(name!("Query")).as_ref()
        );
        assert_eq!(
            subgraph
                .state
                .schema
                .schema()
                .root_operation(OperationType::Mutation),
            Some(name!("Mutation")).as_ref()
        );
        assert_eq!(
            subgraph
                .state
                .schema
                .schema()
                .root_operation(OperationType::Subscription),
            Some(name!("Subscription")).as_ref()
        );
    }

    #[test]
    fn does_not_rename_root_operations_when_disabled() {
        let subgraph = Subgraph::parse(
            "S",
            "",
            r#"
            schema {
                query: MyQuery
                mutation: MyMutation
                subscription: MySubscription
            }
    
            type MyQuery {
                f: Int
            }
    
            type MyMutation {
                g: Int
            }
    
            type MySubscription {
                h: Int
            }
            "#,
        )
        .expect("parses schema")
        .expand_links()
        .expect("expands links")
        .validate(false)
        .expect("is valid");

        assert_eq!(
            subgraph
                .state
                .schema
                .schema()
                .root_operation(OperationType::Query),
            Some(name!("MyQuery")).as_ref()
        );
        assert_eq!(
            subgraph
                .state
                .schema
                .schema()
                .root_operation(OperationType::Mutation),
            Some(name!("MyMutation")).as_ref()
        );
        assert_eq!(
            subgraph
                .state
                .schema
                .schema()
                .root_operation(OperationType::Subscription),
            Some(name!("MySubscription")).as_ref()
        );
    }
}
