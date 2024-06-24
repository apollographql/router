use apollo_compiler::ast::Argument;
use apollo_compiler::ast::Directive;
use apollo_compiler::ast::DirectiveDefinition;
use apollo_compiler::ast::DirectiveList;
use apollo_compiler::ast::DirectiveLocation;
use apollo_compiler::ast::EnumValueDefinition;
use apollo_compiler::ast::Value;
use apollo_compiler::name;
use apollo_compiler::schema::Component;
use apollo_compiler::schema::EnumType;
use apollo_compiler::schema::ExtendedType;
use apollo_compiler::schema::InputValueDefinition;
use apollo_compiler::schema::Name;
use apollo_compiler::schema::ScalarType;
use apollo_compiler::ty;
use apollo_compiler::Node;
use apollo_compiler::NodeStr;
use apollo_compiler::Schema;
use indexmap::IndexMap;

use crate::ValidFederationSubgraph;

// TODO link spec
pub(super) fn add_core_feature_link(supergraph: &mut Schema) {
    // @link(url: "https://specs.apollo.dev/link/v1.0")
    supergraph
        .schema_definition
        .make_mut()
        .directives
        .push(Component::new(Directive {
            name: name!("link"),
            arguments: vec![Node::new(Argument {
                name: name!("url"),
                value: Node::new(Value::String(NodeStr::new(
                    "https://specs.apollo.dev/link/v1.0",
                ))),
            })],
        }));

    let (name, link_purpose_enum) = link_purpose_enum_type();
    supergraph.types.insert(name, link_purpose_enum.into());

    // scalar Import
    let link_import_name = name!("link__Import");
    let link_import_scalar = ExtendedType::Scalar(Node::new(ScalarType {
        directives: Default::default(),
        name: link_import_name.clone(),
        description: None,
    }));
    supergraph
        .types
        .insert(link_import_name, link_import_scalar);

    let link_directive_definition = link_directive_definition();
    supergraph
        .directive_definitions
        .insert(name!("link"), Node::new(link_directive_definition));
}

pub(super) fn add_authenticated_spec_link(supergraph: &mut Schema) {
    // @link(url: "https://specs.apollo.dev/authenticated/v0.1", for: SECURITY)
    supergraph
        .schema_definition
        .make_mut()
        .directives
        .push(Component::new(Directive {
            name: name!("link"),
            arguments: vec![
                Node::new(Argument {
                    name: name!("url"),
                    value: Node::new(Value::String(NodeStr::new(
                        "https://specs.apollo.dev/authenticated/v0.1",
                    ))),
                }),
                Node::new(Argument {
                    name: name!("for"),
                    value: Node::new(Value::Enum(name!(SECURITY))),
                }),
            ],
        }));

    // directive @authenticated on FIELD_DEFINITION | OBJECT | INTERFACE | SCALAR | ENUM
    supergraph.directive_definitions.insert(
        name!("authenticated"),
        Node::new(DirectiveDefinition {
            name: name!("authenticated"),
            description: None,
            arguments: vec![],
            locations: vec![
                DirectiveLocation::FieldDefinition,
                DirectiveLocation::Object,
                DirectiveLocation::Interface,
                DirectiveLocation::Scalar,
                DirectiveLocation::Enum,
            ],
            repeatable: false,
        }),
    );
}

