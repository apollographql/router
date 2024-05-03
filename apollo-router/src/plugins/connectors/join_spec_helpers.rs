use apollo_compiler::ast;
use apollo_compiler::ast::DirectiveList;
use apollo_compiler::executable::Argument;
use apollo_compiler::name;
use apollo_compiler::schema::Directive;
use apollo_compiler::schema::EnumType;
use apollo_compiler::schema::EnumValueDefinition;
use apollo_compiler::schema::ExtendedType;
use apollo_compiler::schema::FieldDefinition;
use apollo_compiler::schema::InputValueDefinition;
use apollo_compiler::schema::Name;
use apollo_compiler::schema::ScalarType;
use apollo_compiler::schema::Type;
use apollo_compiler::schema::Value;
use apollo_compiler::ty;
use apollo_compiler::NodeStr;
use apollo_compiler::Schema;
use indexmap::IndexMap;
use itertools::Itertools;

/// Copy directive and enum/scalar definitions necessary for a supergraph.
pub(super) fn copy_definitions(schema: &Schema, new_schema: &mut Schema) {
    // @link
    let schema_definition = new_schema.schema_definition.make_mut();
    schema
        .schema_definition
        .directives
        .iter()
        .filter(|directive| directive.name == "link")
        .for_each(|directive| {
            schema_definition.directives.push(directive.clone());
        });

    // @join__ directive definitions
    // @link
    // @inaccessible
    schema
        .directive_definitions
        .iter()
        .filter(|(name, _)| {
            name.to_string().starts_with("join__") || *name == "link" || *name == "inaccessible"
        })
        .for_each(|(name, d)| {
            new_schema
                .directive_definitions
                .insert(name.clone(), d.clone());
        });

    // join__ and link__ scalars
    schema
        .types
        .iter()
        .filter(|(name, t)| {
            matches!(t, ExtendedType::Scalar(_))
                && (name.to_string().starts_with("join__")
                    || name.to_string().starts_with("link__"))
        })
        .for_each(|(name, d)| {
            new_schema.types.insert(name.clone(), d.clone());
        });

    // link__ enum
    schema
        .types
        .iter()
        .filter(|(name, t)| {
            matches!(t, ExtendedType::Enum(_)) && (name.to_string().starts_with("link__"))
        })
        .for_each(|(name, d)| {
            new_schema.types.insert(name.clone(), d.clone());
        });
}

// enum join__Graph and @join__graph -------------------------------------------

pub(super) fn join_graph_enum(names: &[&str]) -> ExtendedType {
    let values: IndexMap<_, _> = names
        .iter()
        .map(|name| {
            (
                ast::Name::new_unchecked((*name).into()),
                EnumValueDefinition {
                    value: ast::Name::new_unchecked((*name).into()),
                    directives: DirectiveList(vec![
                        join_graph_directive(name, "http://unused").into()
                    ]),
                    description: None,
                }
                .into(),
            )
        })
        .collect();

    ExtendedType::Enum(
        EnumType {
            name: name!("join__Graph"),
            description: None,
            directives: Default::default(),
            values,
        }
        .into(),
    )
}

// directive @join__graph(name: String!, url: String!) on ENUM_VALUE

fn join_graph_directive(name: &str, url: &str) -> Directive {
    Directive {
        name: name!("join__graph"),
        arguments: vec![
            Argument {
                name: name!("name"),
                value: Value::String(name.into()).into(),
            }
            .into(),
            Argument {
                name: name!("url"),
                value: Value::String(url.into()).into(),
            }
            .into(),
        ],
    }
}

// @join__type -----------------------------------------------------------------

/*
directive @join__type(
  graph: join__Graph!
  key: join__FieldSet
  extension: Boolean! = false         # probably not necessary, i think this is a fed 1 concept
  resolvable: Boolean! = true         # probably not necessary
  isInterfaceObject: Boolean! = false
) repeatable on OBJECT | INTERFACE | UNION | ENUM | INPUT_OBJECT | SCALAR
*/

