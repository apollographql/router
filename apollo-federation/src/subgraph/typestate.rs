use std::collections::HashMap;

use apollo_compiler::Name;
use apollo_compiler::Node;
use apollo_compiler::Schema;
use apollo_compiler::ast;
use apollo_compiler::ast::OperationType;
use apollo_compiler::collections::IndexSet;
use apollo_compiler::schema::Component;
use apollo_compiler::schema::ComponentName;
use apollo_compiler::schema::Directive;
use apollo_compiler::schema::Type;

use crate::LinkSpecDefinition;
use crate::ValidFederationSchema;
use crate::bail;
use crate::ensure;
use crate::error::FederationError;
use crate::error::MultipleFederationErrors;
use crate::error::SingleFederationError;
use crate::internal_error;
use crate::link::DEFAULT_LINK_NAME;
use crate::link::federation_spec_definition::FED_1;
use crate::link::federation_spec_definition::FEDERATION_EXTENDS_DIRECTIVE_NAME_IN_SPEC;
use crate::link::federation_spec_definition::FEDERATION_FROM_CONTEXT_DIRECTIVE_NAME_IN_SPEC;
use crate::link::federation_spec_definition::FEDERATION_KEY_DIRECTIVE_NAME_IN_SPEC;
use crate::link::federation_spec_definition::FEDERATION_OVERRIDE_DIRECTIVE_NAME_IN_SPEC;
use crate::link::federation_spec_definition::FEDERATION_PROVIDES_DIRECTIVE_NAME_IN_SPEC;
use crate::link::federation_spec_definition::FEDERATION_REQUIRES_DIRECTIVE_NAME_IN_SPEC;
use crate::link::federation_spec_definition::FEDERATION_TAG_DIRECTIVE_NAME_IN_SPEC;
use crate::link::federation_spec_definition::FEDERATION_VERSIONS;
use crate::link::federation_spec_definition::FederationSpecDefinition;
use crate::link::inaccessible_spec_definition::INACCESSIBLE_DIRECTIVE_NAME_IN_SPEC;
use crate::link::link_spec_definition::LINK_DIRECTIVE_IMPORT_ARGUMENT_NAME;
use crate::link::link_spec_definition::LINK_DIRECTIVE_URL_ARGUMENT_NAME;
use crate::link::spec::Identity;
use crate::link::spec::Version;
use crate::link::spec_definition::SpecDefinition;
use crate::schema::FederationSchema;
use crate::schema::SchemaElement;
use crate::schema::blueprint::FederationBlueprint;
use crate::schema::compute_subgraph_metadata;
use crate::schema::position::ObjectFieldDefinitionPosition;
use crate::schema::position::ObjectOrInterfaceTypeDefinitionPosition;
use crate::schema::position::SchemaRootDefinitionKind;
use crate::schema::position::SchemaRootDefinitionPosition;
use crate::schema::position::TypeDefinitionPosition;
use crate::schema::subgraph_metadata::SubgraphMetadata;
use crate::schema::type_and_directive_specification::FieldSpecification;
use crate::schema::type_and_directive_specification::ResolvedArgumentSpecification;
use crate::schema::type_and_directive_specification::TypeAndDirectiveSpecification;
use crate::schema::type_and_directive_specification::UnionTypeSpecification;
use crate::subgraph::SubgraphError;
use crate::supergraph::ANY_TYPE_SPEC;
use crate::supergraph::EMPTY_QUERY_TYPE_SPEC;
use crate::supergraph::FEDERATION_ANY_TYPE_NAME;
use crate::supergraph::FEDERATION_ENTITIES_FIELD_NAME;
use crate::supergraph::FEDERATION_ENTITY_TYPE_NAME;
use crate::supergraph::FEDERATION_REPRESENTATIONS_ARGUMENTS_NAME;
use crate::supergraph::FEDERATION_SERVICE_FIELD_NAME;
use crate::supergraph::GRAPHQL_MUTATION_TYPE_NAME;
use crate::supergraph::GRAPHQL_QUERY_TYPE_NAME;
use crate::supergraph::GRAPHQL_SUBSCRIPTION_TYPE_NAME;
use crate::supergraph::SERVICE_TYPE_SPEC;

#[derive(Clone, Debug)]
pub struct Initial {
    schema: Schema,
}

#[derive(Clone, Debug)]
pub struct Expanded {
    schema: FederationSchema,
    metadata: SubgraphMetadata,
}

#[derive(Clone, Debug)]
pub struct Upgraded {
    schema: FederationSchema,
    metadata: SubgraphMetadata,
}

#[derive(Clone, Debug)]
pub struct Validated {
    schema: ValidFederationSchema,
    metadata: SubgraphMetadata,
}

pub(crate) trait HasMetadata {
    fn metadata(&self) -> &SubgraphMetadata;
    fn schema(&self) -> &FederationSchema;
}

impl HasMetadata for Expanded {
    fn metadata(&self) -> &SubgraphMetadata {
        &self.metadata
    }

    fn schema(&self) -> &FederationSchema {
        &self.schema
    }
}

impl HasMetadata for Upgraded {
    fn metadata(&self) -> &SubgraphMetadata {
        &self.metadata
    }

    fn schema(&self) -> &FederationSchema {
        &self.schema
    }
}