pub(super) fn add_requires_scopes_spec_link(supergraph: &mut Schema) {
    // @link(url: "https://specs.apollo.dev/requiresScopes/v0.1", for: SECURITY)
    supergraph
        .schema_definition
        .make_mut()
        .directives
        .push(Component::new(Directive {
            name: name!("link"),
            arguments: vec![
                Node::new(Argument {
                    name: name!("url"),
                    value: Node::new(Value::String(NodeStr::new(
                        "https://specs.apollo.dev/requiresScopes/v0.1",
                    ))),
                }),
                Node::new(Argument {
                    name: name!("for"),
                    value: Node::new(Value::Enum(name!(SECURITY))),
                }),
            ],
        }));

    // scalar requiresScopes__Scope
    supergraph.types.insert(
        name!("requiresScopes__Scope"),
        ScalarType {
            description: Default::default(),
            name: name!("requiresScopes__Scope"),
            directives: Default::default(),
        }
        .into(),
    );

    // directive @requiresScopes(scopes: [[requiresScopes__Scope!]!]!) on FIELD_DEFINITION | OBJECT | INTERFACE | SCALAR | ENUM
    supergraph.directive_definitions.insert(
        name!("requiresScopes"),
        Node::new(DirectiveDefinition {
            name: name!("requiresScopes"),
            description: None,
            arguments: vec![Node::new(InputValueDefinition {
                description: Default::default(),
                name: name!(scopes),
                ty: ty!([[requiresScopes__Scope!]!]!).into(),
                default_value: Default::default(),
                directives: Default::default(),
            })],
            locations: vec![
                DirectiveLocation::FieldDefinition,
                DirectiveLocation::Object,
                DirectiveLocation::Interface,
                DirectiveLocation::Scalar,
                DirectiveLocation::Enum,
            ],
            repeatable: false,
        }),
    );
}

pub(super) fn add_policy_spec_link(supergraph: &mut Schema) {
    // @link(url: "https://specs.apollo.dev/policy/v0.1", for: SECURITY)
    supergraph
        .schema_definition
        .make_mut()
        .directives
        .push(Component::new(Directive {
            name: name!("link"),
            arguments: vec![
                Node::new(Argument {
                    name: name!("url"),
                    value: Node::new(Value::String(NodeStr::new(
                        "https://specs.apollo.dev/policy/v0.1",
                    ))),
                }),
                Node::new(Argument {
                    name: name!("for"),
                    value: Node::new(Value::Enum(name!(SECURITY))),
                }),
            ],
        }));

    // scalar policy__Policy
    supergraph.types.insert(
        name!("policy__Policy"),
        ScalarType {
            description: Default::default(),
            name: name!("policy__Policy"),
            directives: Default::default(),
        }
        .into(),
    );

    // directive @policy(policies: [[policy__Policy!]!]!) on FIELD_DEFINITION | OBJECT | INTERFACE | SCALAR | ENUM
    supergraph.directive_definitions.insert(
        name!("policy"),
        Node::new(DirectiveDefinition {
            name: name!("policy"),
            description: None,
            arguments: vec![Node::new(InputValueDefinition {
                description: Default::default(),
                name: name!(policies),
                ty: ty!([[policy__Policy!]!]!).into(),
                default_value: Default::default(),
                directives: Default::default(),
            })],
            locations: vec![
                DirectiveLocation::FieldDefinition,
                DirectiveLocation::Object,
                DirectiveLocation::Interface,
                DirectiveLocation::Scalar,
                DirectiveLocation::Enum,
            ],
            repeatable: false,
        }),
    );
}

/// directive @link(url: String, as: String, import: [Import], for: link__Purpose) repeatable on SCHEMA
pub(super) fn link_directive_definition() -> DirectiveDefinition {
    DirectiveDefinition {
        name: name!("link"),
        description: None,
        arguments: vec![
            Node::new(InputValueDefinition {
                name: name!("url"),
                description: None,
                directives: Default::default(),
                ty: ty!(String).into(),
                default_value: None,
            }),
            Node::new(InputValueDefinition {
                name: name!("as"),
                description: None,
                directives: Default::default(),
                ty: ty!(String).into(),
                default_value: None,
            }),
            Node::new(InputValueDefinition {
                name: name!("for"),
                description: None,
                directives: Default::default(),
                ty: ty!(link__Purpose).into(),
                default_value: None,
            }),
            Node::new(InputValueDefinition {
                name: name!("import"),
                description: None,
                directives: Default::default(),
                ty: ty!([link__Import]).into(),
                default_value: None,
            }),
        ],
        locations: vec![DirectiveLocation::Schema],
        repeatable: true,
    }
}

