use std::collections::HashMap;
use std::sync::Arc;

use apollo_compiler::ast::Directive;
use apollo_compiler::validation::Valid;
use apollo_compiler::Name;
use apollo_compiler::Schema;
use carryover::carryover_directives;
use indexmap::IndexMap;
use indexmap::IndexSet;
use itertools::Itertools;

use crate::error::FederationError;
use crate::link::Link;
use crate::merge::merge_subgraphs;
use crate::schema::FederationSchema;
use crate::sources::connect::ConnectSpecDefinition;
use crate::sources::connect::Connector;
use crate::sources::connect::SubgraphConnectorConfiguration;
use crate::subgraph::Subgraph;
use crate::subgraph::ValidSubgraph;
use crate::ApiSchemaOptions;
use crate::Supergraph;
use crate::ValidFederationSubgraph;

mod carryover;
mod visitor;
use visitor::ToSchemaVisitor;

pub struct Connectors {
    pub by_service_name: Arc<IndexMap<Arc<str>, Connector>>,
    pub labels_by_service_name: Arc<IndexMap<Arc<str>, String>>,
}

/// The result of a supergraph expansion of connect-aware subgraphs
pub enum ExpansionResult {
    /// The supergraph had some subgraphs that were expanded
    Expanded {
        raw_sdl: String,
        api_schema: Valid<Schema>,
        connectors: Connectors,
    },

    /// The supergraph contained no connect directives and was unchanged.
    Unchanged,
}

/// Expand a schema with connector directives into unique subgraphs per directive
///
/// Until we have a source-aware query planner, work with connectors will need to interface
/// with standard query planning concepts while still enforcing connector-specific rules. To do so,
/// each connector is separated into its own unique subgraph with relevant GraphQL directives to enforce
/// field dependencies and response structures. This allows for satisfiability and validation to piggy-back
/// off of existing functionality in a reproducable way.
pub fn expand_connectors(
    supergraph_str: &str,
    mut config_per_subgraph: Option<HashMap<String, SubgraphConnectorConfiguration>>,
) -> Result<ExpansionResult, FederationError> {
    // TODO: Don't rely on finding the URL manually to short out
    let connect_url = ConnectSpecDefinition::identity();
    let connect_url = format!("{}/{}/v", connect_url.domain, connect_url.name);
    if !supergraph_str.contains(&connect_url) {
        return Ok(ExpansionResult::Unchanged);
    }

    let supergraph = Supergraph::new(supergraph_str)?;
    let api_schema = supergraph.to_api_schema(ApiSchemaOptions {
        // TODO: Get these options from the router?
        include_defer: true,
        include_stream: false,
    })?;

    let (connect_subgraphs, graphql_subgraphs): (Vec<_>, Vec<_>) = supergraph
        .extract_subgraphs()?
        .into_iter()
        .partition_map(|(_, sub)| {
            match ConnectSpecDefinition::get_from_schema(sub.schema.schema()) {
                Some((_, link)) if contains_connectors(&link, &sub) => {
                    either::Either::Left((link, sub))
                }
                _ => either::Either::Right(ValidSubgraph::from(sub)),
            }
        });

    // Expand just the connector subgraphs
    let connect_subgraphs = connect_subgraphs
        .into_iter()
        .map(|(link, sub)| {
            let config = config_per_subgraph
                .as_mut()
                .and_then(|config| config.remove(&sub.name));
            split_subgraph(&link, sub, config)
        })
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .flatten()
        .collect_vec();

    // Merge the subgraphs into one supergraph
    let all_subgraphs = graphql_subgraphs
        .iter()
        .chain(connect_subgraphs.iter().map(|(_, sub)| sub))
        .collect();
    let new_supergraph = merge_subgraphs(all_subgraphs).map_err(|e| {
        FederationError::internal(format!("could not merge expanded subgraphs: {e:?}"))
    })?;

    let mut new_supergraph = FederationSchema::new(new_supergraph.schema.into_inner())?;
    carryover_directives(&supergraph.schema, &mut new_supergraph)
        .map_err(|_e| FederationError::internal("could not carry over directives"))?;

    let connectors_by_service_name: IndexMap<Arc<str>, Connector> = connect_subgraphs
        .into_iter()
        .map(|(connector, sub)| (sub.name.into(), connector))
        .collect();

    let labels_by_service_name = connectors_by_service_name
        .iter()
        .map(|(service_name, connector)| (service_name.clone(), connector.id.label.clone()))
        .collect();

    Ok(ExpansionResult::Expanded {
        raw_sdl: new_supergraph.schema().serialize().to_string(),
        api_schema: api_schema.schema().clone(),
        connectors: Connectors {
            by_service_name: Arc::new(connectors_by_service_name),
            labels_by_service_name: Arc::new(labels_by_service_name),
        },
    })
}