impl HasMetadata for Validated {
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
///      (expand)                  (validate)
/// Initial ──► Expanded ──► Upgraded ──► Validated
///                       ▲            │
///                       └────────────┘
///                   (mutate/invalidate)
///  ```
///
/// Subgraph states and their invariants:
/// - `Initial`: The initial state, containing original schema. This provides no guarantees about the schema,
///   other than that it can be parsed.
/// - `Expanded`: The schema's links have been expanded to include missing directive definitions and subgraph
///   metadata has been computed.
/// - `Upgraded`: The schema has been upgraded to Federation v2 format (if starting with Fed v2 schema then this is no-op).
/// - `Validated`: The schema has been validated according to Federation rules. Iterators over directives are
///   infallible at this stage.
#[derive(Clone, Debug)]
pub struct Subgraph<S> {
    pub name: String,
    pub url: String,
    pub state: S,
}

impl Subgraph<Initial> {
    pub fn new(name: &str, url: &str, schema: Schema) -> Subgraph<Initial> {
        Subgraph {
            name: name.to_string(),
            url: url.to_string(),
            state: Initial { schema },
        }
    }

    pub fn parse(
        name: &str,
        url: &str,
        schema_str: &str,
    ) -> Result<Subgraph<Initial>, SubgraphError> {
        let schema = Schema::builder()
            .adopt_orphan_extensions()
            .ignore_builtin_redefinitions()
            .parse(schema_str, name)
            .build()
            .map_err(|e| SubgraphError::from_diagnostic_list(name, e.errors))?;

        Ok(Self::new(name, url, schema))
    }

    /// Converts the schema to a fed2 schema.
    /// - It is assumed to have no `@link` to the federation spec.
    /// - Returns an equivalent subgraph with a `@link` to the auto expanded federation spec.
    /// - This is mainly for testing and not optimized.
    // PORT_NOTE: Corresponds to `asFed2SubgraphDocument` function in JS, but simplified.
    pub fn into_fed2_test_subgraph(self, use_latest: bool) -> Result<Self, SubgraphError> {
        let mut schema = self.state.schema;
        let federation_spec = if use_latest {
            FederationSpecDefinition::latest()
        } else {
            FederationSpecDefinition::auto_expanded_federation_spec()
        };
        add_federation_link_to_test_schema(&mut schema, federation_spec.version())
            .map_err(|e| SubgraphError::new_without_locations(self.name.clone(), e))?;
        Ok(Self::new(&self.name, &self.url, schema))
    }

    pub fn assume_expanded(self) -> Result<Subgraph<Expanded>, SubgraphError> {
        let schema = FederationSchema::new(self.state.schema)
            .map_err(|e| SubgraphError::new_without_locations(self.name.clone(), e))?;
        let metadata = compute_subgraph_metadata(&schema)
            .and_then(|m| {
                m.ok_or_else(|| {
                    internal_error!(
                        "Unable to detect federation version used in subgraph '{}'",
                        self.name
                    )
                })
            })
            .map_err(|e| SubgraphError::new_without_locations(self.name.clone(), e))?;

        Ok(Subgraph {
            name: self.name,
            url: self.url,
            state: Expanded { schema, metadata },
        })
    }

    pub fn expand_links(self) -> Result<Subgraph<Expanded>, SubgraphError> {
        tracing::debug!("expand_links: expand subgraph `{}`", self.name);
        let subgraph_name = self.name.clone();
        self.expand_links_internal()
            .map_err(|e| SubgraphError::new_without_locations(subgraph_name, e))
    }

    fn expand_links_internal(self) -> Result<Subgraph<Expanded>, FederationError> {
        let schema = expand_schema(self.state.schema)?;
        tracing::debug!("expand_links: compute_subgraph_metadata");
        let metadata = compute_subgraph_metadata(&schema)?.ok_or_else(|| {
            internal_error!(
                "Unable to detect federation version used in subgraph '{}'",
                self.name
            )
        })?;

        tracing::debug!("expand_links: finished");
        Ok(Subgraph {
            name: self.name,
            url: self.url,
            state: Expanded { schema, metadata },
        })
    }
}

impl Subgraph<Expanded> {
    pub fn assume_upgraded(self) -> Subgraph<Upgraded> {
        Subgraph {
            name: self.name,
            url: self.url,
            state: Upgraded {
                schema: self.state.schema,
                metadata: self.state.metadata,
            },
        }
    }
}

impl Subgraph<Upgraded> {
    pub fn assume_validated(self) -> Result<Subgraph<Validated>, SubgraphError> {
        let valid_federation_schema = ValidFederationSchema::new_assume_valid(self.state.schema)
            .map_err(|(_schema, error)| {
                SubgraphError::new_without_locations(self.name.clone(), error)
            })?;
        Ok(Subgraph {
            name: self.name,
            url: self.url,
            state: Validated {
                schema: valid_federation_schema,
                metadata: self.state.metadata,
            },
        })
    }

    pub fn validate(self) -> Result<Subgraph<Validated>, SubgraphError> {
        let schema = self
            .state
            .schema
            .validate_or_return_self()
            .map_err(|(schema, err)| {
                // Specialize GraphQL validation errors.
                let iter = err.into_errors().into_iter().map(|err| match err {
                    SingleFederationError::InvalidGraphQL { message } => {
                        FederationBlueprint::on_invalid_graphql_error(&schema, message)
                    }
                    _ => err,
                });
                SubgraphError::new_without_locations(
                    self.name.clone(),
                    MultipleFederationErrors::from_iter(iter),
                )
            })?;

        FederationBlueprint::on_validation(&schema, &self.state.metadata)
            .map_err(|e| SubgraphError::new_without_locations(self.name.clone(), e))?;

        Ok(Subgraph {
            name: self.name,
            url: self.url,
            state: Validated {
                schema,
                metadata: self.state.metadata,
            },
        })
    }

