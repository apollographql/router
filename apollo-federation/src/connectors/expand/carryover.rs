mod inputs;

use apollo_compiler::Name;
use apollo_compiler::Node;
use apollo_compiler::ast::Argument;
use apollo_compiler::ast::Directive;
use apollo_compiler::ast::Value;
use apollo_compiler::collections::HashMap;
use apollo_compiler::collections::HashSet;
use apollo_compiler::name;
use inputs::copy_input_types;
use multimap::MultiMap;

use crate::connectors::ConnectSpec;
use crate::error::FederationError;
use crate::link::DEFAULT_LINK_NAME;
use crate::link::Link;
use crate::link::inaccessible_spec_definition::INACCESSIBLE_DIRECTIVE_NAME_IN_SPEC;
use crate::link::spec::APOLLO_SPEC_DOMAIN;
use crate::link::spec::Identity;
use crate::schema::FederationSchema;
use crate::schema::position::DirectiveArgumentDefinitionPosition;
use crate::schema::position::DirectiveDefinitionPosition;
use crate::schema::position::EnumTypeDefinitionPosition;
use crate::schema::position::EnumValueDefinitionPosition;
use crate::schema::position::InputObjectFieldDefinitionPosition;
use crate::schema::position::InputObjectTypeDefinitionPosition;
use crate::schema::position::InterfaceFieldArgumentDefinitionPosition;
use crate::schema::position::InterfaceFieldDefinitionPosition;
use crate::schema::position::InterfaceTypeDefinitionPosition;
use crate::schema::position::ObjectFieldArgumentDefinitionPosition;
use crate::schema::position::ObjectFieldDefinitionPosition;
use crate::schema::position::ObjectOrInterfaceFieldDefinitionPosition;
use crate::schema::position::ObjectOrInterfaceFieldDirectivePosition;
use crate::schema::position::ObjectTypeDefinitionPosition;
use crate::schema::position::ScalarTypeDefinitionPosition;
use crate::schema::position::SchemaDefinitionPosition;
use crate::schema::position::UnionTypeDefinitionPosition;
use crate::schema::referencer::DirectiveReferencers;

const TAG_DIRECTIVE_NAME_IN_SPEC: Name = name!("tag");
const AUTHENTICATED_DIRECTIVE_NAME_IN_SPEC: Name = name!("authenticated");
const REQUIRES_SCOPES_DIRECTIVE_NAME_IN_SPEC: Name = name!("requiresScopes");
const POLICY_DIRECTIVE_NAME_IN_SPEC: Name = name!("policy");
const COST_DIRECTIVE_NAME_IN_SPEC: Name = name!("cost");
const LIST_SIZE_DIRECTIVE_NAME_IN_SPEC: Name = name!("listSize");
const CONTEXT_DIRECTIVE_NAME_IN_SPEC: Name = name!("context");
const JOIN_DIRECTIVE_DIRECTIVE_NAME: Name = name!("join__directive");