fn join_type_directive(
    graph: &str,
    key: Option<String>,
    is_interface_object: Option<bool>,
) -> Directive {
    let mut arguments = vec![Argument {
        name: name!("graph"),
        value: Value::Enum(ast::Name::new_unchecked(graph.into())).into(),
    }
    .into()];

    if let Some(fields) = key {
        arguments.push(
            Argument {
                name: name!("key"),
                value: Value::String(NodeStr::new(fields.as_str())).into(),
            }
            .into(),
        );
    }

    if let Some(is_interface_object) = is_interface_object {
        if is_interface_object {
            arguments.push(
                Argument {
                    name: name!("isInterfaceObject"),
                    value: Value::Boolean(true).into(),
                }
                .into(),
            );
        }
    }

    Directive {
        name: name!("join__type"),
        arguments,
    }
}

fn upgrade_join_type_directive(
    directive: &Directive,
    key: Option<String>,
    is_interface_object: Option<bool>,
) -> Directive {
    let graph = directive
        .argument_by_name("graph")
        .and_then(|val| val.as_enum())
        .map(|val| val.as_str())
        .unwrap_or_default();

    let existing_key = directive
        .argument_by_name("key")
        .and_then(|val| val.as_str())
        .map(|val| val.to_string());
    let existing_interface_object = directive
        .argument_by_name("isInterfaceObject")
        .and_then(|val| val.to_bool());

    let key = match (existing_key, key) {
        (Some(k), None) => Some(k.clone()),
        (None, Some(k)) => Some(k.clone()),
        (Some(_), Some(k)) => Some(k.clone()),
        _ => None,
    };

    let is_interface_object = match (existing_interface_object, is_interface_object) {
        (Some(true), _) | (_, Some(true)) => Some(true),
        _ => None,
    };

    join_type_directive(graph, key, is_interface_object)
}

pub(super) fn add_join_type_directive(
    ty: &mut ExtendedType,
    graph: &str,
    key: Option<String>,
    is_interface_object: Option<bool>,
) {
    let existing = ty.directives().iter().find_position(|d| {
        d.name == "join__type"
            && d.argument_by_name("graph")
                .and_then(|val| val.as_enum())
                .map(|val| val.as_str() == graph)
                .unwrap_or(false)
    });

    let (index_to_remove, to_insert) = match existing {
        Some((index, existing)) => (
            Some(index),
            upgrade_join_type_directive(existing, key, is_interface_object),
        ),
        _ => (None, join_type_directive(graph, key, is_interface_object)),
    };

    match ty {
        ExtendedType::Object(ref mut ty) => {
            let ty = ty.make_mut();
            if let Some(index) = index_to_remove {
                ty.directives.remove(index);
            }
            ty.directives.push(to_insert.into());
        }
        ExtendedType::Interface(ty) => {
            let ty = ty.make_mut();
            if let Some(index) = index_to_remove {
                ty.directives.remove(index);
            }
            ty.directives.push(to_insert.into());
        }
        ExtendedType::Union(ty) => {
            let ty = ty.make_mut();
            if let Some(index) = index_to_remove {
                ty.directives.remove(index);
            }
            ty.directives.push(to_insert.into());
        }
        ExtendedType::Enum(ty) => {
            let ty = ty.make_mut();
            if let Some(index) = index_to_remove {
                ty.directives.remove(index);
            }
            ty.directives.push(to_insert.into());
        }
        ExtendedType::InputObject(ty) => {
            let ty = ty.make_mut();
            if let Some(index) = index_to_remove {
                ty.directives.remove(index);
            }
            ty.directives.push(to_insert.into());
        }
        ExtendedType::Scalar(ty) => {
            let ty = ty.make_mut();
            if let Some(index) = index_to_remove {
                ty.directives.remove(index);
            }
            ty.directives.push(to_insert.into());
        }
    }
}

// @join__field ----------------------------------------------------------------

/*
directive @join__field(
  graph: join__Graph
  requires: join__FieldSet # TODO
  provides: join__FieldSet # TODO
  type: String             # TODO
  external: Boolean        # TODO
  override: String         # TODO
  usedOverridden: Boolean  # TODO
) repeatable on FIELD_DEFINITION | INPUT_FIELD_DEFINITION
*/