    pub fn normalize_root_types(&mut self) -> Result<(), SubgraphError> {
        let mut operation_types_to_rename = HashMap::new();
        for (op_type, op_name) in self
            .state
            .schema
            .schema()
            .schema_definition
            .iter_root_operations()
        {
            let default_name = default_operation_name(&op_type);
            if op_name.name != default_name {
                operation_types_to_rename.insert(op_name.name.clone(), default_name.clone());
                if self
                    .state
                    .schema
                    .try_get_type(default_name.clone())
                    .is_some()
                {
                    // TODO: Add locations
                    return Err(SubgraphError::new_without_locations(
                        self.name.clone(),
                        SingleFederationError::root_already_used(
                            op_type,
                            default_name,
                            op_name.name.clone(),
                        ),
                    ));
                }
            }
        }
        for (current_name, new_name) in operation_types_to_rename {
            self.state
                .schema
                .get_type(current_name)
                .map_err(|e| SubgraphError::new_without_locations(self.name.clone(), e))?
                .rename(&mut self.state.schema, new_name)
                .map_err(|e| SubgraphError::new_without_locations(self.name.clone(), e))?;
        }

        Ok(())
    }
}

fn default_operation_name(op_type: &OperationType) -> Name {
    match op_type {
        OperationType::Query => GRAPHQL_QUERY_TYPE_NAME,
        OperationType::Mutation => GRAPHQL_MUTATION_TYPE_NAME,
        OperationType::Subscription => GRAPHQL_SUBSCRIPTION_TYPE_NAME,
    }
}

impl Subgraph<Validated> {
    pub fn validated_schema(&self) -> &ValidFederationSchema {
        &self.state.schema
    }

    pub fn invalidate(self) -> Subgraph<Upgraded> {
        // PORT_NOTE: In JS, the metadata gets invalidated by calling
        // `federationMetadata.onInvalidate` (via `FederationBlueprint.onValidation`). But, it
        // doesn't seem necessary in Rust, since the metadata is computed eagerly.
        Subgraph {
            name: self.name,
            url: self.url,
            state: Upgraded {
                // Other holders may still need the data in the `Arc`, so we clone the contents to allow mutation later
                schema: (*self.state.schema).clone(),
                metadata: self.state.metadata,
            },
        }
    }
}

#[allow(private_bounds)]
impl<S: HasMetadata> Subgraph<S> {
    pub(crate) fn metadata(&self) -> &SubgraphMetadata {
        self.state.metadata()
    }

    pub(crate) fn schema(&self) -> &FederationSchema {
        self.state.schema()
    }

    /// Returns the schema as a string. Mainly for testing purposes.
    pub fn schema_string(&self) -> String {
        self.schema().schema().to_string()
    }

    pub(crate) fn extends_directive_name(&self) -> Result<Option<Name>, FederationError> {
        self.metadata()
            .federation_spec_definition()
            .directive_name_in_schema(self.schema(), &FEDERATION_EXTENDS_DIRECTIVE_NAME_IN_SPEC)
    }

    #[allow(clippy::wrong_self_convention)]
    pub(crate) fn from_context_directive_name(&self) -> Result<Option<Name>, FederationError> {
        self.metadata()
            .federation_spec_definition()
            .directive_name_in_schema(
                self.schema(),
                &FEDERATION_FROM_CONTEXT_DIRECTIVE_NAME_IN_SPEC,
            )
    }

    pub(crate) fn inaccessible_directive_name(&self) -> Result<Option<Name>, FederationError> {
        self.metadata()
            .federation_spec_definition()
            .directive_name_in_schema(self.schema(), &INACCESSIBLE_DIRECTIVE_NAME_IN_SPEC)
    }

    pub(crate) fn key_directive_name(&self) -> Result<Option<Name>, FederationError> {
        self.metadata()
            .federation_spec_definition()
            .directive_name_in_schema(self.schema(), &FEDERATION_KEY_DIRECTIVE_NAME_IN_SPEC)
    }

    pub(crate) fn override_directive_name(&self) -> Result<Option<Name>, FederationError> {
        self.metadata()
            .federation_spec_definition()
            .directive_name_in_schema(self.schema(), &FEDERATION_OVERRIDE_DIRECTIVE_NAME_IN_SPEC)
    }

    pub(crate) fn provides_directive_name(&self) -> Result<Option<Name>, FederationError> {
        self.metadata()
            .federation_spec_definition()
            .directive_name_in_schema(self.schema(), &FEDERATION_PROVIDES_DIRECTIVE_NAME_IN_SPEC)
    }

    pub(crate) fn requires_directive_name(&self) -> Result<Option<Name>, FederationError> {
        self.metadata()
            .federation_spec_definition()
            .directive_name_in_schema(self.schema(), &FEDERATION_REQUIRES_DIRECTIVE_NAME_IN_SPEC)
    }

    pub(crate) fn tag_directive_name(&self) -> Result<Option<Name>, FederationError> {
        self.metadata()
            .federation_spec_definition()
            .directive_name_in_schema(self.schema(), &FEDERATION_TAG_DIRECTIVE_NAME_IN_SPEC)
    }