fn contains_connectors(link: &Link, subgraph: &ValidFederationSubgraph) -> bool {
    let connect_name = ConnectSpecDefinition::connect_directive_name(link);
    let source_name = ConnectSpecDefinition::source_directive_name(link);

    subgraph
        .schema
        .get_directive_definitions()
        .any(|directive| {
            directive.directive_name == connect_name || directive.directive_name == source_name
        })
}

/// Split up a subgraph so that each connector directive becomes its own subgraph.
///
/// Subgraphs passed to this function should contain connector directives.
fn split_subgraph(
    link: &Link,
    subgraph: ValidFederationSubgraph,
    config: Option<SubgraphConnectorConfiguration>,
) -> Result<Vec<(Connector, ValidSubgraph)>, FederationError> {
    let connector_map = Connector::from_valid_schema(&subgraph.schema, &subgraph.name, config)?;

    let expander = helpers::Expander::new(link, &subgraph);
    connector_map
        .into_iter()
        .map(|(id, connector)| {
            // Build a subgraph using only the necessary fields from the directive
            let schema = expander.expand(&connector)?;
            let subgraph = Subgraph::new(
                id.synthetic_name().as_str(),
                &subgraph.url,
                &schema.schema().serialize().to_string(),
            )?;

            // We only validate during debug builds since we should realistically only generate valid schemas
            // for these subgraphs.
            #[cfg(debug_assertions)]
            let schema = subgraph.schema.validate()?;
            #[cfg(not(debug_assertions))]
            let schema = Valid::assume_valid(subgraph.schema);

            Ok((
                connector,
                ValidSubgraph {
                    name: subgraph.name,
                    url: subgraph.url,
                    schema,
                },
            ))
        })
        .try_collect()
}

/// Filter out directives from a directive list
fn filter_directives<'a, D, I, O>(deny_list: &IndexSet<Name>, directives: D) -> O
where
    D: IntoIterator<Item = &'a I>,
    I: 'a + AsRef<Directive> + Clone,
    O: FromIterator<I>,
{
    directives
        .into_iter()
        .filter(|d| !deny_list.contains(&d.as_ref().name))
        .cloned()
        .collect()
}

mod helpers {
    use apollo_compiler::ast;
    use apollo_compiler::ast::Argument;
    use apollo_compiler::ast::Directive;
    use apollo_compiler::ast::FieldDefinition;
    use apollo_compiler::ast::InputValueDefinition;
    use apollo_compiler::ast::Value;
    use apollo_compiler::name;
    use apollo_compiler::schema::Component;
    use apollo_compiler::schema::ComponentName;
    use apollo_compiler::schema::ComponentOrigin;
    use apollo_compiler::schema::DirectiveList;
    use apollo_compiler::schema::ObjectType;
    use apollo_compiler::Name;
    use apollo_compiler::Node;
    use indexmap::IndexMap;
    use indexmap::IndexSet;

    use super::filter_directives;
    use super::ToSchemaVisitor;
    use crate::error::FederationError;
    use crate::link::spec::Identity;
    use crate::link::Link;
    use crate::query_graph::extract_subgraphs_from_supergraph::new_empty_fed_2_subgraph_schema;
    use crate::schema::position::ObjectTypeDefinitionPosition;
    use crate::schema::position::SchemaRootDefinitionKind;
    use crate::schema::position::SchemaRootDefinitionPosition;
    use crate::schema::position::TypeDefinitionPosition;
    use crate::schema::FederationSchema;
    use crate::schema::ObjectFieldDefinitionPosition;
    use crate::schema::ObjectOrInterfaceFieldDefinitionPosition;
    use crate::schema::ValidFederationSchema;
    use crate::sources::connect::json_selection::JSONSelectionVisitor;
    use crate::sources::connect::url_path_template::Parameter;
    use crate::sources::connect::ConnectSpecDefinition;
    use crate::sources::connect::Connector;
    use crate::sources::connect::EntityResolver;
    use crate::sources::connect::JSONSelection;
    use crate::sources::connect::Transport;
    use crate::subgraph::spec::EXTERNAL_DIRECTIVE_NAME;
    use crate::subgraph::spec::KEY_DIRECTIVE_NAME;
    use crate::subgraph::spec::REQUIRES_DIRECTIVE_NAME;
    use crate::ValidFederationSubgraph;