fn join_field_directive(graph: &str) -> Directive {
    Directive {
        name: name!("join__field"),
        arguments: vec![Argument {
            name: name!("graph"),
            value: Value::Enum(ast::Name::new_unchecked(graph.into())).into(),
        }
        .into()],
    }
}

pub(super) fn add_join_field_directive(field: &mut FieldDefinition, graph: &str) {
    let exists = field.directives.iter().any(|d| {
        d.name == "join__field"
            && d.argument_by_name("graph")
                .and_then(|val| val.as_enum())
                .map(|val| val.as_str() == graph)
                .unwrap_or(false)
    });

    if exists {
        return;
    }

    field.directives.push(join_field_directive(graph).into());
}

pub(super) fn add_input_join_field_directive(field: &mut InputValueDefinition, graph: &str) {
    let exists = field.directives.iter().any(|d| {
        d.name == "join__field"
            && d.argument_by_name("graph")
                .and_then(|val| val.as_enum())
                .map(|val| val.as_str() == graph)
                .unwrap_or(false)
    });

    if exists {
        return;
    }

    field.directives.push(join_field_directive(graph).into());
}

// @join__implements -----------------------------------------------------------

/*

directive @join__implements(
  graph: join__Graph!
  interface: String!
) repeatable on OBJECT | INTERFACE

*/

fn join_implements_directive(graph: &str, interface: &str) -> Directive {
    Directive {
        name: name!("join__implements"),
        arguments: vec![
            Argument {
                name: name!("graph"),
                value: Value::Enum(ast::Name::new_unchecked(graph.into())).into(),
            }
            .into(),
            Argument {
                name: name!("interface"),
                value: Value::String(NodeStr::new(interface)).into(),
            }
            .into(),
        ],
    }
}

pub(super) fn add_join_implements(ty: &mut ExtendedType, graph: &str, interface: &Name) {
    if ty.directives().iter().any(|d| {
        d.name == "join__implements"
            && d.argument_by_name("graph")
                .and_then(|val| val.as_enum())
                .map(|val| val.as_str() == graph)
                .unwrap_or_default()
            && d.argument_by_name("interface")
                .and_then(|val| val.as_str())
                .map(|val| val == interface.as_str())
                .unwrap_or_default()
    }) {
        return;
    }

    match ty {
        ExtendedType::Object(ref mut ty) => {
            let ty = ty.make_mut();
            ty.directives
                .push(join_implements_directive(graph, interface).into());
        }
        ExtendedType::Interface(ty) => {
            let ty = ty.make_mut();
            ty.directives
                .push(join_implements_directive(graph, interface).into());
        }
        _ => debug_assert!(false, "Cannot add join__implements to non-object type"),
    }
}

// @join__enumValue ------------------------------------------------------------

/*
directive @join__enumValue(graph: join__Graph!) repeatable on ENUM_VALUE
*/

pub(super) fn add_join_enum_value_directive(value: &mut EnumValueDefinition, graph: &str) {
    value.directives.push(
        Directive {
            name: name!("join__enumValue"),
            arguments: vec![Argument {
                name: name!("graph"),
                value: Value::Enum(ast::Name::new_unchecked(graph.into())).into(),
            }
            .into()],
        }
        .into(),
    );
}

// @join__unionMember ----------------------------------------------------------

/*
directive @join__unionMember(
  graph: join__Graph!
  member: String!
) repeatable on UNION
*/

fn join_union_member_directive(graph: &str, member: &str) -> Directive {
    Directive {
        name: name!("join__unionMember"),
        arguments: vec![
            Argument {
                name: name!("graph"),
                value: Value::Enum(ast::Name::new_unchecked(graph.into())).into(),
            }
            .into(),
            Argument {
                name: name!("member"),
                value: Value::String(NodeStr::new(member)).into(),
            }
            .into(),
        ],
    }
}