    pub(crate) fn is_interface_object_type(&self, type_: &TypeDefinitionPosition) -> bool {
        let Ok(Some(interface_object)) = self
            .metadata()
            .federation_spec_definition()
            .interface_object_directive_definition(self.schema())
        else {
            return false;
        };
        if let TypeDefinitionPosition::Object(obj) = type_ {
            let interface_object_referencers = self
                .schema()
                .referencers()
                .get_directive(&interface_object.name);
            return interface_object_referencers.is_ok_and(|refs| refs.object_types.contains(obj));
        }
        false
    }
}

/// Adds a federation (v2 or above) link directive to the schema.
/// - Similar to `add_fed1_link_to_schema` & `schema_as_fed2_subgraph`, but the link can be added
///   before collecting metadata.
/// - This is mainly for testing.
fn add_federation_link_to_test_schema(
    schema: &mut Schema,
    federation_version: &Version,
) -> Result<(), FederationError> {
    let federation_spec = FEDERATION_VERSIONS
        .find(federation_version)
        .ok_or_else(|| internal_error!(
            "Subgraph unexpectedly does not use a supported federation spec version. Requested version: {}",
            federation_version,
        ))?;

    // Insert `@link(url: "http://specs.apollo.dev/federation/vX.Y", import: ...)`.
    // - auto import all directives.
    let imports: Vec<_> = federation_spec
        .directive_specs()
        .iter()
        .map(|d| format!("@{}", d.name()).into())
        .collect();

    schema
        .schema_definition
        .make_mut()
        .directives
        .push(Component::new(Directive {
            name: Identity::link_identity().name,
            arguments: vec![
                Node::new(ast::Argument {
                    name: LINK_DIRECTIVE_URL_ARGUMENT_NAME,
                    value: federation_spec.url().to_string().into(),
                }),
                Node::new(ast::Argument {
                    name: LINK_DIRECTIVE_IMPORT_ARGUMENT_NAME,
                    value: Node::new(ast::Value::List(imports)),
                }),
            ],
        }));
    Ok(())
}

/// Turns a schema without a federation spec link into a federation 1 subgraph schema.
/// - Adds a fed 1 spec link directive to the schema.
fn add_fed1_link_to_schema(
    schema: &mut FederationSchema,
    link_spec: &LinkSpecDefinition,
    link_name_in_schema: Name,
) -> Result<(), FederationError> {
    // Insert `@core(feature: "http://specs.apollo.dev/federation/v1.0")` directive (or a `@link`
    // directive, if applicable) to the schema definition.
    // We can't use `import` argument here since fed1 @core does not support `import`.
    // We will add imports later (see `fed1_link_imports`).
    let directive = Directive {
        name: link_name_in_schema,
        arguments: vec![Node::new(ast::Argument {
            name: link_spec.url_arg_name(),
            value: FED_1.url().to_string().into(),
        })],
    };
    let origin = schema.schema().schema_definition.origin_to_use();
    crate::schema::position::SchemaDefinitionPosition.insert_directive(
        schema,
        Component {
            origin,
            node: directive.into(),
        },
    )
}

/// Turns a schema without a federation spec link into a federation 2 subgraph schema.
/// - The schema must not have a federation spec. But, it may have a link spec.
/// - This is used for fed1-to-fed2 schema upgrading.
/// - Also, it is used by `new_empty_federation_2_subgraph_schema`.
// PORT_NOTE: This corresponds to the `setSchemaAsFed2Subgraph` function in JS.
//            The inner Schema is not exposed as mutable at the moment. So, this function consumes
//            the input and returns the updated inner Schema.
pub(crate) fn schema_as_fed2_subgraph(
    mut schema: FederationSchema,
    use_latest: bool,
) -> Result<Schema, FederationError> {
    let (link_name_in_schema, metadata) = if let Some(metadata) = schema.metadata() {
        let link_spec = metadata.link_spec_definition()?;
        // We don't accept pre-1.0 @core: this avoid having to care about what the name
        // of the argument below is, and why would be bother?
        ensure!(
            link_spec
                .url()
                .version
                .satisfies(LinkSpecDefinition::latest().version()),
            "Fed2 schema must use @link with version >= 1.0, but schema uses {spec_url}",
            spec_url = link_spec.url()
        );
        let Some(link) = link_spec.link_in_schema(&schema)? else {
            bail!("Core schema is missing the link spec link directive");
        };
        (link.spec_name_in_schema().clone(), metadata)
    } else {
        let link_spec = LinkSpecDefinition::latest();
        let link_name_in_schema = add_link_spec_to_schema(&mut schema, link_spec)?;
        schema.collect_links_metadata()?;
        let Some(metadata) = schema.metadata() else {
            bail!("Schema should now be a core schema")
        };
        (link_name_in_schema, metadata)
    };

    let fed_spec = if use_latest {
        FederationSpecDefinition::latest()
    } else {
        FederationSpecDefinition::auto_expanded_federation_spec()
    };
    ensure!(
        metadata.for_identity(fed_spec.identity()).is_none(),
        "Schema already set as a federation subgraph"
    );

    // Insert `@link(url: "http://specs.apollo.dev/federation/vX.Y", import: ...)`.
    // - auto import certain directives.
    // Note that there is a mismatch between url and directives that are imported. This is because
    // we want to maintain backward compatibility for those who have already upgraded and we had
    // been upgrading the url to latest, but we never automatically import directives that exist
    // past 2.4
    let imports: Vec<_> = FederationSpecDefinition::auto_expanded_federation_spec()
        .directive_specs()
        .iter()
        .map(|d| format!("@{}", d.name()).into())
        .collect();

    // PORT_NOTE: We are adding the fed spec link to the schema definition unconditionally, not
    //            considering extensions. This seems consistent with the JS version. But, it's
    //            not consistent with the `add_to_schema`'s behavior. We may change to use the
    //            `schema_definition.origin_to_use()` method in the future.
    let mut inner_schema = schema.into_inner();
    inner_schema
        .schema_definition
        .make_mut()
        .directives
        .push(Component::new(Directive {
            name: link_name_in_schema,
            arguments: vec![
                Node::new(ast::Argument {
                    name: LINK_DIRECTIVE_URL_ARGUMENT_NAME,
                    value: fed_spec.url().to_string().into(),
                }),
                Node::new(ast::Argument {
                    name: LINK_DIRECTIVE_IMPORT_ARGUMENT_NAME,
                    value: Node::new(ast::Value::List(imports)),
                }),
            ],
        }));
    Ok(inner_schema)
}

/// Returns a suitable alias for a named directive.
/// - Returns Some with an unused directive name.
/// - Returns None, if aliasing is unnecessary.
// PORT_NOTE: This corresponds to the `findUnusedNamedForLinkDirective` function in JS.
fn find_unused_name_for_directive(
    schema: &FederationSchema,
    directive_name: &Name,
) -> Result<Option<Name>, FederationError> {
    if schema.get_directive_definition(directive_name).is_none() {
        return Ok(None);
    }

    // The schema already defines a directive named `@link` so we need to use an alias. To keep it
    // simple, we add a number in the end (so we try `@link1`, and if that's taken `@link2`, ...)
    for i in 1..=1000 {
        let candidate = Name::try_from(format!("{directive_name}{i}"))?;
        if schema.get_directive_definition(&candidate).is_none() {
            return Ok(Some(candidate));
        }
    }
    // We couldn't find one that is not used.
    Err(internal_error!(
        "Unable to find a name for the link directive",
    ))
}

// PORT_NOTE: This corresponds to the Schema's constructor in JS.
fn new_federation_subgraph_schema(
    inner_schema: Schema,
) -> Result<FederationSchema, FederationError> {
    let mut schema = FederationSchema::new_uninitialized(inner_schema)?;

    // First, copy types over from the underlying schema AST to make sure we have built-ins that directives may reference
    tracing::debug!("new_federation_subgraph_schema: collect_shallow_references");
    schema.collect_shallow_references();

    // Backfill missing directive definitions. This is primarily making sure we have a definition for `@link`.
    // Note: Unlike `@core`, `@link` doesn't have to be defined in the schema.
    tracing::debug!("new_federation_subgraph_schema: missing directive definitions");
    for directive in &schema.schema().schema_definition.directives.clone() {
        if schema.get_directive_definition(&directive.name).is_none() {
            FederationBlueprint::on_missing_directive_definition(&mut schema, directive)?;
        }
    }

    // Now that we have the definition for `@link`, the bootstrap directive detection should work.
    tracing::debug!("new_federation_subgraph_schema: collect_links_metadata");
    schema.collect_links_metadata()?;

    Ok(schema)
}

// PORT_NOTE: This corresponds to the `newEmptyFederation2Schema` function in JS.
#[allow(unused)]
pub(crate) fn new_empty_federation_2_subgraph_schema() -> Result<Schema, FederationError> {
    let mut schema = new_federation_subgraph_schema(Schema::new())?;
    schema_as_fed2_subgraph(schema, true)
}

/// Expands schema with all imported federation definitions.
pub(crate) fn expand_schema(schema: Schema) -> Result<FederationSchema, FederationError> {
    let mut schema = new_federation_subgraph_schema(schema)?;

    // If there's a use of `@link` and we successfully added its definition, add the bootstrap directive
    tracing::debug!("expand_links: bootstrap_spec_links");
    bootstrap_spec_links(&mut schema)?;

    tracing::debug!("expand_links: on_directive_definition_and_schema_parsed");
    FederationBlueprint::on_directive_definition_and_schema_parsed(&mut schema)?;

    // Also, the backfilled definitions mean we can collect deep references.
    // Ignore the error case, which means the schema has invalid references. It will be
    // reported later in the validation phase.
    tracing::debug!("expand_links: collect_deep_references");
    _ = schema.collect_deep_references();

    // TODO: Remove this and use metadata from this Subgraph instead of FederationSchema
    tracing::debug!("expand_links: on_constructed");
    FederationBlueprint::on_constructed(&mut schema)?;

    // PORT_NOTE: JS version calls `addFederationOperations` in the `validate` method.
    //            It seems to make sense for it to be a part of expansion stage. We can create
    //            a separate stage for it between `Expanded` and `Validated` if we need a stage
    //            that is expanded, but federation operations are not added.
    tracing::debug!("expand_links: add_federation_operations");
    schema.add_federation_operations()?;
    Ok(schema)
}

/// Bootstrap link spec and federation spec links.
/// - Make sure the schema has a link spec definition & link.
/// - Make sure the schema has a federation spec link.
// PORT_NOTE: This partially corresponds to the `completeSubgraphSchema` function in JS.
fn bootstrap_spec_links(schema: &mut FederationSchema) -> Result<(), FederationError> {
    // PORT_NOTE: JS version calls `completeFed1SubgraphSchema` and `completeFed2SubgraphSchema`
    //            here. In Rust, we don't have them, since
    //            `on_directive_definition_and_schema_parsed` method handles it. Also, while JS
    //            version doesn't actually add implicit fed1 spec links to the schema, Rust version
    //            add it, so that fed 1 and fed 2 can be processed the same way in the method.

    #[allow(clippy::collapsible_else_if)]
    if let Some(metadata) = schema.metadata() {
        // The schema has a @core or @link spec directive.
        if schema.is_fed_2() {
            tracing::debug!("bootstrap_spec_links: metadata indicates fed2");
        } else {
            // This must be a Fed 1 schema.
            tracing::debug!("bootstrap_spec_links: metadata indicates fed1");
            if metadata
                .for_identity(&Identity::federation_identity())
                .is_none()
            {
                // Federation spec is not present. Implicitly add the fed 1 spec.
                let link_spec = metadata.link_spec_definition()?;
                let link_name_in_schema = metadata
                    .for_identity(link_spec.identity())
                    .map(|link| link.spec_name_in_schema().clone())
                    .unwrap_or_else(|| link_spec.identity().name.clone());
                add_fed1_link_to_schema(schema, link_spec, link_name_in_schema)?;
            }
        }
    } else {
        // The schemas has no link metadata.
        if has_federation_spec_link(schema.schema()) {
            // Has a federation spec link, but no link spec itself. Add the latest link spec.
            // Since `@link` directive is present, this must be a fed 2 schema.
            tracing::debug!(
                "bootstrap_spec_links: has a federation spec without a link spec itself"
            );
            LinkSpecDefinition::latest().add_to_schema(schema, /*alias*/ None)?;
        } else {
            // This must be a Fed 1 schema with no link/federation spec.
            // Implicitly add the link spec and federation spec to the schema.
            tracing::debug!("bootstrap_spec_links: has no link/federation spec");
            let link_spec = LinkSpecDefinition::fed1_latest();
            // PORT_NOTE: JS version doesn't add link specs here, (maybe) due to a potential name
            //            conflict. We generate an alias to avoid conflicts, if necessary.
            let link_name_in_schema = add_link_spec_to_schema(schema, link_spec)?;
            add_fed1_link_to_schema(schema, link_spec, link_name_in_schema)?;
        }
    }
    Ok(())
}

/// Add `@link` (or `@core` if fed1) to the `schema` definition.
/// - Potentially, alias the directive name to avoid conflicts.
/// - Returns the determined link directive name in schema.
fn add_link_spec_to_schema(
    schema: &mut FederationSchema,
    link_spec: &'static LinkSpecDefinition,
) -> Result<Name, FederationError> {
    let link_spec_name = &link_spec.identity().name;
    let alias = find_unused_name_for_directive(schema, link_spec_name)?;
    let link_name_in_schema = alias.clone().unwrap_or_else(|| link_spec_name.clone());
    link_spec.add_to_schema(schema, alias)?;
    Ok(link_name_in_schema)
}

fn has_federation_spec_link(schema: &Schema) -> bool {
    schema
        .schema_definition
        .directives
        .iter()
        .any(|d| is_fed_spec_link_directive(schema, d))
}

fn is_fed_spec_link_directive(schema: &Schema, directive: &Directive) -> bool {
    if directive.name != DEFAULT_LINK_NAME {
        return false;
    }
    let Ok(url_arg) = directive.argument_by_name(&LINK_DIRECTIVE_URL_ARGUMENT_NAME, schema) else {
        return false;
    };
    url_arg
        .as_str()
        .is_some_and(|url| url.starts_with(&Identity::federation_identity().domain))
}

impl FederationSchema {
    fn add_federation_operations(&mut self) -> Result<(), FederationError> {
        // Add federation operation types
        // PORT_NOTE: The JS version ignores errors from these check-or-add calls.
        //            (https://github.com/apollographql/federation/blob/e17173bf9e7b3fdee42a9ee0ac4bd269de67e374/internals-js/src/federation.ts#L2505)
        //            Many corpus subgraphs have `_Entity` definitions that do not exactly match
        //            the one computed by composition. Reporting error here will break them.
        _ = ANY_TYPE_SPEC.check_or_add(self, None);
        _ = SERVICE_TYPE_SPEC.check_or_add(self, None);
        _ = self.entity_type_spec()?.check_or_add(self, None);

        // Add the root `Query` Type (if not already present) and get the actual name in the schema.
        let query_root_pos = SchemaRootDefinitionPosition {
            root_kind: SchemaRootDefinitionKind::Query,
        };
        let query_root_type_name = if query_root_pos.try_get(self.schema()).is_none() {
            // If not present, add the default Query type with empty fields.
            EMPTY_QUERY_TYPE_SPEC.check_or_add(self, None)?;
            query_root_pos.insert(self, ComponentName::from(EMPTY_QUERY_TYPE_SPEC.name))?;
            EMPTY_QUERY_TYPE_SPEC.name
        } else {
            query_root_pos.get(self.schema())?.name.clone()
        };

        // Add or remove `Query._entities` (if applicable)
        let entity_field_pos = ObjectFieldDefinitionPosition {
            type_name: query_root_type_name.clone(),
            field_name: FEDERATION_ENTITIES_FIELD_NAME,
        };
        if let Some(_entity_type) = self.entity_type()? {
            if entity_field_pos.try_get(self.schema()).is_none() {
                entity_field_pos
                    .insert(self, Component::new(self.entities_field_spec()?.into()))?;
            }
            // PORT_NOTE: JS version checks if the entity field definition's type is null when the
            //            definition is found, but the `type` field is not nullable in Rust.
        } else {
            // Remove the `_entities` field if it is present
            // PORT_NOTE: It's unclear why this is necessary. Maybe it's to avoid schema confusion?
            entity_field_pos.remove(self)?;
        }

        // Add `Query._service` (if not already present)
        let service_field_pos = ObjectFieldDefinitionPosition {
            type_name: query_root_type_name,
            field_name: FEDERATION_SERVICE_FIELD_NAME,
        };
        if service_field_pos.try_get(self.schema()).is_none() {
            service_field_pos.insert(self, Component::new(self.service_field_spec()?.into()))?;
        }

        Ok(())
    }

