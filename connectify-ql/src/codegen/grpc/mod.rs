use std::fs::File;
use std::io::prelude::*;
use std::{path::PathBuf, str::FromStr};

use crate::codegen::grpc::processor::{process_enum_types, process_message_types, process_service};
use crate::codegen::grpc::types::{AvailableType, GraphqlType, GrpcType};
use crate::sources::Grpc;
use prost_types::FileDescriptorSet;
use protox::Error as ProtoxError;

pub mod processor;
pub mod types;

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

fn proto_ast(grpc: &Grpc) -> Result<FileDescriptorSet, GrpcCodegenError> {
    Ok(protox::compile([&grpc.file], ["."])?)
}

pub fn generate(grpc: &Grpc) -> Result<PathBuf, GrpcCodegenError> {
    let proto_ast = proto_ast(grpc)?;
    let graphql_file = grpc.file.replace(".proto", ".graphql");
    let mut graphql_content = String::new();
    let default_service = grpc.service.split('.').last().unwrap_or("GrpcService");
    if !proto_ast
        .file
        .iter()
        .flat_map(|file| &file.service)
        .any(|service| service.name() == default_service)
    {
        return Err(GrpcCodegenError::ServiceNotFound(
            default_service.to_string(),
            proto_ast
                .file
                .iter()
                .flat_map(|file| &file.service)
                .map(|service| service.name())
                .collect::<Vec<_>>()
                .join(", "),
        ));
    }

    // process all types first
    let available_types: Result<Vec<AvailableType>, GrpcCodegenError> = proto_ast
        .file
        .iter()
        .map(|file| {
            let external_path = (file.name().replace('/', ".").contains(file.package())
                && !file.package().is_empty())
            .then(|| file.package())
            .unwrap_or_default();

            Ok((
                process_enum_types(
                    &mut graphql_content,
                    &file.enum_type,
                    &grpc.service,
                    external_path,
                )?,
                process_message_types(
                    &mut graphql_content,
                    &file.message_type,
                    &grpc.service,
                    external_path,
                )?,
            ))
        })
        .try_fold(Vec::new(), |mut acc, types: Result<_, GrpcCodegenError>| {
            let (mut enums, mut messages) = types?;
            acc.append(&mut enums);
            acc.append(&mut messages);
            Ok(acc)
        });
    let available_types = available_types?;

    // process all services
    for file in &proto_ast.file {
        process_service(&mut graphql_content, &file.service, grpc, &available_types)?;
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
            file: "src/codegen/grpc/fixture/user.proto".to_string(),
            service: "com.example.users.UserService".to_string(),
            endpoint: "localhost:8080".to_string(),
            mutations: vec!["CreateUser".to_string()],
        }
    }

    mod import_files {
        use super::*;

        #[test]
        fn should_generate_file_descriptor_when_correct_grpc() {
            let grpc = null_grpc();
            let proto = format!("{:#?}", proto_ast(&grpc).unwrap());
            assert_snapshot!(proto);
        }

        #[test]
        fn should_generate_graphql_from_empty_grpc() {
            let grpc = null_grpc();
            let path = generate(&grpc).unwrap();
            let graphql: String = std::fs::read_to_string(path).unwrap();
            assert_snapshot!(graphql);
        }

        fn null_grpc() -> Grpc {
            Grpc {
                file: "src/codegen/grpc/fixture/check.proto".to_string(),
                service: "com.example.check.CheckService".to_string(),
                endpoint: "localhost:8080".to_string(),
                mutations: vec![],
            }
        }
    }
}