pub(super) fn add_join_union_member_directive(
    ty: &mut ExtendedType,
    graph_name: &str,
    member: &str,
) {
    if ty.directives().iter().any(|d| {
        d.name == "join__unionMember"
            && d.argument_by_name("graph")
                .and_then(|val| val.as_enum())
                .map(|val| val.as_str() == graph_name)
                .unwrap_or(false)
            && d.argument_by_name("member")
                .and_then(|val| val.as_str())
                .map(|val| val == member)
                .unwrap_or(false)
    }) {
        return;
    }

    match ty {
        ExtendedType::Union(ref mut ty) => {
            let ty = ty.make_mut();
            ty.directives
                .push(join_union_member_directive(graph_name, member).into());
        }
        _ => debug_assert!(false, "Cannot add join__unionMember to non-union type"),
    }
}

// Support for magic finder fields ---------------------------------------------

pub(super) fn add_entities_field(
    ty: &mut ExtendedType,
    graph_name: &str,
    name: &str,
    entity_name: &str,
) {
    match ty {
        ExtendedType::Object(ref mut ty) => {
            let ty = ty.make_mut();
            ty.fields
                .entry(ast::Name::new_unchecked(name.into()))
                .and_modify(|f| {
                    f.make_mut()
                        .directives
                        .push(join_field_directive(graph_name).into());
                })
                .or_insert_with(|| {
                    FieldDefinition {
                        name: ast::Name::new_unchecked(name.into()),
                        arguments: vec![InputValueDefinition {
                            description: Default::default(),
                            directives: Default::default(),
                            default_value: Default::default(),
                            name: name!("representations"),
                            ty: ty!([_Any!]!).into(),
                        }
                        .into()],
                        directives: DirectiveList(vec![join_field_directive(graph_name).into()]),
                        description: None,
                        ty: Type::Named(ast::Name::new_unchecked(entity_name.into()))
                            .non_null()
                            .list()
                            .non_null(),
                    }
                    .into()
                });
        }
        _ => debug_assert!(false, "Cannot add entities field to non-object type"),
    }
}

pub(super) fn make_any_scalar() -> ExtendedType {
    ExtendedType::Scalar(
        ScalarType {
            name: name!("_Any"),
            description: None,
            directives: apollo_compiler::schema::DirectiveList(vec![Directive {
                arguments: vec![Argument {
                    name: name!("url"),
                    // just to avoid validation warnings
                    value: Value::String(NodeStr::new("https://whatever")).into(),
                }
                .into()],
                name: name!("specifiedBy"),
            }
            .into()]),
        }
        .into(),
    )
}

// GraphQL Selection Sets for @key fields --------------------------------------

use apollo_compiler::ast::Selection as GraphQLSelection;

use super::request_inputs::PARENT_PREFIX;

fn new_field(name: String, selection: Option<Vec<GraphQLSelection>>) -> GraphQLSelection {
    GraphQLSelection::Field(
        apollo_compiler::ast::Field {
            alias: None,
            name: ast::Name::new_unchecked(name.into()),
            arguments: Default::default(),
            directives: Default::default(),
            selection_set: selection.unwrap_or_default(),
        }
        .into(),
    )
}

// key fields are typically a single line
pub(super) fn selection_set_to_string(selection_set: &[apollo_compiler::ast::Selection]) -> String {
    selection_set
        .iter()
        .map(|s| s.serialize().no_indent().to_string())
        .join(" ")
}