/// enum link__Purpose {
///   """
///   \`SECURITY\` features provide metadata necessary to securely resolve fields.
///   """
///   SECURITY
///
///   """
///   \`EXECUTION\` features provide metadata necessary for operation execution.
///   """
///   EXECUTION
/// }
pub(super) fn link_purpose_enum_type() -> (Name, EnumType) {
    let link_purpose_name = name!("link__Purpose");
    let mut link_purpose_enum = EnumType {
        description: None,
        name: link_purpose_name.clone(),
        directives: Default::default(),
        values: IndexMap::new(),
    };
    let link_purpose_security_value = EnumValueDefinition {
        description: Some(NodeStr::new(
            r"`SECURITY` features provide metadata necessary to securely resolve fields.",
        )),
        directives: Default::default(),
        value: name!("SECURITY"),
    };
    let link_purpose_execution_value = EnumValueDefinition {
        description: Some(NodeStr::new(
            r"`EXECUTION` features provide metadata necessary for operation execution.",
        )),
        directives: Default::default(),
        value: name!("EXECUTION"),
    };
    link_purpose_enum.values.insert(
        link_purpose_security_value.value.clone(),
        Component::new(link_purpose_security_value),
    );
    link_purpose_enum.values.insert(
        link_purpose_execution_value.value.clone(),
        Component::new(link_purpose_execution_value),
    );
    (link_purpose_name, link_purpose_enum)
}

// TODO join spec
pub(super) fn add_core_feature_join(
    supergraph: &mut Schema,
    subgraphs_and_enum_values: &Vec<(&ValidFederationSubgraph, Name)>,
) {
    // @link(url: "https://specs.apollo.dev/join/v0.3", for: EXECUTION)
    supergraph
        .schema_definition
        .make_mut()
        .directives
        .push(Component::new(Directive {
            name: name!("link"),
            arguments: vec![
                Node::new(Argument {
                    name: name!("url"),
                    value: Node::new(Value::String(NodeStr::new(
                        "https://specs.apollo.dev/join/v0.3",
                    ))),
                }),
                Node::new(Argument {
                    name: name!("for"),
                    value: Node::new(Value::Enum(name!("EXECUTION"))),
                }),
            ],
        }));

    // scalar FieldSet
    let join_field_set_name = name!("join__FieldSet");
    let join_field_set_scalar = ExtendedType::Scalar(Node::new(ScalarType {
        directives: Default::default(),
        name: join_field_set_name.clone(),
        description: None,
    }));
    supergraph
        .types
        .insert(join_field_set_name, join_field_set_scalar);

    let join_graph_directive_definition = join_graph_directive_definition();
    supergraph.directive_definitions.insert(
        join_graph_directive_definition.name.clone(),
        Node::new(join_graph_directive_definition),
    );

    let join_type_directive_definition = join_type_directive_definition();
    supergraph.directive_definitions.insert(
        join_type_directive_definition.name.clone(),
        Node::new(join_type_directive_definition),
    );

    let join_field_directive_definition = join_field_directive_definition();
    supergraph.directive_definitions.insert(
        join_field_directive_definition.name.clone(),
        Node::new(join_field_directive_definition),
    );

    let join_implements_directive_definition = join_implements_directive_definition();
    supergraph.directive_definitions.insert(
        join_implements_directive_definition.name.clone(),
        Node::new(join_implements_directive_definition),
    );

    let join_union_member_directive_definition = join_union_member_directive_definition();
    supergraph.directive_definitions.insert(
        join_union_member_directive_definition.name.clone(),
        Node::new(join_union_member_directive_definition),
    );

    let join_enum_value_directive_definition = join_enum_value_directive_definition();
    supergraph.directive_definitions.insert(
        join_enum_value_directive_definition.name.clone(),
        Node::new(join_enum_value_directive_definition),
    );

    let (name, join_graph_enum_type) = join_graph_enum_type(subgraphs_and_enum_values);
    supergraph.types.insert(name, join_graph_enum_type.into());
}

/// directive @enumValue(graph: join__Graph!) repeatable on ENUM_VALUE
pub(super) fn join_enum_value_directive_definition() -> DirectiveDefinition {
    DirectiveDefinition {
        name: name!("join__enumValue"),
        description: None,
        arguments: vec![Node::new(InputValueDefinition {
            name: name!("graph"),
            description: None,
            directives: Default::default(),
            ty: ty!(join__Graph!).into(),
            default_value: None,
        })],
        locations: vec![DirectiveLocation::EnumValue],
        repeatable: true,
    }
}