    // Constructs the `_Entity` type spec for the subgraph schema.
    // PORT_NOTE: Corresponds to the `entityTypeSpec` constant definition.
    fn entity_type_spec(&self) -> Result<UnionTypeSpecification, FederationError> {
        // Please note that `_Entity` cannot use "interface entities" since interface types cannot
        // be in unions. It is ok in practice because _Entity is only use as return type for
        // `_entities`, and even when interfaces are involve, the result of an `_entities` call
        // will always be an object type anyway, and since we force all implementations of an
        // interface entity to be entity themselves in a subgraph, we're fine.
        let mut entity_members = IndexSet::default();
        for key_directive_app in self.key_directive_applications()?.into_iter() {
            let key_directive_app = key_directive_app?;
            let target = key_directive_app.target();
            if let ObjectOrInterfaceTypeDefinitionPosition::Object(obj_ty) = target {
                entity_members.insert(ComponentName::from(&obj_ty.type_name));
            }
        }

        Ok(UnionTypeSpecification {
            name: FEDERATION_ENTITY_TYPE_NAME,
            members: Box::new(move |_| entity_members.clone()),
        })
    }

    fn representations_arguments_field_spec() -> ResolvedArgumentSpecification {
        ResolvedArgumentSpecification {
            name: FEDERATION_REPRESENTATIONS_ARGUMENTS_NAME,
            ty: Type::NonNullList(Box::new(Type::NonNullNamed(FEDERATION_ANY_TYPE_NAME))),
            default_value: None,
        }
    }