pub(super) fn carryover_directives(
    from: &FederationSchema,
    to: &mut FederationSchema,
    specs: impl Iterator<Item = ConnectSpec>,
    subgraph_name_replacements: &MultiMap<&str, String>,
    connect_directive_names: HashMap<String, [Name; 2]>,
) -> Result<(), FederationError> {
    let Some(metadata) = from.metadata() else {
        return Ok(());
    };

    let from_join_graph_enum = from
        .schema()
        .get_enum(&name!(join__Graph))
        .ok_or_else(|| FederationError::internal("Cannot find join__graph enum"))?;
    let to_join_graph_enum = to
        .schema()
        .get_enum(&name!(join__Graph))
        .ok_or_else(|| FederationError::internal("Cannot find join__graph enum"))?;
    let subgraph_enum_replacements = inputs::subgraph_replacements(
        from_join_graph_enum,
        to_join_graph_enum,
        subgraph_name_replacements,
    )
    .map_err(|e| FederationError::internal(format!("Failed to get subgraph replacements: {e}")))?;

    let from_subgraph_names: HashMap<_, _> =
        inputs::subgraph_names_to_enum_values(from_join_graph_enum)
            .map_err(|e| FederationError::internal(format!("Failed to get subgraph names: {e}")))?
            .into_iter()
            .map(|(k, v)| (k, v.as_str()))
            .collect();
    let connect_directive_names: HashMap<&str, [Name; 2]> = connect_directive_names
        .into_iter()
        .flat_map(|(subgraph, v)| {
            from_subgraph_names
                .get(subgraph.as_str())
                .map(|name| (*name, v))
        })
        .collect();

    // @join__directive(graph: [], name: "link", args: { url: "https://specs.apollo.dev/connect/v0.1" })
    // this must exist for license key enforcement
    for spec in specs {
        SchemaDefinitionPosition.insert_directive(to, spec.join_directive_application().into())?;
    }

    // @link for connect
    if let Some(link) = metadata.for_identity(&ConnectSpec::identity()) {
        SchemaDefinitionPosition.insert_directive(to, link.to_directive_application().into())?;
    }

    // before copying over directive definitions, we need to ensure we copy over
    // any input types (scalars, enums, input objects) they use
    copy_input_types(from, to, &subgraph_enum_replacements)?;

    // @inaccessible

    if let Some(link) = metadata.for_identity(&Identity::inaccessible_identity()) {
        let directive_name = link.directive_name_in_schema(&INACCESSIBLE_DIRECTIVE_NAME_IN_SPEC);
        from.referencers()
            .get_directive(&directive_name)
            .and_then(|referencers| {
                // because the merge code handles inaccessible, we have to check if the
                // @link and directive definition are already present in the schema
                if referencers.len() > 0
                    && to
                        .metadata()
                        .and_then(|m| m.by_identity.get(&Identity::inaccessible_identity()))
                        .is_none()
                {
                    SchemaDefinitionPosition
                        .insert_directive(to, link.to_directive_application().into())?;
                    copy_directive_definition(from, to, directive_name.clone())?;
                }
                referencers.copy_directives(from, to, &directive_name)
            })?;
    }

    // @tag

    if let Some(link) = metadata.for_identity(&Identity {
        domain: APOLLO_SPEC_DOMAIN.to_string(),
        name: TAG_DIRECTIVE_NAME_IN_SPEC,
    }) {
        let directive_name = link.directive_name_in_schema(&TAG_DIRECTIVE_NAME_IN_SPEC);
        from.referencers()
            .get_directive(&directive_name)
            .and_then(|referencers| {
                if referencers.len() > 0 {
                    SchemaDefinitionPosition
                        .insert_directive(to, link.to_directive_application().into())?;
                    copy_directive_definition(from, to, directive_name.clone())?;
                }
                referencers.copy_directives(from, to, &directive_name)
            })?;
    }

    // @authenticated

    if let Some(link) = metadata.for_identity(&Identity {
        domain: APOLLO_SPEC_DOMAIN.to_string(),
        name: AUTHENTICATED_DIRECTIVE_NAME_IN_SPEC,
    }) {
        let directive_name = link.directive_name_in_schema(&AUTHENTICATED_DIRECTIVE_NAME_IN_SPEC);
        from.referencers()
            .get_directive(&directive_name)
            .and_then(|referencers| {
                if referencers.len() > 0 {
                    SchemaDefinitionPosition
                        .insert_directive(to, link.to_directive_application().into())?;
                    copy_directive_definition(from, to, directive_name.clone())?;
                }
                referencers.copy_directives(from, to, &directive_name)
            })?;
    }

    // @requiresScopes

    if let Some(link) = metadata.for_identity(&Identity {
        domain: APOLLO_SPEC_DOMAIN.to_string(),
        name: REQUIRES_SCOPES_DIRECTIVE_NAME_IN_SPEC,
    }) {
        let directive_name = link.directive_name_in_schema(&REQUIRES_SCOPES_DIRECTIVE_NAME_IN_SPEC);
        from.referencers()
            .get_directive(&directive_name)
            .and_then(|referencers| {
                if referencers.len() > 0 {
                    SchemaDefinitionPosition
                        .insert_directive(to, link.to_directive_application().into())?;

                    copy_directive_definition(from, to, directive_name.clone())?;
                }
                referencers.copy_directives(from, to, &directive_name)
            })?;
    }

    // @policy

    if let Some(link) = metadata.for_identity(&Identity {
        domain: APOLLO_SPEC_DOMAIN.to_string(),
        name: POLICY_DIRECTIVE_NAME_IN_SPEC,
    }) {
        let directive_name = link.directive_name_in_schema(&POLICY_DIRECTIVE_NAME_IN_SPEC);
        from.referencers()
            .get_directive(&directive_name)
            .and_then(|referencers| {
                if referencers.len() > 0 {
                    SchemaDefinitionPosition
                        .insert_directive(to, link.to_directive_application().into())?;

                    copy_directive_definition(from, to, directive_name.clone())?;
                }
                referencers.copy_directives(from, to, &directive_name)
            })?;
    }

    // @cost

    if let Some(link) = metadata.for_identity(&Identity {
        domain: APOLLO_SPEC_DOMAIN.to_string(),
        name: COST_DIRECTIVE_NAME_IN_SPEC,
    }) {
        let mut insert_link = false;

        let directive_name = link.directive_name_in_schema(&COST_DIRECTIVE_NAME_IN_SPEC);
        from.referencers()
            .get_directive(&directive_name)
            .and_then(|referencers| {
                if referencers.len() > 0 {
                    insert_link = true;
                    copy_directive_definition(from, to, directive_name.clone())?;
                }
                referencers.copy_directives(from, to, &directive_name)
            })?;

        let directive_name = link.directive_name_in_schema(&LIST_SIZE_DIRECTIVE_NAME_IN_SPEC);
        from.referencers()
            .get_directive(&directive_name)
            .and_then(|referencers| {
                if referencers.len() > 0 {
                    insert_link = true;
                    copy_directive_definition(from, to, directive_name.clone())?;
                }
                referencers.copy_directives(from, to, &directive_name)
            })?;

        if insert_link {
            SchemaDefinitionPosition
                .insert_directive(to, link.to_directive_application().into())?;
        }
    }

    // compose directive

    metadata
        .directives_by_imported_name
        .iter()
        .filter(|(_name, (link, _import))| !is_known_link(link))
        .try_for_each(|(name, (link, import))| {
            // This is a strange thing — someone is importing @defer, but it's not a type system directive so we don't need to carry it over
            if name == "defer" {
                return Ok(());
            }
            let directive_name = link.directive_name_in_schema(&import.element);
            from.referencers()
                .get_directive(&directive_name)
                .and_then(|referencers| {
                    if referencers.len() > 0 {
                        if !SchemaDefinitionPosition
                            .get(to.schema())
                            .directives
                            .iter()
                            .any(|d| {
                                d.name == DEFAULT_LINK_NAME
                                    && d.specified_argument_by_name("url")
                                        .and_then(|url| url.as_str())
                                        .map(|url| link.url.to_string() == *url)
                                        .unwrap_or_default()
                            })
                        {
                            SchemaDefinitionPosition
                                .insert_directive(to, link.to_directive_application().into())?;
                        }

                        copy_directive_definition(from, to, directive_name.clone())?;
                    }
                    referencers.copy_directives(from, to, &directive_name)
                })?;
            Ok::<_, FederationError>(())
        })?;

    // @context

    if let Some(link) = metadata.for_identity(&Identity {
        domain: APOLLO_SPEC_DOMAIN.to_string(),
        name: CONTEXT_DIRECTIVE_NAME_IN_SPEC,
    }) {
        let mut insert_link = false;

        let directive_name = link.directive_name_in_schema(&CONTEXT_DIRECTIVE_NAME_IN_SPEC);
        from.referencers()
            .get_directive(&directive_name)
            .and_then(|referencers| {
                if referencers.len() > 0 {
                    insert_link = true;
                    copy_directive_definition(from, to, directive_name.clone())?;
                }
                referencers.copy_directives(from, to, &directive_name)
            })?;

        if insert_link {
            SchemaDefinitionPosition
                .insert_directive(to, link.to_directive_application().into())?;
        }
    }

    // @join__field(contextArguments: ...)
    // This is a special case where we need to copy a specific argument from
    // join__field directives in the original supergraph over to matching (by
    // graph: arguments) join__field directives in the new schema. This is to
    // avoid recreating the logic for constructing the contextArguments
    // argument. This works because @fromContext is not allowed in connector
    // subgraphs, so we can always directly carry over argument values.
    if let Ok(referencers) = from.referencers().get_directive("join__field") {
        let fields = referencers
            .object_fields
            .iter()
            .map(|pos| ObjectOrInterfaceFieldDefinitionPosition::Object(pos.clone()))
            .chain(
                referencers
                    .interface_fields
                    .iter()
                    .map(|pos| ObjectOrInterfaceFieldDefinitionPosition::Interface(pos.clone())),
            )
            .filter_map(|pos| {
                let field_def = pos.get(from.schema()).ok()?;
                let applications = field_def
                    .directives
                    .iter()
                    .filter(|d| d.name == name!("join__field"))
                    .collect::<Vec<_>>();
                Some((pos, applications))
            })
            .flat_map(|(pos, applications)| {
                applications
                    .into_iter()
                    .map(move |application| (pos.clone(), application))
            })
            .filter_map(|(pos, application)| {
                let argument = application
                    .arguments
                    .iter()
                    .find(|arg| arg.name == name!("contextArguments"))?
                    .clone();
                let graph = application
                    .arguments
                    .iter()
                    .find(|arg| arg.name == name!("graph"))
                    .and_then(|arg| arg.value.as_enum())?
                    .to_string();
                Some((pos, graph, argument))
            });

        for (pos, graph, argument) in fields {
            let field = pos.get(to.schema())?;
            let directive_index = field
                .directives
                .iter()
                .position(|d| {
                    d.name == name!("join__field")
                        && d.arguments.iter().any(|a| {
                            a.name == name!("graph")
                                && a.value.as_enum().map(|e| e.to_string()).unwrap_or_default()
                                    == graph
                        })
                })
                .ok_or_else(|| {
                    FederationError::internal("Cannot find matching directive in new supergraph")
                })?;

            let argument_names = argument
                .value
                .as_list()
                .map(|list| list.iter().flat_map(|v| v.as_object()).flatten())
                .map(|pairs| {
                    pairs
                        .filter(|(name, _)| name == &name!("name"))
                        .flat_map(|(_, value)| value.as_str())
                        .flat_map(|s| Name::new(s).ok())
                        .collect::<HashSet<_>>()
                })
                .ok_or_else(|| {
                    FederationError::internal("Cannot find `name` argument in `contextArguments`")
                })?;

            ObjectOrInterfaceFieldDirectivePosition {
                field: pos.clone(),
                directive_name: name!("join__field"),
                directive_index,
            }
            .add_argument(to, argument)?;

            for argument_name in argument_names {
                // Remove the argument now that it's handled by `@join__field(contextArguments:)`
                match &pos {
                    ObjectOrInterfaceFieldDefinitionPosition::Object(pos) => {
                        ObjectFieldArgumentDefinitionPosition {
                            type_name: pos.type_name.clone(),
                            field_name: pos.field_name.clone(),
                            argument_name,
                        }
                        .remove(to)?;
                    }
                    ObjectOrInterfaceFieldDefinitionPosition::Interface(pos) => {
                        InterfaceFieldArgumentDefinitionPosition {
                            type_name: pos.type_name.clone(),
                            field_name: pos.field_name.clone(),
                            argument_name,
                        }
                        .remove(to)?;
                    }
                }
            }
        }
    };

    // @join__directive
    if let Ok(referencers) = from
        .referencers()
        .get_directive(&JOIN_DIRECTIVE_DIRECTIVE_NAME)
    {
        referencers.copy_join_directive_directives(
            from,
            to,
            &subgraph_enum_replacements,
            &connect_directive_names,
        )?;
    }

    Ok(())
}