/// directive @field(
///   graph: Graph,
///   requires: FieldSet,
///   provides: FieldSet,
///   type: String,
///   external: Boolean,
///   override: String,
///   usedOverridden: Boolean
/// ) repeatable on FIELD_DEFINITION | INPUT_FIELD_DEFINITION
pub(super) fn join_field_directive_definition() -> DirectiveDefinition {
    DirectiveDefinition {
        name: name!("join__field"),
        description: None,
        arguments: vec![
            Node::new(InputValueDefinition {
                name: name!("graph"),
                description: None,
                directives: Default::default(),
                ty: ty!(join__Graph).into(),
                default_value: None,
            }),
            Node::new(InputValueDefinition {
                name: name!("requires"),
                description: None,
                directives: Default::default(),
                ty: ty!(join__FieldSet).into(),
                default_value: None,
            }),
            Node::new(InputValueDefinition {
                name: name!("provides"),
                description: None,
                directives: Default::default(),
                ty: ty!(join__FieldSet).into(),
                default_value: None,
            }),
            Node::new(InputValueDefinition {
                name: name!("type"),
                description: None,
                directives: Default::default(),
                ty: ty!(String).into(),
                default_value: None,
            }),
            Node::new(InputValueDefinition {
                name: name!("external"),
                description: None,
                directives: Default::default(),
                ty: ty!(Boolean).into(),
                default_value: None,
            }),
            Node::new(InputValueDefinition {
                name: name!("override"),
                description: None,
                directives: Default::default(),
                ty: ty!(String).into(),
                default_value: None,
            }),
            Node::new(InputValueDefinition {
                name: name!("usedOverridden"),
                description: None,
                directives: Default::default(),
                ty: ty!(Boolean).into(),
                default_value: None,
            }),
        ],
        locations: vec![
            DirectiveLocation::FieldDefinition,
            DirectiveLocation::InputFieldDefinition,
        ],
        repeatable: true,
    }
}

pub(super) fn join_field_applied_directive(
    subgraph_name: Name,
    requires: Option<&str>,
    provides: Option<&str>,
    external: bool,
) -> Directive {
    let mut join_field_directive = Directive {
        name: name!("join__field"),
        arguments: vec![Node::new(Argument {
            name: name!("graph"),
            value: Node::new(Value::Enum(subgraph_name)),
        })],
    };
    if let Some(required_fields) = requires {
        join_field_directive.arguments.push(Node::new(Argument {
            name: name!("requires"),
            value: Node::new(Value::String(NodeStr::new(required_fields))),
        }));
    }
    if let Some(provided_fields) = provides {
        join_field_directive.arguments.push(Node::new(Argument {
            name: name!("provides"),
            value: Node::new(Value::String(NodeStr::new(provided_fields))),
        }));
    }
    if external {
        join_field_directive.arguments.push(Node::new(Argument {
            name: name!("external"),
            value: Node::new(Value::Boolean(external)),
        }));
    }
    join_field_directive
}

/// directive @graph(name: String!, url: String!) on ENUM_VALUE
pub(super) fn join_graph_directive_definition() -> DirectiveDefinition {
    DirectiveDefinition {
        name: name!("join__graph"),
        description: None,
        arguments: vec![
            Node::new(InputValueDefinition {
                name: name!("name"),
                description: None,
                directives: Default::default(),
                ty: ty!(String!).into(),
                default_value: None,
            }),
            Node::new(InputValueDefinition {
                name: name!("url"),
                description: None,
                directives: Default::default(),
                ty: ty!(String!).into(),
                default_value: None,
            }),
        ],
        locations: vec![DirectiveLocation::EnumValue],
        repeatable: false,
    }
}

/// directive @implements(
///   graph: Graph!,
///   interface: String!
/// ) on OBJECT | INTERFACE
pub(super) fn join_implements_directive_definition() -> DirectiveDefinition {
    DirectiveDefinition {
        name: name!("join__implements"),
        description: None,
        arguments: vec![
            Node::new(InputValueDefinition {
                name: name!("graph"),
                description: None,
                directives: Default::default(),
                ty: ty!(join__Graph!).into(),
                default_value: None,
            }),
            Node::new(InputValueDefinition {
                name: name!("interface"),
                description: None,
                directives: Default::default(),
                ty: ty!(String!).into(),
                default_value: None,
            }),
        ],
        locations: vec![DirectiveLocation::Object, DirectiveLocation::Interface],
        repeatable: true,
    }
}

