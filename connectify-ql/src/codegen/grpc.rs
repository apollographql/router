use std::fs::File;
use std::io::prelude::*;
use std::{
    fmt::{Display, Write as _},
    path::PathBuf,
    str::FromStr,
};

use crate::sources::Grpc;
use convert_case::{Case, Casing};
use prost_types::{
    DescriptorProto, EnumDescriptorProto, FileDescriptorSet, ServiceDescriptorProto,
};
use protox::Error as ProtoxError;

#[derive(Debug, thiserror::Error)]
pub enum GrpcCodegenError {
    #[error("failed to compile grpc: {0}")]
    ProtoxCompile(#[from] ProtoxError),
    #[error("failed to write new graphql file from grpc")]
    IO(#[from] std::io::Error),
    #[error("failed to write graphql content")]
    Write(#[from] std::fmt::Error),
    #[error("grpc type `{0:?}` not supported by GraphQL, try {1}")]
    TypeNotSupported(GrpcType, GraphqlType),
    #[error("unknown gRPC type")]
    UnknownGrpcType,
    #[error("gRPC service `{0}` not found in protos. Available services are: {1}.")]
    ServiceNotFound(String, String),
}

#[derive(Debug)]
#[repr(i32)]
pub enum GrpcType {
    Double = 1,
    Float = 2,
    Int64 = 3,
    Uint64 = 4,
    Int32 = 5,
    Fixed64 = 6,
    Fixed32 = 7,
    Bool = 8,
    String = 9,
    Group = 10,
    Message = 11,
    Bytes = 12,
    Uint32 = 13,
    Enum = 14,
    Sfixed32 = 15,
    Sfixed64 = 16,
    Sint32 = 17,
    Sint64 = 18,
}

impl TryFrom<i32> for GrpcType {
    type Error = GrpcCodegenError;
    fn try_from(value: i32) -> Result<Self, Self::Error> {
        if !(1..=18).contains(&value) {
            Err(GrpcCodegenError::UnknownGrpcType)
        } else {
            // SAFTEY: Borders checked in if branch
            // SAFETY 2: This value should only be created by the GRPC protox, so it should be contained
            Ok(unsafe { std::mem::transmute::<i32, GrpcType>(value) })
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GraphqlType {
    Int,
    Float,
    String,
    Boolean,
    ID,
    Enum,
    CustomType,
}

impl Display for GraphqlType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}", self)
    }
}

impl TryFrom<GrpcType> for GraphqlType {
    type Error = GrpcCodegenError;

    fn try_from(value: GrpcType) -> Result<Self, Self::Error> {
        Ok(match value {
            GrpcType::Double => {
                return Err(GrpcCodegenError::TypeNotSupported(
                    GrpcType::Double,
                    GraphqlType::Float,
                ));
            }
            GrpcType::Float => GraphqlType::Float,
            GrpcType::Int64 => {
                return Err(GrpcCodegenError::TypeNotSupported(
                    GrpcType::Int64,
                    GraphqlType::Int,
                ));
            }
            GrpcType::Uint64 => {
                return Err(GrpcCodegenError::TypeNotSupported(
                    GrpcType::Uint64,
                    GraphqlType::Int,
                ));
            }
            GrpcType::Fixed64 => {
                return Err(GrpcCodegenError::TypeNotSupported(
                    GrpcType::Fixed64,
                    GraphqlType::Int,
                ));
            }
            GrpcType::Fixed32 => {
                return Err(GrpcCodegenError::TypeNotSupported(
                    GrpcType::Fixed32,
                    GraphqlType::Int,
                ));
            }
            GrpcType::Bool => GraphqlType::Boolean,
            GrpcType::String => GraphqlType::String,
            GrpcType::Group => GraphqlType::CustomType,
            GrpcType::Message => GraphqlType::CustomType,
            GrpcType::Bytes => GraphqlType::ID,
            GrpcType::Uint32 => {
                return Err(GrpcCodegenError::TypeNotSupported(
                    GrpcType::Uint32,
                    GraphqlType::Int,
                ));
            }
            GrpcType::Enum => GraphqlType::Enum,
            GrpcType::Sfixed64 => {
                return Err(GrpcCodegenError::TypeNotSupported(
                    GrpcType::Sfixed64,
                    GraphqlType::Int,
                ));
            }
            GrpcType::Sint32 | GrpcType::Sfixed32 | GrpcType::Int32=> GraphqlType::Int,
            GrpcType::Sint64 => {
                return Err(GrpcCodegenError::TypeNotSupported(
                    GrpcType::Sint64,
                    GraphqlType::Int,
                ));
            }
        })
    }
}

fn proto_ast(grpc: &Grpc) -> Result<FileDescriptorSet, GrpcCodegenError> {
    Ok(protox::compile([&grpc.file], ["."])?)
}

fn process_enum_types(
    content: &mut String,
    enum_types: &[EnumDescriptorProto],
    service: &str,
) -> Result<(), GrpcCodegenError> {
    let default_naming = service.split('.').last().unwrap_or("GrpcService");

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
        writeln!(content, "enum {} {{", name)?;
        for value in values {
            writeln!(content, "  {}", value)?;
        }
        writeln!(content, "}}\n",)?;
    }
    Ok(())
}

fn process_message_types(
    content: &mut String,
    message_types: &[DescriptorProto],
    service: &str,
) -> Result<(), GrpcCodegenError> {
    let default_naming = service.split('.').last().unwrap_or("GrpcService");

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
                    field.proto3_optional(),
                )
            })
            .collect::<Vec<_>>();
        writeln!(content, "type {} {{", name)?;
        for (idx, attribute_name, mut attribute_type_name, attribute_type, is_optional) in values {
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
                "{}{}",
                field_type,
                if is_optional { "" } else { "!" }
            )?;
        }
        writeln!(content, "}}\n",)?;
    }
    Ok(())
}