    fn entities_field_spec(&self) -> Result<FieldSpecification, FederationError> {
        let Some(entity_type) = self.entity_type()? else {
            bail!("The federation entity type is expected to be defined, but not found")
        };
        Ok(FieldSpecification {
            name: FEDERATION_ENTITIES_FIELD_NAME,
            ty: Type::NonNullList(Box::new(Type::Named(entity_type.type_name))),
            arguments: vec![Self::representations_arguments_field_spec()],
        })
    }

    fn service_field_spec(&self) -> Result<FieldSpecification, FederationError> {
        Ok(FieldSpecification {
            name: FEDERATION_SERVICE_FIELD_NAME,
            ty: Type::NonNullNamed(self.service_type()?.type_name),
            arguments: vec![],
        })
    }
}

#[cfg(test)]
mod tests {
    use apollo_compiler::ast::OperationType;
    use apollo_compiler::name;

    use super::*;
    use crate::subgraph::test_utils::build_and_validate;
    use crate::subgraph::test_utils::build_for_errors;

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
                name!("tag"),
            ]
        );
    }

    #[test]
    fn implicit_fed1_link_does_not_add_import_type() {
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

        let mut defined_type_names = subgraph
            .state
            .schema
            .schema()
            .types
            .keys()
            .filter(|k| k.starts_with("core__"))
            .cloned()
            .collect::<Vec<_>>();
        defined_type_names.sort();

        assert_eq!(defined_type_names, vec![name!("core__Purpose")]);
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
                name!("federation__extends"),
                name!("federation__external"),
                name!("federation__inaccessible"),
                name!("federation__key"),
                name!("federation__override"),
                name!("federation__provides"),
                name!("federation__requires"),
                name!("federation__shareable"),
                name!("federation__tag"),
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
                name!("federation__extends"),
                name!("federation__external"),
                name!("federation__inaccessible"),
                name!("federation__key"),
                name!("federation__override"),
                name!("federation__provides"),
                name!("federation__requires"),
                name!("federation__shareable"),
                name!("federation__tag"),
                name!("include"),
                name!("link"),
                name!("skip"),
                name!("specifiedBy"),
            ]
        );
    }

    #[test]
    fn injects_missing_directive_definitions_fed_2_12() {
        let subgraph = Subgraph::parse(
            "S",
            "",
            r#"
                extend schema @link(url: "https://specs.apollo.dev/federation/v2.12")

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
                name!("federation__authenticated"),
                name!("federation__cacheTag"),
                name!("federation__composeDirective"),
                name!("federation__context"),
                name!("federation__cost"),
                name!("federation__extends"),
                name!("federation__external"),
                name!("federation__fromContext"),
                name!("federation__inaccessible"),
                name!("federation__interfaceObject"),
                name!("federation__key"),
                name!("federation__listSize"),
                name!("federation__override"),
                name!("federation__policy"),
                name!("federation__provides"),
                name!("federation__requires"),
                name!("federation__requiresScopes"),
                name!("federation__shareable"),
                name!("federation__tag"),
                name!("include"),
                name!("link"),
                name!("skip"),
                name!("specifiedBy")
            ]
        );
    }

    #[test]
    fn injects_missing_directive_definitions_connect_v0_1() {
        let subgraph = Subgraph::parse(
            "S",
            "",
            r#"
                extend schema @link(url: "https://specs.apollo.dev/federation/v2.10") @link(url: "https://specs.apollo.dev/connect/v0.1")

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
                name!("connect"),
                name!("connect__source"),
                name!("deprecated"),
                name!("federation__authenticated"),
                name!("federation__composeDirective"),
                name!("federation__context"),
                name!("federation__cost"),
                name!("federation__extends"),
                name!("federation__external"),
                name!("federation__fromContext"),
                name!("federation__inaccessible"),
                name!("federation__interfaceObject"),
                name!("federation__key"),
                name!("federation__listSize"),
                name!("federation__override"),
                name!("federation__policy"),
                name!("federation__provides"),
                name!("federation__requires"),
                name!("federation__requiresScopes"),
                name!("federation__shareable"),
                name!("federation__tag"),
                name!("include"),
                name!("link"),
                name!("skip"),
                name!("specifiedBy"),
            ]
        );

        let mut defined_type_names = subgraph
            .schema()
            .schema()
            .types
            .keys()
            .cloned()
            .collect::<Vec<_>>();
        defined_type_names.sort();

        assert_eq!(
            defined_type_names,
            vec![
                name!("Boolean"),
                name!("Float"),
                name!("ID"),
                name!("Int"),
                name!("Query"),
                name!("String"),
                name!("_Any"),
                name!("_Service"),
                name!("__Directive"),
                name!("__DirectiveLocation"),
                name!("__EnumValue"),
                name!("__Field"),
                name!("__InputValue"),
                name!("__Schema"),
                name!("__Type"),
                name!("__TypeKind"),
                name!("connect__ConnectBatch"),
                name!("connect__ConnectHTTP"),
                name!("connect__ConnectorErrors"),
                name!("connect__HTTPHeaderMapping"),
                name!("connect__JSONSelection"),
                name!("connect__SourceHTTP"),
                name!("connect__URLTemplate"),
                name!("federation__ContextFieldValue"),
                name!("federation__FieldSet"),
                name!("federation__Policy"),
                name!("federation__Scope"),
                name!("link__Import"),
                name!("link__Purpose"),
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
        let errors = build_for_errors(
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
        );

        assert_eq!(errors.len(), 1);
        assert_eq!(
            errors[0].1,
            r#"[S] The schema has a type named "Query" but it is not set as the query root type ("MyQuery" is instead): this is not supported by federation. If a root type does not use its default name, there should be no other type with that default name."#
        );
    }

    #[test]
    fn rejects_non_root_use_of_default_mutation_name() {
        let errors = build_for_errors(
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
        );

        assert_eq!(errors.len(), 1);
        assert_eq!(
            errors[0].1,
            r#"[S] The schema has a type named "Mutation" but it is not set as the mutation root type ("MyMutation" is instead): this is not supported by federation. If a root type does not use its default name, there should be no other type with that default name."#,
        );
    }

    #[test]
    fn rejects_non_root_use_of_default_subscription_name() {
        let errors = build_for_errors(
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
        );

        assert_eq!(errors.len(), 1);
        assert_eq!(
            errors[0].1,
            r#"[S] The schema has a type named "Subscription" but it is not set as the subscription root type ("MySubscription" is instead): this is not supported by federation. If a root type does not use its default name, there should be no other type with that default name."#,
        );
    }

    #[test]
    fn renames_root_operations_to_default_names() {
        let subgraph = build_and_validate(
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
        );

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
        .assume_upgraded()
        .validate()
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

    #[test]
    fn allows_duplicate_imports_within_same_link() {
        // This test used to panic.
        let schema_doc = r#"
          extend schema @link(url: "https://specs.apollo.dev/federation/v2.5", import: ["@key" "@key"])
          type Query { test: Int! }
        "#;
        Subgraph::parse("subgraph", "subgraph.graphql", schema_doc)
            .expect("parses schema")
            .expand_links()
            .expect("expands links");
    }

    #[test]
    fn ignores_unexpected_custom_entity_type_spec() {
        // This test used to panic.
        // The `_Entity` type is not expected to be defined, but defined.
        let schema_doc = r#"
            union _Entity = Int
        "#;
        Subgraph::parse("subgraph", "subgraph.graphql", schema_doc)
            .expect("parses schema")
            .expand_links()
            .expect("expands links");
    }

    #[test]
    fn ignores_custom_entity_type_spec_even_when_incorrectly_defined() {
        // This test used to panic.
        // When `_Entity` type is expected to be defined, but defined incorrectly.
        let schema_doc = r#"
            type X {
                data: Int!
            }

            type Y @key(fields: "id") {
                id: ID!
            }

            union _Entity = X

            type Query {
                test: X
            }
        "#;
        Subgraph::parse("subgraph", "subgraph.graphql", schema_doc)
            .expect("parses schema")
            .expand_links()
            .expect("expands links");
    }
}