fn is_known_link(link: &Link) -> bool {
    link.url.identity.domain == APOLLO_SPEC_DOMAIN
        && [
            name!(link),
            name!(join),
            name!(tag),
            name!(inaccessible),
            name!(authenticated),
            name!(requiresScopes),
            name!(policy),
            name!(context),
        ]
        .contains(&link.url.identity.name)
}

fn copy_directive_definition(
    from: &FederationSchema,
    to: &mut FederationSchema,
    directive_name: Name,
) -> Result<(), FederationError> {
    let def_pos = DirectiveDefinitionPosition { directive_name };

    // If it exists, remove it so we can add the directive as defined in the
    // supergraph. In rare cases where a directive can be applied to both
    // executable and type system locations, extract_subgraphs_from_supergraph
    // will include the definition with only the executable locations, making
    // other applications invalid.
    if def_pos.get(to.schema()).is_ok() {
        def_pos.remove(to)?;
    }

    def_pos
        .get(from.schema())
        .map_err(From::from)
        .and_then(|def| {
            def_pos.pre_insert(to)?;
            def_pos.insert(to, def.clone())
        })
}

impl Link {
    fn to_directive_application(&self) -> Directive {
        let mut arguments: Vec<Node<Argument>> = vec![
            Argument {
                name: name!(url),
                value: self.url.to_string().into(),
            }
            .into(),
        ];

        // purpose: link__Purpose
        if let Some(purpose) = &self.purpose {
            arguments.push(
                Argument {
                    name: name!(for),
                    value: Value::Enum(purpose.into()).into(),
                }
                .into(),
            );
        }

        // as: String
        if let Some(alias) = &self.spec_alias {
            arguments.push(
                Argument {
                    name: name!(as),
                    value: Value::String(alias.to_string()).into(),
                }
                .into(),
            );
        }

        // import: [link__Import!]
        if !self.imports.is_empty() {
            arguments.push(
                Argument {
                    name: name!(import),
                    value: Value::List(
                        self.imports
                            .iter()
                            .map(|i| {
                                let name = if i.is_directive {
                                    format!("@{}", i.element)
                                } else {
                                    i.element.to_string()
                                };

                                if let Some(alias) = &i.alias {
                                    let alias = if i.is_directive {
                                        format!("@{alias}")
                                    } else {
                                        alias.to_string()
                                    };

                                    Value::Object(vec![
                                        (name!(name), Value::String(name).into()),
                                        (name!(as), Value::String(alias).into()),
                                    ])
                                } else {
                                    Value::String(name)
                                }
                                .into()
                            })
                            .collect::<Vec<_>>(),
                    )
                    .into(),
                }
                .into(),
            );
        }

        Directive {
            name: name!(link),
            arguments,
        }
    }
}

