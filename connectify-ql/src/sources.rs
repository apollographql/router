use std::borrow::Cow;
use std::collections::BTreeMap;

use indexmap::IndexMap;
use toml_span::{
    DeserError, Deserialize, Span,
    de_helpers::*,
    value::{Key, Value, ValueInner},
};

#[derive(Debug, thiserror::Error)]
pub enum SourceError {
    #[error("failed to parse toml: {0}")]
    TomlParse(#[from] toml_span::Error),
    #[error("failed to deserialize toml: {0}")]
    TomlDeserialization(#[from] toml_span::DeserError),
}

#[derive(Clone, Debug, PartialEq, serde::Deserialize)]
pub struct Sources {
    sources: IndexMap<String, Source>,
}

impl Sources {
    pub fn load(source: &str) -> Result<Self, SourceError> {
        let mut value = toml_span::parse(source)?;
        Ok(Sources::deserialize(&mut value)?)
    }
}

#[derive(Clone, Debug, PartialEq, serde::Deserialize)]
pub enum Source {
    Grpc(Grpc),
    Sql(Sql),
}

#[derive(Clone, Debug, PartialEq, serde::Deserialize)]
pub struct Grpc {
    pub file: String,
    pub service: String,
    pub mutations: Vec<String>,
    pub(crate) endpoint: String,
}

#[derive(Clone, Debug, PartialEq, serde::Deserialize)]
pub struct Sql {
    pub files: Vec<String>,
    pub(crate) host: String,
    pub(crate) port: u16,
    pub(crate) username: String,
    pub(crate) password: String,
    pub(crate) database: String,
}

impl<'de> Deserialize<'de> for Sql {
    fn deserialize(value: &mut Value<'de>) -> Result<Self, DeserError> {
        let mut mh = TableHelper::new(value)?;

        let _kind: String = mh.required("kind")?;
        let files = mh.required("files")?;
        let port = mh.required("port")?;
        let host = mh.required("host")?;
        let database = mh.required("database")?;
        let username = mh.optional("username").unwrap_or_default();
        let password = mh.optional("password").unwrap_or_default();

        mh.finalize(None)?;

        Ok(Self {
            files,
            host,
            port,
            username,
            password,
            database,
        })
    }
}

impl<'de> Deserialize<'de> for Grpc {
    fn deserialize(value: &mut Value<'de>) -> Result<Self, DeserError> {
        let mut mh = TableHelper::new(value)?;

        let _kind: String = mh.required("kind")?;
        let file = mh.required("file")?;
        let service = mh.required("service")?;
        let endpoint = mh.required("endpoint")?;
        let mutations = mh.optional("mutations").unwrap_or_default();

        mh.finalize(None)?;

        Ok(Self {
            file,
            service,
            mutations,
            endpoint,
        })
    }
}

impl<'de> Deserialize<'de> for Source {
    fn deserialize(value: &mut Value<'de>) -> Result<Self, DeserError> {
        let Some(table) = value.as_table() else {
            return Err(expected(
                "a table",
                ValueInner::Table(BTreeMap::default()),
                value.span,
            )
            .into());
        };

        match table
            .get(&Key {
                name: Cow::Borrowed("kind"),
                span: Span::default(),
            })
            .and_then(|source| source.as_str())
        {
            Some("grpc") => Ok(Source::Grpc(Grpc::deserialize(value)?)),
            Some("sql") => Ok(Source::Sql(Sql::deserialize(value)?)),
            Some(other) => unimplemented!("`{other}`"),
            None => Err(expected(
                "a table with key `kind`",
                ValueInner::String(Cow::Borrowed("Not found")),
                value.span,
            )
            .into()),
        }
    }
}

impl<'de> Deserialize<'de> for Sources {
    fn deserialize(value: &mut Value<'de>) -> Result<Self, DeserError> {
        let mut sources = IndexMap::default();
        let mut errors = Vec::new();

        match value.take() {
            ValueInner::Table(mut table) => {
                let Some(table) = table.get_mut(&Key {
                    name: Cow::Borrowed("sources"),
                    span: Span::new(0, 7),
                }) else {
                    return Err(expected(
                        "a table with `[sources]`",
                        ValueInner::String(Cow::Borrowed("Not found")),
                        value.span,
                    )
                    .into());
                };
                let ValueInner::Table(mut table) = table.take() else {
                    return Err(expected(
                        "a table",
                        ValueInner::Table(BTreeMap::default()),
                        value.span,
                    )
                    .into());
                };
                for (key, value) in &mut table {
                    match Source::deserialize(value) {
                        Ok(value) => {
                            sources.insert(key.name.to_string(), value);
                        }
                        Err(mut err) => {
                            errors.append(&mut err.errors);
                        }
                    }
                }

                if !errors.is_empty() {
                    return Err(DeserError { errors });
                }
            }
            other => return Err(expected("a table", other, value.span).into()),
        }

        Ok(Self { sources })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn should_parse_as_grpc() {
        let source = r#"
[sources.users]
kind = "grpc"
file = "users.proto"
service = "com.example.users.UserService"
endpoint = "localhost:50051"
"#;

        let mut value = toml_span::parse(source).unwrap();
        let result = Sources::deserialize(&mut value).unwrap();

        assert_eq!(
            result,
            Sources {
                sources: [(
                    "users".into(),
                    Source::Grpc(Grpc {
                        file: "users.proto".to_string(),
                        service: "com.example.users.UserService".to_string(),
                        endpoint: "localhost:50051".to_string(),
                        mutations: Vec::new(),
                    })
                )]
                .into_iter()
                .collect()
            }
        )
    }

    #[test]
    fn should_parse_as_grpc_with_mutations() {
        let source = r#"
[sources.users]
kind = "grpc"
file = "users.proto"
service = "com.example.users.UserService"
endpoint = "localhost:50051"
mutations = ["CreateUser"]
"#;

        let mut value = toml_span::parse(source).unwrap();
        let result = Sources::deserialize(&mut value).unwrap();

        assert_eq!(
            result,
            Sources {
                sources: [(
                    "users".into(),
                    Source::Grpc(Grpc {
                        file: "users.proto".to_string(),
                        service: "com.example.users.UserService".to_string(),
                        endpoint: "localhost:50051".to_string(),
                        mutations: vec!["CreateUser".to_string()],
                    })
                )]
                .into_iter()
                .collect()
            }
        )
    }

    #[test]
    fn should_parse_as_sql() {
        let source = r#"
[sources.user_db]
kind = "sql"
files = ["users.sql"]
host = "host"
port = 250
username = "user"
password = "pswd"
database = "db"
"#;

        let mut value = toml_span::parse(source).unwrap();
        let result = Sources::deserialize(&mut value).unwrap();

        assert_eq!(
            result,
            Sources {
                sources: [(
                    "user_db".into(),
                    Source::Sql(Sql {
                        files: vec!["users.sql".to_string()],
                        host: "host".to_string(),
                        port: 250,
                        username: "user".to_string(),
                        password: "pswd".to_string(),
                        database: "db".to_string()
                    })
                )]
                .into_iter()
                .collect()
            }
        )
    }
}