pub(super) fn parameters_to_selection_set(paths: &Vec<String>) -> Vec<GraphQLSelection> {
    let mut root = Vec::new();

    for path in paths {
        let mut parts: Vec<&str> = path.split('.').collect();
        // "$this" is an alias, so we can ignore it
        if parts.first() == Some(&PARENT_PREFIX) {
            parts = parts[1..].to_vec();
        }

        let mut current_node = &mut root;

        for part in parts {
            let existing_node_index =
                current_node
                    .iter()
                    .position(|n: &GraphQLSelection| match n {
                        GraphQLSelection::Field(n) => n.name == part,
                        GraphQLSelection::FragmentSpread(_) => false, // TODO
                        GraphQLSelection::InlineFragment(_) => false, // TODO
                    });

            match existing_node_index {
                Some(index) => {
                    current_node = match &mut current_node[index] {
                        GraphQLSelection::Field(n) => &mut n.make_mut().selection_set,
                        GraphQLSelection::FragmentSpread(_) => todo!(),
                        GraphQLSelection::InlineFragment(_) => todo!(),
                    };
                }
                None => {
                    let new_node = new_field(part.to_string(), Some(Vec::new()));
                    current_node.push(new_node);
                    let new_node_index = current_node.len() - 1;
                    current_node = match &mut current_node[new_node_index] {
                        GraphQLSelection::Field(n) => &mut n.make_mut().selection_set,
                        GraphQLSelection::FragmentSpread(_) => todo!(),
                        GraphQLSelection::InlineFragment(_) => todo!(),
                    };
                }
            }
        }
    }

    root
}

#[cfg(test)]
mod tests {
    use apollo_compiler::name;
    use apollo_compiler::schema::ExtendedType;
    use apollo_compiler::schema::ObjectType;

    use super::add_join_type_directive;
    use super::parameters_to_selection_set;
    use super::selection_set_to_string;

    #[test]
    fn test_parameters_to_selection_set() {
        assert_eq!(
            selection_set_to_string(&parameters_to_selection_set(&vec![
                "id".to_string(),
                "b.c".to_string(),
                "b.d.e".to_string(),
                "b.d.f".to_string(),
                "$this.g".to_string(),
                "$this.h.i".to_string()
            ])),
            "id b { c d { e f } } g h { i }"
        )
    }

    #[test]
    fn test_add_join_type_directive() {
        let mut ty = ExtendedType::Object(
            ObjectType {
                name: name!("Foo"),
                description: None,
                directives: Default::default(),
                fields: Default::default(),
                implements_interfaces: Default::default(),
            }
            .into(),
        );

        add_join_type_directive(&mut ty, "MY_GRAPH", None, None);

        let directive = &ty.directives().first().unwrap().node;
        insta::assert_debug_snapshot!(directive, @r###"
        Directive {
            name: "join__type",
            arguments: [
                Argument {
                    name: "graph",
                    value: Enum(
                        "MY_GRAPH",
                    ),
                },
            ],
        }
        "###);

        add_join_type_directive(&mut ty, "MY_GRAPH", Some("id".to_string()), None);

        assert_eq!(ty.directives().len(), 1);
        let directive = &ty.directives().first().unwrap().node;
        insta::assert_debug_snapshot!(directive, @r###"
        Directive {
            name: "join__type",
            arguments: [
                Argument {
                    name: "graph",
                    value: Enum(
                        "MY_GRAPH",
                    ),
                },
                Argument {
                    name: "key",
                    value: String(
                        "id",
                    ),
                },
            ],
        }
        "###);

        add_join_type_directive(&mut ty, "MY_GRAPH", None, None);

        assert_eq!(ty.directives().len(), 1);
        let directive = &ty.directives().first().unwrap().node;
        insta::assert_debug_snapshot!(directive, @r###"
        Directive {
            name: "join__type",
            arguments: [
                Argument {
                    name: "graph",
                    value: Enum(
                        "MY_GRAPH",
                    ),
                },
                Argument {
                    name: "key",
                    value: String(
                        "id",
                    ),
                },
            ],
        }
        "###);

        add_join_type_directive(&mut ty, "MY_GRAPH", None, Some(true));

        assert_eq!(ty.directives().len(), 1);
        let directive = &ty.directives().first().unwrap().node;
        insta::assert_debug_snapshot!(directive, @r###"
        Directive {
            name: "join__type",
            arguments: [
                Argument {
                    name: "graph",
                    value: Enum(
                        "MY_GRAPH",
                    ),
                },
                Argument {
                    name: "key",
                    value: String(
                        "id",
                    ),
                },
                Argument {
                    name: "isInterfaceObject",
                    value: Boolean(
                        true,
                    ),
                },
            ],
        }
        "###);
    }
}