trait CopyDirective {
    fn copy_directive(
        &self,
        from: &FederationSchema,
        to: &mut FederationSchema,
        directive_name: &Name,
    ) -> Result<(), FederationError>;

    fn copy_join_directive_directive(
        &self,
        from: &FederationSchema,
        to: &mut FederationSchema,
        subgraph_name_replacements: &MultiMap<Name, Name>,
        connect_directive_names: &HashMap<&str, [Name; 2]>,
    ) -> Result<(), FederationError>;
}

impl CopyDirective for SchemaDefinitionPosition {
    fn copy_directive(
        &self,
        from: &FederationSchema,
        to: &mut FederationSchema,
        directive_name: &Name,
    ) -> Result<(), FederationError> {
        self.get(from.schema())
            .directives
            .iter()
            .filter(|d| &d.name == directive_name)
            .try_for_each(|directive| self.insert_directive(to, directive.clone()))
    }

    fn copy_join_directive_directive(
        &self,
        from: &FederationSchema,
        to: &mut FederationSchema,
        subgraph_name_replacements: &MultiMap<Name, Name>,
        connect_directive_names: &HashMap<&str, [Name; 2]>,
    ) -> Result<(), FederationError> {
        self.get(from.schema())
            .directives
            .iter()
            .filter(|d| d.name == JOIN_DIRECTIVE_DIRECTIVE_NAME)
            .try_for_each(|directive| {
                if let Some(updated_directive) = inputs::replace_join_directive_graphs_argument(
                    directive,
                    subgraph_name_replacements,
                    connect_directive_names,
                ) {
                    self.insert_directive(to, updated_directive.into())
                } else {
                    Ok(())
                }
            })
    }
}