    /// A helper struct for expanding a subgraph into one per connect directive.
    pub(super) struct Expander<'a> {
        /// The name of the connect directive, possibly aliased.
        connect_name: Name,

        /// The name of the connect directive, possibly aliased.
        source_name: Name,

        /// The name of the @key directive, as known in the subgraph
        key_name: Name,

        /// The original schema that contains connect directives
        original_schema: &'a ValidFederationSchema,

        /// A list of directives to exclude when copying over types from the
        /// original schema.
        directive_deny_list: IndexSet<Name>,
    }

    impl<'a> Expander<'a> {
        pub(super) fn new(link: &Link, subgraph: &'a ValidFederationSubgraph) -> Expander<'a> {
            let connect_name = ConnectSpecDefinition::connect_directive_name(link);
            let source_name = ConnectSpecDefinition::source_directive_name(link);

            // When we go to expand all output types, we'll need to make sure that we don't carry over
            // any connect-related directives. The following directives are also special because they
            // influence planning and satisfiability:
            //
            // - @key: derived based on the fields selected
            // - @external: the current approach will only add external fields to the list of keys
            //     if used in the transport. If not used at all, the field marked with this directive
            //     won't even be included in the expanded subgraph, but if it _is_ used then leaving
            //     this directive will result in planning failures.
            // - @requires: the current approach will add required fields to the list of keys for
            //     implicit entities, so it can't stay.
            let key_name = subgraph
                .schema
                .metadata()
                .and_then(|m| m.for_identity(&Identity::federation_identity()))
                .map(|f| f.directive_name_in_schema(&KEY_DIRECTIVE_NAME))
                .unwrap_or(KEY_DIRECTIVE_NAME);
            let extra_excluded = [EXTERNAL_DIRECTIVE_NAME, REQUIRES_DIRECTIVE_NAME]
                .into_iter()
                .map(|d| {
                    subgraph
                        .schema
                        .metadata()
                        .and_then(|m| m.for_identity(&Identity::federation_identity()))
                        .map(|f| f.directive_name_in_schema(&d))
                        .unwrap_or(d)
                });
            let directive_deny_list = IndexSet::from_iter(extra_excluded.chain([
                key_name.clone(),
                connect_name.clone(),
                source_name.clone(),
            ]));

            Self {
                connect_name,
                source_name,
                key_name,
                original_schema: &subgraph.schema,
                directive_deny_list,
            }
        }

        /// Build an expanded subgraph for the supplied connector
        pub(super) fn expand(
            &self,
            connector: &Connector,
        ) -> Result<FederationSchema, FederationError> {
            let mut schema = new_empty_fed_2_subgraph_schema()?;

            // Add the parent type for this connector
            let field = &connector.id.directive.field;
            match field {
                ObjectOrInterfaceFieldDefinitionPosition::Object(o) => {
                    self.expand_object(&mut schema, o, connector)?
                }
                ObjectOrInterfaceFieldDefinitionPosition::Interface(i) => {
                    todo!("not yet done for interface {i}")
                }
            };

            // Add a query root with dummy information if the connect directive is not on a query
            // field.
            let query_alias = self
                .original_schema
                .schema()
                .schema_definition
                .query
                .as_ref()
                .map(|m| m.name.clone())
                .unwrap_or(name!("Query"));
            if field.parent().type_name() != &query_alias {
                self.insert_dummy_query(&mut schema, query_alias)?;
            }

            // Add the query root
            let root = SchemaRootDefinitionPosition {
                root_kind: SchemaRootDefinitionKind::Query,
            };
            root.insert(
                &mut schema,
                ComponentName {
                    origin: ComponentOrigin::Definition,
                    name: name!("Query"),
                },
            )?;

            Ok(schema)
        }

        /// Expand an object into the schema, copying over fields / types specified by the
        /// JSONSelection.
        fn expand_object(
            &self,
            to_schema: &mut FederationSchema,
            object_field: &ObjectFieldDefinitionPosition,
            connector: &Connector,
        ) -> Result<(), FederationError> {
            // Prime the schema for our new output parent
            let parent = object_field.parent();
            let parent_type = parent.get(self.original_schema.schema())?;
            parent.pre_insert(to_schema)?;

            // Process the output type, making sure to carry over just the fields / types
            // needed by the selection.
            let field_def = object_field.get(self.original_schema.schema())?;
            let output_name = field_def.ty.inner_named_type().clone();
            let output_type = self.original_schema.get_type(output_name.clone())?;
            let extended_output_type = output_type.get(self.original_schema.schema())?;

            // If the type is built-in, then there isn't anything that we need to do for the output type
            if !extended_output_type.is_built_in() {
                let visitor = ToSchemaVisitor::new(
                    self.original_schema,
                    to_schema,
                    output_type,
                    extended_output_type,
                    &self.directive_deny_list,
                );

                visitor.walk(&connector.selection)?;
            }

            // Add the object to our schema
            let field_type = FieldDefinition {
                description: field_def.description.clone(),
                name: field_def.name.clone(),
                arguments: field_def.arguments.clone(),
                ty: field_def.ty.clone(),
                directives: filter_directives(&self.directive_deny_list, &field_def.directives),
            };
            let connector_parent_type = ObjectType {
                description: parent_type.description.clone(),
                name: parent_type.name.clone(),
                implements_interfaces: parent_type.implements_interfaces.clone(),
                directives: filter_directives(&self.directive_deny_list, &parent_type.directives),
                fields: IndexMap::from([(
                    field_type.ty.inner_named_type().clone(),
                    Component::new(field_type),
                )]),
            };

            // The input arguments are independent of the actual output type in which they are used, but
            // require access to the field of the output in order to process the inputs. The output type
            // needs to be present in order to modify its keys and other directives, so we split up
            // the processing so that each has what it needs before and after adding the field.
            self.process_inputs(to_schema, connector, &field_def.arguments)?;
            parent.insert(to_schema, Node::new(connector_parent_type))?;
            self.process_outputs(to_schema, connector, parent_type.name.clone(), output_name)?;

            Ok(())
        }

        /// Process all input types
        ///
        /// Inputs can include leaf types as well as custom inputs.
        fn process_inputs(
            &self,
            to_schema: &mut FederationSchema,
            connector: &Connector,
            arguments: &[Node<InputValueDefinition>],
        ) -> Result<(), FederationError> {
            let parameters = match connector.transport {
                Transport::HttpJson(ref http) => http.path_template.parameters().map_err(|e| {
                    FederationError::internal(format!(
                        "could not extract path template parameters: {e}"
                    ))
                })?,
            };

            for parameter in parameters {
                match parameter {
                    // Ensure that input arguments are carried over to the new schema, including
                    // any types associated with them.
                    Parameter::Argument { argument, .. } => {
                        // Get the argument type
                        let arg = arguments
                            .iter()
                            .find(|a| a.name.as_str() == argument)
                            .ok_or(FederationError::internal("could not find argument"))?;
                        let arg_type = self
                            .original_schema
                            .get_type(arg.ty.inner_named_type().clone())?;
                        let extended_arg_type = arg_type.get(self.original_schema.schema())?;

                        // If the input type isn't built in (and hasn't been copied over yet), then we
                        // need to carry it over.
                        //
                        // TODO: We need to drill down the input type and make sure that all of its field
                        // types are also carried over.
                        if !extended_arg_type.is_built_in()
                            && arg_type.try_get(to_schema.schema()).is_none()
                        {
                            match (arg_type, extended_arg_type) {
                                // Input objects are special because it would be invalid to not carry
                                // over the entire input type since intersection merging rules dictate
                                // that subgraphs with different views of the same input type would
                                // not carry over the union of those types, but rather the intersection.
                                //
                                // So we just copy it blindly.
                                (
                                    TypeDefinitionPosition::InputObject(pos),
                                    apollo_compiler::schema::ExtendedType::InputObject(def),
                                ) => {
                                    pos.pre_insert(to_schema)?;
                                    pos.insert(to_schema, def.clone())?;
                                }

                                // Leaf types are simple: just insert
                                (
                                    TypeDefinitionPosition::Scalar(pos),
                                    apollo_compiler::schema::ExtendedType::Scalar(def),
                                ) => {
                                    pos.pre_insert(to_schema)?;
                                    pos.insert(to_schema, def.clone())?;
                                }
                                (
                                    TypeDefinitionPosition::Enum(pos),
                                    apollo_compiler::schema::ExtendedType::Enum(def),
                                ) => {
                                    pos.pre_insert(to_schema)?;
                                    pos.insert(to_schema, def.clone())?;
                                }

                                (pos, def) => {
                                    return Err(FederationError::internal(format!(
                                        "did not expect to process in input: {pos:?} {def:?}"
                                    )));
                                }
                            };
                        }
                    }

                    // Any other parameter type is not from an input
                    _ => continue,
                }
            }

            Ok(())
        }

        fn process_outputs(
            &self,
            to_schema: &mut FederationSchema,
            connector: &Connector,
            parent_type_name: Name,
            output_type_name: Name,
        ) -> Result<(), FederationError> {
            let parent_type = self.original_schema.get_type(parent_type_name)?;
            let output_type = to_schema.get_type(output_type_name)?;

            let parameters = match connector.transport {
                Transport::HttpJson(ref http) => http.path_template.parameters().map_err(|e| {
                    FederationError::internal(format!(
                        "could not extract path template parameters: {e}"
                    ))
                })?,
            };

            // We'll need to collect all synthesized keys for the output type, adding a federation
            // `@key` directive once completed.
            let mut keys = Vec::new();
            for parameter in parameters {
                match parameter {
                    // Arguments should be added to the synthesized key, since they are mandatory
                    // to resolving the output type. The synthesized key should only include the portions
                    // of the inputs actually used throughout the selections of the transport.
                    //
                    // Note that this only applies to connectors marked as an entity resolver, since only
                    // those should be allowed to fully resolve a type given the required arguments /
                    // synthesized keys.
                    Parameter::Argument { argument, paths } => {
                        // Synthesize the key based on the argument. Note that this is only relevant in the
                        // argument case when the connector is marked as being an entity resolved.
                        if matches!(connector.entity_resolver, Some(EntityResolver::Explicit)) {
                            let mut key = argument.to_string();
                            if !paths.is_empty() {
                                // Slight hack to generate nested { a { b { c } } }
                                let sub_selection = {
                                    let mut s = format!("{{ {}", paths.join(" { "));
                                    s.push_str(&" }".repeat(paths.len()));

                                    s
                                };

                                key.push(' ');
                                key.push_str(&sub_selection);
                            }

                            keys.push(key);
                        }
                    }

                    // All sibling fields marked by $this in a transport must be carried over to the output type
                    // regardless of its use in the output selection.
                    Parameter::Sibling { field, paths } => {
                        match parent_type {
                            TypeDefinitionPosition::Object(ref o) => {
                                let field_name = Name::new(field)?;
                                let field = o.field(field_name.clone());
                                let field_def = field.get(self.original_schema.schema())?;

                                // Mark it as a required key for the output type
                                let mut key = field_name.to_string();
                                if !paths.is_empty() {
                                    // Slight hack to generate nested { a { b { c } } }
                                    let sub_selection = {
                                        let mut s = paths.join(" { ").to_string();
                                        s.push_str(&" }".repeat(paths.len() - 1));

                                        s
                                    };

                                    key.push_str(&format!("{{ {sub_selection} }}"));

                                    // We'll also need to carry over the output type of this sibling if there is a sub
                                    // selection.
                                    let field_output = field_def.ty.inner_named_type();
                                    if to_schema.try_get_type(field_output.clone()).is_none() {
                                        let field_output =
                                            self.original_schema.get_type(field_output.clone())?;
                                        let field_type =
                                            field_output.get(self.original_schema.schema())?;

                                        // We use a fake JSONSelection here so that we can reuse the visitor
                                        // when generating the output types and their required memebers.
                                        let visitor = ToSchemaVisitor::new(
                                            self.original_schema,
                                            to_schema,
                                            field_output,
                                            field_type,
                                            &self.directive_deny_list,
                                        );
                                        let (_, parsed) =
                                            JSONSelection::parse(&sub_selection).map_err(|e| {
                                                FederationError::internal(format!("could not parse fake selection for sibling field: {e}"))
                                            })?;

                                        visitor.walk(&parsed)?;
                                    }
                                }

                                keys.push(key);

                                // Add the field if not already present in the output schema
                                if field.try_get(to_schema.schema()).is_none() {
                                    field.insert(
                                        to_schema,
                                        Component::new(FieldDefinition {
                                            description: field_def.description.clone(),
                                            name: field_def.name.clone(),
                                            arguments: field_def.arguments.clone(),
                                            ty: field_def.ty.clone(),
                                            directives: filter_directives(
                                                &self.directive_deny_list,
                                                &field_def.directives,
                                            ),
                                        }),
                                    )?;
                                }
                            }
                            TypeDefinitionPosition::Interface(_) => todo!(),
                            TypeDefinitionPosition::Union(_) => todo!(),
                            TypeDefinitionPosition::InputObject(_) => todo!(),

                            other => {
                                return Err(FederationError::internal(format!(
                                    "cannot select a sibling on a leaf type: {other:#?}"
                                )))
                            }
                        };
                    }
                }
            }

            // If we have marked keys as being necessary for this output type, add them as an `@key`
            // directive now.
            if !keys.is_empty() {
                let key_directive = Directive {
                    name: self.key_name.clone(),
                    arguments: vec![Node::new(Argument {
                        name: name!("fields"),
                        value: Node::new(Value::String(keys.join(" "))),
                    })],
                };

                let key_for_type =
                    if matches!(connector.entity_resolver, Some(EntityResolver::Explicit)) {
                        output_type
                    } else {
                        parent_type
                    };
                match key_for_type {
                    TypeDefinitionPosition::Object(o) => {
                        o.insert_directive(to_schema, Component::new(key_directive))
                    }

                    TypeDefinitionPosition::Scalar(_) => todo!(),
                    TypeDefinitionPosition::Interface(_) => todo!(),
                    TypeDefinitionPosition::Union(_) => todo!(),
                    TypeDefinitionPosition::Enum(_) => todo!(),
                    TypeDefinitionPosition::InputObject(_) => todo!(),
                }?;
            }

            Ok(())
        }

        /// Inserts a dummy root-level query to the schema to pass validation.
        ///
        /// The inserted dummy query looks like the schema below:
        /// ```graphql
        /// type Query {
        ///   _: ID @shareable @inaccessible
        /// }
        /// ```
        ///
        /// Note: This would probably be better off expanding the query to have
        /// an __entries vs. adding an inaccessible field.
        fn insert_dummy_query(
            &self,
            to_schema: &mut FederationSchema,
            name: Name,
        ) -> Result<(), FederationError> {
            let field_name = name!("_");

            let dummy_query = ObjectTypeDefinitionPosition {
                type_name: name.clone(),
            };
            let dummy_field = Component::new(FieldDefinition {
                description: None,
                name: field_name.clone(),
                arguments: Vec::new(),
                ty: ast::Type::Named(ast::NamedType::new_unchecked("ID")),
                directives: ast::DirectiveList(
                    [
                        name!("federation__shareable"),
                        name!("federation__inaccessible"),
                    ]
                    .into_iter()
                    .map(|name| {
                        Node::new(ast::Directive {
                            name,
                            arguments: Vec::new(),
                        })
                    })
                    .collect(),
                ),
            });

            dummy_query.pre_insert(to_schema)?;
            dummy_query.insert(
                to_schema,
                Node::new(ObjectType {
                    description: None,
                    name,
                    implements_interfaces: IndexSet::new(),
                    directives: DirectiveList::new(),
                    fields: IndexMap::from([(field_name, dummy_field)]),
                }),
            )?;

            Ok(())
        }
    }
}

#[cfg(test)]
mod tests;
