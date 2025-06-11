use std::collections::BTreeSet;
use std::fmt::Write as _;

use crate::codegen::grpc::GrpcCodegenError;
use crate::codegen::grpc::types::{AvailableType, GraphqlType, GrpcType};
use crate::sources::Grpc;
use convert_case::{Boundary, Case, Casing};
use prost_types::field_descriptor_proto::Label;
use prost_types::{DescriptorProto, EnumDescriptorProto, ServiceDescriptorProto};

pub fn process_enum_types(
    content: &mut String,
    enum_types: &[EnumDescriptorProto],
    service: &str,
    external_path: &str,
) -> Result<Vec<AvailableType>, GrpcCodegenError> {
    let default_naming = service.split('.').last().unwrap_or("GrpcService");
    let mut available_types = Vec::new();

    for (index, enum_type) in enum_types.iter().enumerate() {
        let name = enum_type
            .name
            .clone()
            .unwrap_or_else(|| format!("{}Enum{}", default_naming, index + 1))
            .to_case(Case::Pascal);
        let values = enum_type
            .value
            .iter()
            .enumerate()
            .map(|(idx, value)| {
                value
                    .name
                    .clone()
                    .unwrap_or_else(|| format!("    Value{idx}"))
                    .to_case(Case::Constant)
            })
            .collect::<Vec<_>>();
        available_types.push(AvailableType {
            name: if external_path.is_empty() {
                name.clone()
            } else {
                format!("{external_path}.{name}")
            },
            graph_name: if external_path.is_empty() {
                name.clone()
            } else {
                format!("{external_path}.{name}")
                    .with_boundaries(&[Boundary::from_delim(".")])
                    .to_case(Case::Ada)
            },
            is_empty: values.is_empty(),
        });

        if external_path.is_empty() {
            writeln!(content, "enum {} {{", name)?;
        } else {
            writeln!(
                content,
                "enum {}_{} {{",
                external_path
                    .with_boundaries(&[Boundary::from_delim(".")])
                    .to_case(Case::Ada),
                name
            )?;
        };
        for value in values {
            writeln!(content, "  {}", value)?;
        }
        writeln!(content, "}}\n",)?;
    }
    Ok(available_types)
}

pub fn process_message_types(
    content: &mut String,
    message_types: &[DescriptorProto],
    service: &str,
    external_path: &str,
) -> Result<Vec<AvailableType>, GrpcCodegenError> {
    let default_naming = service.split('.').last().unwrap_or("GrpcService");
    let mut available_types = Vec::new();

    for (index, message_type) in message_types.iter().enumerate() {
        let name = message_type
            .name
            .clone()
            .unwrap_or_else(|| format!("{}Message{}", default_naming, index + 1))
            .to_case(Case::Pascal);
        let values = message_type
            .field
            .iter()
            .enumerate()
            .map(|(idx, field)| {
                (
                    idx,
                    &field.name,
                    field.type_name.iter().cloned(),
                    field.r#type,
                    field.label().as_str_name() == Label::Repeated.as_str_name(),
                    field.proto3_optional(),
                )
            })
            .collect::<Vec<_>>();

        available_types.push(AvailableType {
            name: if external_path.is_empty() {
                name.clone()
            } else {
                format!("{external_path}.{name}")
            },
            graph_name: if external_path.is_empty() {
                name.clone()
            } else {
                format!("{external_path}.{name}")
                    .with_boundaries(&[Boundary::from_delim(".")])
                    .to_case(Case::Ada)
            },
            is_empty: values.is_empty(),
        });

        if external_path.is_empty() {
            writeln!(content, "type {} {{", name)?;
        } else {
            writeln!(
                content,
                "type {}_{} {{",
                external_path
                    .with_boundaries(&[Boundary::from_delim(".")])
                    .to_case(Case::Ada),
                name
            )?;
        };
        for (idx, attribute_name, mut attribute_type_name, attribute_type, is_array, is_optional) in
            values
        {
            write!(
                content,
                "  {}: ",
                attribute_name
                    .clone()
                    .unwrap_or_else(|| format!("field_{}", idx))
                    .to_case(Case::Snake)
            )?;
            let field_type = attribute_type
                .map(GrpcType::try_from)
                .and_then(
                    |type_name| match type_name.and_then(GraphqlType::try_from) {
                        Ok(GraphqlType::Int) => Some("Int".to_string()),
                        Ok(GraphqlType::Float) => Some("Float".to_string()),
                        Ok(GraphqlType::String) => Some("String".to_string()),
                        Ok(GraphqlType::Boolean) => Some("Boolean".to_string()),
                        Ok(GraphqlType::ID) => Some("ID".to_string()),
                        _ => None,
                    },
                )
                .or_else(|| attribute_type_name.next())
                .and_then(|ty_name| ty_name.split('.').map(|n| n.to_string()).last())
                .ok_or(GrpcCodegenError::UnknownGrpcType)?;

            writeln!(
                content,
                "{}{}{}{}",
                if is_array { "[" } else { "" },
                field_type,
                if is_optional { "" } else { "!" },
                if is_array { "]!" } else { "" },
            )?;
        }
        writeln!(content, "}}\n",)?;
    }
    Ok(available_types)
}