macro_rules! impl_copy_directive {
    ($( $Ty: ty )+) => {
        $(
            impl CopyDirective for $Ty {
                fn copy_directive(
                    &self,
                    from: &FederationSchema,
                    to: &mut FederationSchema,
                    directive_name: &Name,
                ) -> Result<(), FederationError> {
                    self.get(from.schema())
                        .map(|def| {
                            def.directives
                                .iter()
                                .filter(|d| &d.name == directive_name)
                                .try_for_each(|directive| {
                                    if self.get(to.schema()).is_ok() {
                                        self.insert_directive(to, directive.clone())
                                    } else {
                                        Ok(())
                                    }
                                })
                        })
                        .unwrap_or(Ok(()))
                }

                fn copy_join_directive_directive(
                    &self,
                    from: &FederationSchema,
                    to: &mut FederationSchema,
                    subgraph_name_replacements: &MultiMap<Name, Name>,
                    connect_directive_names: &HashMap<&str, [Name; 2]>,
                ) -> Result<(), FederationError> {
                    self.get(from.schema())
                        .map(|def| {
                            def.directives
                                .iter()
                                .filter(|d| d.name == JOIN_DIRECTIVE_DIRECTIVE_NAME)
                                .try_for_each(|directive| {
                                    if let Some(updated_directive) = inputs::replace_join_directive_graphs_argument(
                                        directive,
                                        subgraph_name_replacements,
                                        connect_directive_names
                                    ) && self.get(to.schema()).is_ok() {
                                        self.insert_directive(to, updated_directive.into())
                                    } else {
                                        Ok(())
                                    }
                                })
                        })
                        .unwrap_or(Ok(()))
                }
            }
        )+
    };
}

