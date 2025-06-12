use std::{
    fmt::{Display, Write as _},
    fs::File,
    io::Write as _,
};

use indexmap::IndexMap;

use crate::codegen::grpc::GrpcCodegenError;

#[derive(Hash, Debug, PartialEq, Eq)]
pub struct GrpcKey {
    pub source: String,
    pub service: String,
    pub rpc: String,
}

impl Display for GrpcKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let source = self.source.strip_suffix(".proto").unwrap_or(&self.source);
        write!(f, "{source}.{}::{}", self.service, self.rpc)
    }
}

#[derive(Hash, Debug, PartialEq, Eq)]
pub enum GraphQLValue {
    Mutation(String),
    Query(String),
}

impl GraphQLValue {
    pub(crate) fn set_method(&mut self, name: String) {
        match self {
            GraphQLValue::Mutation(value) => {
                *value = name;
            }
            GraphQLValue::Query(value) => {
                *value = name;
            }
        }
    }
}

impl Display for GraphQLValue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            GraphQLValue::Mutation(m) => write!(f, "Mutation::{m}"),
            GraphQLValue::Query(q) => write!(f, "Query::{q}"),
        }
    }
}

pub fn write_mapping_directives(
    map: IndexMap<GrpcKey, GraphQLValue>,
    aol_file: &str,
) -> Result<(), GrpcCodegenError> {
    let mut file = File::create(aol_file)?;
    let mut content = String::new();

    for (grpc, graphql) in map {
        writeln!(content, "use {} as {};", grpc, graphql)?;
    }

    file.write_all(content.as_bytes())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use indexmap::IndexMap;
    use insta::assert_snapshot;

    use super::*;

    #[test]
    fn format_graphql_types() {
        assert_eq!(
            format!("{}", GraphQLValue::Mutation("HelloWorld".to_string())),
            "Mutation::HelloWorld"
        );
        assert_eq!(
            format!("{}", GraphQLValue::Query("HelloWorld".to_string())),
            "Query::HelloWorld"
        );
    }

    #[test]
    fn format_grpc_types_with_proto_source() {
        let grpc = GrpcKey {
            source: "greeting.proto".to_string(),
            service: "HelloService".to_string(),
            rpc: "helloWorld".to_string(),
        };

        assert_eq!(format!("{grpc}"), "greeting.HelloService::helloWorld");
    }

    #[test]
    fn format_grpc_types_without_proto_source() {
        let grpc = GrpcKey {
            source: "greeting".to_string(),
            service: "HelloService".to_string(),
            rpc: "helloWorld".to_string(),
        };

        assert_eq!(format!("{grpc}"), "greeting.HelloService::helloWorld");
    }

    #[test]
    fn should_generate_aol() {
        let path = "src/codegen/grpc/fixture/tests/mapping_coodegen.aol";
        let map = map_layer();
        write_mapping_directives(map, path).unwrap();
        let aol: String = std::fs::read_to_string(path).unwrap();
        assert_snapshot!(aol);
    }

    fn map_layer() -> IndexMap<GrpcKey, GraphQLValue> {
        [
            (
                GrpcKey {
                    source: "user".to_string(),
                    service: "UserService".to_string(),
                    rpc: "GetUser".to_string(),
                },
                GraphQLValue::Query("GetUser".to_string()),
            ),
            (
                GrpcKey {
                    source: "user".to_string(),
                    service: "UserService".to_string(),
                    rpc: "CreateUser".to_string(),
                },
                GraphQLValue::Mutation("CreateUser".to_string()),
            ),
        ]
        .into_iter()
        .collect()
    }
}