pub fn process_service(
    content: &mut String,
    services: &[ServiceDescriptorProto],
    grpc: &Grpc,
    available_types: &[AvailableType],
    dependencies: BTreeSet<String>,
) -> Result<(), GrpcCodegenError> {
    let default_naming = grpc.service.split('.').last().unwrap_or("GrpcService");
    let registered_mutations = grpc.mutations.as_slice();

    #[derive(Debug)]
    struct Method<'d> {
        name: String,
        input: Option<&'d str>,
        output: Option<&'d str>,
    }

    let mut queries: Vec<Method> = Vec::new();
    let mut mutations: Vec<Method> = Vec::new();

    if let Some(service) = services
        .iter()
        .find(|service| service.name() == default_naming)
    {
        for (index, method) in service.method.iter().enumerate() {
            let method_name = method
                .name
                .clone()
                .unwrap_or_else(|| format!("Method{index}"));
            let input = method
                .input_type
                .as_ref()
                .and_then(|name| match name.strip_prefix('.') {
                    Some(strip_name) if dependencies.contains(&strip_name.to_lowercase()) => {
                        Some(strip_name)
                    }
                    _ => name.split('.').last(),
                });
            let output =
                method
                    .output_type
                    .as_ref()
                    .and_then(|name| match name.strip_prefix('.') {
                        Some(strip_name) if dependencies.contains(&strip_name.to_lowercase()) => {
                            Some(strip_name)
                        }
                        _ => name.split('.').last(),
                    });

            if registered_mutations.contains(&method_name) {
                mutations.push(Method {
                    name: method_name,
                    input,
                    output,
                });
            } else {
                queries.push(Method {
                    name: method_name,
                    input,
                    output,
                });
            }
        }

        if !queries.is_empty() {
            writeln!(content, "type Query {{")?;
            for query in queries {
                write!(content, "  {}", query.name.to_case(Case::Camel))?;
                if let Some(input) = query.input.and_then(|input| {
                    available_types
                        .iter()
                        .find(|ty| ty.graph_name == input || ty.name == input)
                }) {
                    if !input.is_empty {
                        write!(content, "(input: {}!)", input.graph_name)?;
                    }
                }
                if let Some(output) = query.output.and_then(|output| {
                    available_types
                        .iter()
                        .find(|ty| ty.graph_name == output || ty.name == output)
                }) {
                    if !output.is_empty {
                        write!(content, ": {}!", output.graph_name)?;
                    }
                }
                writeln!(content)?;
            }
            writeln!(content, "}}\n",)?;
        }

        if !mutations.is_empty() {
            writeln!(content, "type Mutation {{")?;
            for mutation in mutations {
                write!(content, "  {}", mutation.name.to_case(Case::Camel))?;
                if let Some(input) = mutation
                    .input
                    .and_then(|input| available_types.iter().find(|ty| ty.graph_name == input))
                {
                    if !input.is_empty {
                        write!(content, "(input: {}!)", input.graph_name)?;
                    }
                }
                if let Some(output) = mutation
                    .output
                    .and_then(|output| available_types.iter().find(|ty| ty.graph_name == output))
                {
                    if !output.is_empty {
                        write!(content, ": {}!", output.graph_name)?;
                    }
                }
                writeln!(content)?;
            }
            writeln!(content, "}}\n",)?;
        }
    };

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use insta::assert_snapshot;

    mod enum_descriptor {
        use std::str::FromStr;

        use prost_types::EnumValueDescriptorProto;

        use super::*;

        #[test]
        fn should_create_gql_enum_with_full_grpc() {
            let mut s = String::new();
            let enum_descriptor = EnumDescriptorProto {
                name: Some("MyEnum".to_string()),
                value: vec![
                    EnumValueDescriptorProto {
                        name: Some("Active".to_string()),
                        ..Default::default()
                    },
                    EnumValueDescriptorProto {
                        name: Some("Inactive".to_string()),
                        ..Default::default()
                    },
                ],
                ..Default::default()
            };
            let nameless_service = String::new();

            process_enum_types(&mut s, &[enum_descriptor], &nameless_service, "").unwrap();

            assert_snapshot!(s);
        }

        #[test]
        fn should_create_gql_enum_with_nameless_grpc_attributes() {
            let mut s = String::new();
            let enum_descriptor = EnumDescriptorProto {
                name: None,
                value: vec![
                    EnumValueDescriptorProto {
                        name: None,
                        ..Default::default()
                    },
                    EnumValueDescriptorProto {
                        name: None,
                        ..Default::default()
                    },
                ],
                ..Default::default()
            };
            let service = String::from_str("UserService").unwrap();

            process_enum_types(&mut s, &[enum_descriptor], &service, "").unwrap();

            assert_snapshot!(s);
        }
    }

    mod message_descriptor {
        use std::str::FromStr;

        use prost_types::FieldDescriptorProto;

        use super::*;

        #[test]
        fn should_create_gql_type_with_full_grpc() {
            let mut s = String::new();
            let message_descriptor = DescriptorProto {
                name: Some("MyMessage".to_string()),
                field: vec![
                    // float: 2, int: 5, bool: 8, string: 9, message: 11, bytes: 12, enum: 14
                    FieldDescriptorProto {
                        name: Some("my_id".to_string()),
                        r#type: Some(12),
                        type_name: None,
                        proto3_optional: Some(false),
                        ..Default::default()
                    },
                    FieldDescriptorProto {
                        name: Some("my_enum".to_string()),
                        r#type: Some(14),
                        type_name: Some("com.demo.package.user.Enum".to_string()),
                        proto3_optional: Some(true),
                        ..Default::default()
                    },
                    FieldDescriptorProto {
                        name: Some("my_user".to_string()),
                        r#type: Some(11),
                        type_name: Some("com.demo.package.user.User".to_string()),
                        proto3_optional: None,
                        ..Default::default()
                    },
                    FieldDescriptorProto {
                        name: Some("my_bool".to_string()),
                        r#type: Some(8),
                        type_name: None,
                        proto3_optional: None,
                        ..Default::default()
                    },
                    FieldDescriptorProto {
                        name: Some("my_int".to_string()),
                        r#type: Some(5),
                        type_name: None,
                        proto3_optional: None,
                        ..Default::default()
                    },
                    FieldDescriptorProto {
                        name: Some("my_float".to_string()),
                        r#type: Some(2),
                        type_name: None,
                        proto3_optional: None,
                        ..Default::default()
                    },
                    FieldDescriptorProto {
                        name: Some("my_string".to_string()),
                        r#type: Some(9),
                        type_name: None,
                        proto3_optional: None,
                        ..Default::default()
                    },
                ],
                ..Default::default()
            };
            let nameless_service = String::new();

            process_message_types(&mut s, &[message_descriptor], &nameless_service, "").unwrap();

            assert_snapshot!(s);
        }

        #[test]
        fn should_create_gql_type_with_missing_grpc_info() {
            let mut s = String::new();
            let message_descriptor = DescriptorProto {
                name: None,
                field: vec![
                    // float: 2, int: 5, bool: 8, string: 9, message: 11, bytes: 12, enum: 14
                    FieldDescriptorProto {
                        name: None,
                        r#type: Some(12),
                        type_name: None,
                        proto3_optional: Some(false),
                        ..Default::default()
                    },
                ],
                ..Default::default()
            };
            let service = String::from_str("MyRandomService").unwrap();

            process_message_types(&mut s, &[message_descriptor], &service, "").unwrap();

            assert_snapshot!(s);
        }

        #[test]
        fn should_create_gql_type_with_unknown_grpc_type() {
            let mut s = String::new();
            let message_descriptor = DescriptorProto {
                name: None,
                field: vec![
                    // float: 2, int: 5, bool: 8, string: 9, message: 11, bytes: 12, enum: 14
                    FieldDescriptorProto {
                        name: None,
                        r#type: Some(30),
                        type_name: None,
                        proto3_optional: Some(false),
                        ..Default::default()
                    },
                ],
                ..Default::default()
            };
            let service = String::from_str("MyRandomService").unwrap();

            let err =
                process_message_types(&mut s, &[message_descriptor], &service, "").unwrap_err();
            let err = err.to_string();
            assert_eq!(err, "unknown gRPC type");
        }

        #[test]
        // TODO: Fix the error type
        fn should_create_gql_type_with_not_supported_grpc_type() {
            let mut s = String::new();
            let message_descriptor = DescriptorProto {
                name: None,
                field: vec![
                    // float: 2, int: 5, bool: 8, string: 9, message: 11, bytes: 12, enum: 14
                    FieldDescriptorProto {
                        name: None,
                        r#type: Some(7),
                        type_name: None,
                        proto3_optional: Some(false),
                        ..Default::default()
                    },
                ],
                ..Default::default()
            };
            let service = String::from_str("MyRandomService").unwrap();

            let err =
                process_message_types(&mut s, &[message_descriptor], &service, "").unwrap_err();
            let err = err.to_string();
            assert_eq!(err, "unknown gRPC type");
        }
    }

    mod service_descriptor {
        use prost_types::MethodDescriptorProto;

        use super::*;

        #[test]
        fn should_create_gql_queries_from_grpc_service() {
            let grpc_service = grpc_service();
            let grpc = Grpc {
                file: "user.proto".to_string(),
                service: "com.example.users.UserService".to_string(),
                mutations: vec!["CreateUser".to_string()],
                endpoint: "localhost:8080".to_string(),
            };
            let mut s = String::new();
            let types = vec![
                AvailableType {
                    name: "GetUserRequest".to_string(),
                    graph_name: "GetUserRequest".to_string(),
                    is_empty: false,
                },
                AvailableType {
                    name: "GetUserResponse".to_string(),
                    graph_name: "GetUserResponse".to_string(),
                    is_empty: false,
                },
                AvailableType {
                    name: "CreatedUserResponse".to_string(),
                    graph_name: "CreatedUserResponse".to_string(),
                    is_empty: false,
                },
                AvailableType {
                    name: "User".to_string(),
                    graph_name: "User".to_string(),
                    is_empty: false,
                },
            ];
            process_service(&mut s, &grpc_service, &grpc, &types, [].into()).unwrap();

            assert_snapshot!(s);
        }

        #[test]
        fn should_create_gql_queries_from_empty_grpc_service() {
            let grpc_service = empty_grpc_service();
            let grpc = Grpc {
                file: "check.proto".to_string(),
                service: "com.example.check.CheckService".to_string(),
                mutations: Vec::new(),
                endpoint: "localhost:8080".to_string(),
            };
            let mut s = String::new();
            let types = vec![
                AvailableType {
                    name: "Empty".to_string(),
                    graph_name: "Google_Protobuf_Empty".to_string(),
                    is_empty: true,
                },
                AvailableType {
                    name: "CustomEmpty".to_string(),
                    graph_name: "CustomEmpty".to_string(),
                    is_empty: true,
                },
            ];
            process_service(&mut s, &grpc_service, &grpc, &types, [].into()).unwrap();

            assert_snapshot!(s);
        }

        fn empty_grpc_service() -> Vec<ServiceDescriptorProto> {
            vec![ServiceDescriptorProto {
                name: Some("CheckService".to_string()),
                method: vec![MethodDescriptorProto {
                    name: Some("Check".to_string()),
                    input_type: Some(".google.protobuf.Empty".to_string()),
                    output_type: Some(".com.example.users.CustomEmpty".to_string()),
                    ..Default::default()
                }],
                options: None,
            }]
        }

        fn grpc_service() -> Vec<ServiceDescriptorProto> {
            vec![ServiceDescriptorProto {
                name: Some("UserService".to_string()),
                method: vec![
                    MethodDescriptorProto {
                        name: Some("GetUser".to_string()),
                        input_type: Some(".com.example.users.GetUserRequest".to_string()),
                        output_type: Some(".com.example.users.GetUserResponse".to_string()),
                        ..Default::default()
                    },
                    MethodDescriptorProto {
                        name: Some("CreateUser".to_string()),
                        input_type: Some(".com.example.users.User".to_string()),
                        output_type: Some(".com.example.users.CreatedUserResponse".to_string()),
                        ..Default::default()
                    },
                ],
                options: None,
            }]
        }
    }
}