impl_copy_directive! {
    ScalarTypeDefinitionPosition
    ObjectTypeDefinitionPosition
    ObjectFieldDefinitionPosition
    ObjectFieldArgumentDefinitionPosition
    InterfaceTypeDefinitionPosition
    InterfaceFieldDefinitionPosition
    InterfaceFieldArgumentDefinitionPosition
    UnionTypeDefinitionPosition
    EnumTypeDefinitionPosition
    EnumValueDefinitionPosition
    InputObjectTypeDefinitionPosition
    InputObjectFieldDefinitionPosition
    DirectiveArgumentDefinitionPosition
}

impl DirectiveReferencers {
    pub(crate) fn len(&self) -> usize {
        self.schema.as_ref().map(|_| 1).unwrap_or_default()
            + self.scalar_types.len()
            + self.object_types.len()
            + self.object_fields.len()
            + self.object_field_arguments.len()
            + self.interface_types.len()
            + self.interface_fields.len()
            + self.interface_field_arguments.len()
            + self.union_types.len()
            + self.enum_types.len()
            + self.enum_values.len()
            + self.input_object_types.len()
            + self.input_object_fields.len()
            + self.directive_arguments.len()
    }

    fn copy_directives(
        &self,
        from: &FederationSchema,
        to: &mut FederationSchema,
        directive_name: &Name,
    ) -> Result<(), FederationError> {
        if let Some(position) = &self.schema {
            position.copy_directive(from, to, directive_name)?
        }
        self.scalar_types
            .iter()
            .try_for_each(|position| position.copy_directive(from, to, directive_name))?;
        self.object_types
            .iter()
            .try_for_each(|position| position.copy_directive(from, to, directive_name))?;
        self.object_fields
            .iter()
            .try_for_each(|position| position.copy_directive(from, to, directive_name))?;
        self.object_field_arguments
            .iter()
            .try_for_each(|position| position.copy_directive(from, to, directive_name))?;
        self.interface_types
            .iter()
            .try_for_each(|position| position.copy_directive(from, to, directive_name))?;
        self.interface_fields
            .iter()
            .try_for_each(|position| position.copy_directive(from, to, directive_name))?;
        self.interface_field_arguments
            .iter()
            .try_for_each(|position| position.copy_directive(from, to, directive_name))?;
        self.union_types
            .iter()
            .try_for_each(|position| position.copy_directive(from, to, directive_name))?;
        self.enum_types
            .iter()
            .try_for_each(|position| position.copy_directive(from, to, directive_name))?;
        self.enum_values
            .iter()
            .try_for_each(|position| position.copy_directive(from, to, directive_name))?;
        self.input_object_types
            .iter()
            .try_for_each(|position| position.copy_directive(from, to, directive_name))?;
        self.input_object_fields
            .iter()
            .try_for_each(|position| position.copy_directive(from, to, directive_name))?;
        self.directive_arguments
            .iter()
            .try_for_each(|position| position.copy_directive(from, to, directive_name))?;
        Ok(())
    }