fn process_service(
    content: &mut String,
    services: &[ServiceDescriptorProto],
    grpc: &Grpc,
) -> Result<(), GrpcCodegenError> {
    let default_naming = grpc.service.split('.').last().unwrap_or("GrpcService");
    println!("{grpc:?}");
    let registered_mutations = grpc.mutations.as_slice();

    struct Method<'d> {
        name: String,
        input: Option<&'d str>,
        output: Option<&'d str>,
    }

    let mut queries: Vec<Method> = Vec::new();
    let mut mutations: Vec<Method> = Vec::new();

    let Some(service) = services
        .iter()
        .find(|service| service.name() == default_naming)
    else {
        return Err(GrpcCodegenError::ServiceNotFound(
            default_naming.to_string(),
            services
                .iter()
                .map(|serv| serv.name())
                .filter(|name| !name.is_empty())
                .collect::<Vec<_>>()
                .join(", "),
        ));
    };

    for (index, method) in service.method.iter().enumerate() {
        let method_name = method
            .name
            .clone()
            .unwrap_or_else(|| format!("Method{index}"));
        let input = method
            .input_type
            .as_ref()
            .and_then(|name| name.split('.').last());
        let output = method
            .output_type
            .as_ref()
            .and_then(|name| name.split('.').last());

        println!("{method_name}\n{registered_mutations:?}");
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

    writeln!(content, "type Query {{")?;
    for query in queries {
        write!(content, "  {}", query.name.to_case(Case::Camel))?;
        if let Some(input) = query.input {
            write!(content, "(input: {}!)", input)?;
        }
        if let Some(output) = query.output {
            write!(content, ": {}!", output)?;
        }
        writeln!(content)?;
    }
    writeln!(content, "}}\n",)?;

    writeln!(content, "type Mutation {{")?;
    for mutation in mutations {
        write!(content, "  {}", mutation.name.to_case(Case::Camel))?;
        if let Some(input) = mutation.input {
            write!(content, "(input: {}!)", input)?;
        }
        if let Some(output) = mutation.output {
            write!(content, ": {}!", output)?;
        }
        writeln!(content)?;
    }
    writeln!(content, "}}\n",)?;

    Ok(())
}

pub fn generate(grpc: &Grpc) -> Result<PathBuf, GrpcCodegenError> {
    let proto_ast = proto_ast(grpc)?;
    let graphql_file = grpc.file.replace(".proto", ".graphql");
    let mut graphql_content = String::new();

    for file in proto_ast.file {
        process_enum_types(&mut graphql_content, &file.enum_type, &grpc.service)?;
        process_message_types(&mut graphql_content, &file.message_type, &grpc.service)?;
        process_service(&mut graphql_content, &file.service, grpc)?;
    }

    let mut file = File::create(&graphql_file)?;
    file.write_all(graphql_content.as_bytes())?;

    // Infallible error
    Ok(PathBuf::from_str(&graphql_file).unwrap())
}

#[cfg(test)]
mod tests {
    use super::*;
    use insta::assert_snapshot;

    #[test]
    fn should_generate_file_descriptor_when_correct_grpc() {
        let grpc = grpc();
        let proto = format!("{:#?}", proto_ast(&grpc).unwrap());
        assert_snapshot!(proto);
    }

    #[test]
    fn should_error_when_wrong_file_name() {
        let mut grpc = grpc();
        grpc.file = "wrong/name.proto".to_string();
        let err = proto_ast(&grpc).unwrap_err().to_string();
        assert_eq!(
            err,
            "failed to compile grpc: file 'wrong/name.proto' is not in any include path"
        );
    }

    #[test]
    fn should_generate_graphql_from_grpc() {
        let grpc = grpc();
        let path = generate(&grpc).unwrap();
        let graphql: String = std::fs::read_to_string(path).unwrap();
        assert_snapshot!(graphql);
    }

    fn grpc() -> Grpc {
        Grpc {
            file: "src/codegen/fixture/user.proto".to_string(),
            service: "com.example.users.UserService".to_string(),
            endpoint: "localhost:8080".to_string(),
            mutations: vec!["CreateUser".to_string()],
        }
    }

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

            process_enum_types(&mut s, &[enum_descriptor], &nameless_service).unwrap();

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

            process_enum_types(&mut s, &[enum_descriptor], &service).unwrap();

            assert_snapshot!(s);
        }
    }

    mod message_descriptor {
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

            process_message_types(&mut s, &[message_descriptor], &nameless_service).unwrap();

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

            process_message_types(&mut s, &[message_descriptor], &service).unwrap();

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

            let err = process_message_types(&mut s, &[message_descriptor], &service).unwrap_err();
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

            let err = process_message_types(&mut s, &[message_descriptor], &service).unwrap_err();
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
            process_service(&mut s, &grpc_service, &grpc).unwrap();

            assert_snapshot!(s);
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