/// directive @type(
///   graph: Graph!,
///   key: FieldSet,
///   extension: Boolean! = false,
///   resolvable: Boolean = true,
///   isInterfaceObject: Boolean = false
/// ) repeatable on OBJECT | INTERFACE | UNION | ENUM | INPUT_OBJECT | SCALAR
pub(super) fn join_type_directive_definition() -> DirectiveDefinition {
    DirectiveDefinition {
        name: name!("join__type"),
        description: None,
        arguments: vec![
            Node::new(InputValueDefinition {
                name: name!("graph"),
                description: None,
                directives: Default::default(),
                ty: ty!(join__Graph!).into(),
                default_value: None,
            }),
            Node::new(InputValueDefinition {
                name: name!("key"),
                description: None,
                directives: Default::default(),
                ty: ty!(join__FieldSet).into(),
                default_value: None,
            }),
            Node::new(InputValueDefinition {
                name: name!("extension"),
                description: None,
                directives: Default::default(),
                ty: ty!(Boolean!).into(),
                default_value: Some(Node::new(Value::Boolean(false))),
            }),
            Node::new(InputValueDefinition {
                name: name!("resolvable"),
                description: None,
                directives: Default::default(),
                ty: ty!(Boolean!).into(),
                default_value: Some(Node::new(Value::Boolean(true))),
            }),
            Node::new(InputValueDefinition {
                name: name!("isInterfaceObject"),
                description: None,
                directives: Default::default(),
                ty: ty!(Boolean!).into(),
                default_value: Some(Node::new(Value::Boolean(false))),
            }),
        ],
        locations: vec![
            DirectiveLocation::Object,
            DirectiveLocation::Interface,
            DirectiveLocation::Union,
            DirectiveLocation::Enum,
            DirectiveLocation::InputObject,
            DirectiveLocation::Scalar,
        ],
        repeatable: true,
    }
}

/// directive @unionMember(graph: join__Graph!, member: String!) repeatable on UNION
pub(super) fn join_union_member_directive_definition() -> DirectiveDefinition {
    DirectiveDefinition {
        name: name!("join__unionMember"),
        description: None,
        arguments: vec![
            Node::new(InputValueDefinition {
                name: name!("graph"),
                description: None,
                directives: Default::default(),
                ty: ty!(join__Graph!).into(),
                default_value: None,
            }),
            Node::new(InputValueDefinition {
                name: name!("member"),
                description: None,
                directives: Default::default(),
                ty: ty!(String!).into(),
                default_value: None,
            }),
        ],
        locations: vec![DirectiveLocation::Union],
        repeatable: true,
    }
}

/// enum Graph
pub(super) fn join_graph_enum_type(
    subgraphs_and_enum_values: &Vec<(&ValidFederationSubgraph, Name)>,
) -> (Name, EnumType) {
    let join_graph_enum_name = name!("join__Graph");
    let mut join_graph_enum_type = EnumType {
        description: None,
        name: join_graph_enum_name.clone(),
        directives: Default::default(),
        values: IndexMap::new(),
    };
    for (s, subgraph_name) in subgraphs_and_enum_values {
        let join_graph_applied_directive = Directive {
            name: name!("join__graph"),
            arguments: vec![
                (Node::new(Argument {
                    name: name!("name"),
                    value: Node::new(Value::String(NodeStr::new(s.name.as_str()))),
                })),
                (Node::new(Argument {
                    name: name!("url"),
                    value: Node::new(Value::String(NodeStr::new(s.url.as_str()))),
                })),
            ],
        };
        let graph = EnumValueDefinition {
            description: None,
            directives: DirectiveList(vec![Node::new(join_graph_applied_directive)]),
            value: subgraph_name.clone(),
        };
        join_graph_enum_type
            .values
            .insert(graph.value.clone(), Component::new(graph));
    }
    (join_graph_enum_name, join_graph_enum_type)
}