    fn copy_join_directive_directives(
        &self,
        from: &FederationSchema,
        to: &mut FederationSchema,
        subgraph_name_replacements: &MultiMap<Name, Name>,
        connect_directive_names: &HashMap<&str, [Name; 2]>,
    ) -> Result<(), FederationError> {
        if let Some(position) = &self.schema {
            position.copy_join_directive_directive(
                from,
                to,
                subgraph_name_replacements,
                connect_directive_names,
            )?
        }
        self.scalar_types.iter().try_for_each(|position| {
            position.copy_join_directive_directive(
                from,
                to,
                subgraph_name_replacements,
                connect_directive_names,
            )
        })?;
        self.object_types.iter().try_for_each(|position| {
            position.copy_join_directive_directive(
                from,
                to,
                subgraph_name_replacements,
                connect_directive_names,
            )
        })?;
        self.object_fields.iter().try_for_each(|position| {
            position.copy_join_directive_directive(
                from,
                to,
                subgraph_name_replacements,
                connect_directive_names,
            )
        })?;
        self.object_field_arguments
            .iter()
            .try_for_each(|position| {
                position.copy_join_directive_directive(
                    from,
                    to,
                    subgraph_name_replacements,
                    connect_directive_names,
                )
            })?;
        self.interface_types.iter().try_for_each(|position| {
            position.copy_join_directive_directive(
                from,
                to,
                subgraph_name_replacements,
                connect_directive_names,
            )
        })?;
        self.interface_fields.iter().try_for_each(|position| {
            position.copy_join_directive_directive(
                from,
                to,
                subgraph_name_replacements,
                connect_directive_names,
            )
        })?;
        self.interface_field_arguments
            .iter()
            .try_for_each(|position| {
                position.copy_join_directive_directive(
                    from,
                    to,
                    subgraph_name_replacements,
                    connect_directive_names,
                )
            })?;
        self.union_types.iter().try_for_each(|position| {
            position.copy_join_directive_directive(
                from,
                to,
                subgraph_name_replacements,
                connect_directive_names,
            )
        })?;
        self.enum_types.iter().try_for_each(|position| {
            position.copy_join_directive_directive(
                from,
                to,
                subgraph_name_replacements,
                connect_directive_names,
            )
        })?;
        self.enum_values.iter().try_for_each(|position| {
            position.copy_join_directive_directive(
                from,
                to,
                subgraph_name_replacements,
                connect_directive_names,
            )
        })?;
        self.input_object_types.iter().try_for_each(|position| {
            position.copy_join_directive_directive(
                from,
                to,
                subgraph_name_replacements,
                connect_directive_names,
            )
        })?;
        self.input_object_fields.iter().try_for_each(|position| {
            position.copy_join_directive_directive(
                from,
                to,
                subgraph_name_replacements,
                connect_directive_names,
            )
        })?;
        self.directive_arguments.iter().try_for_each(|position| {
            position.copy_join_directive_directive(
                from,
                to,
                subgraph_name_replacements,
                connect_directive_names,
            )
        })?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use apollo_compiler::Schema;
    use apollo_compiler::name;
    use insta::assert_snapshot;

    use super::carryover_directives;
    use crate::connectors::ConnectSpec;
    use crate::merge::merge_federation_subgraphs;
    use crate::schema::FederationSchema;
    use crate::schema::position::EnumTypeDefinitionPosition;
    use crate::supergraph::extract_subgraphs_from_supergraph;

    #[test]
    fn test_carryover() {
        let sdl = include_str!("./tests/schemas/ignore/directives.graphql");
        let schema = Schema::parse(sdl, "directives.graphql").expect("parse failed");
        let supergraph_schema = FederationSchema::new(schema).expect("federation schema failed");
        let subgraphs = extract_subgraphs_from_supergraph(&supergraph_schema, None)
            .expect("extract subgraphs failed");
        let merged = merge_federation_subgraphs(subgraphs).expect("merge failed");
        let schema = merged.schema.into_inner();

        let mut schema = FederationSchema::new(schema).expect("federation schema failed");

        // to test graceful handling of types that don't make it into the merged
        // result because no connector references them
        EnumTypeDefinitionPosition {
            type_name: name!(UnusedEnum),
        }
        .remove(&mut schema)
        .unwrap();

        carryover_directives(
            &supergraph_schema,
            &mut schema,
            [ConnectSpec::V0_1].into_iter(),
            &Default::default(),
            Default::default(),
        )
        .expect("carryover failed");
        assert_snapshot!(schema.schema().serialize().to_string());
    }
}
