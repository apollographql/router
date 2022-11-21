//!  `@live` implementation

// With regards to ELv2 licensing, this entire file is license key functionality

use derivative::Derivative;
use serde::de;
use serde::de::MapAccess;
use serde::de::Visitor;
use serde::Deserialize;
use serde::Serialize;

#[cfg(feature = "experimental_cache")]
pub(crate) mod layer;

#[derive(Clone, Derivative)]
#[derivative(Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum QueryCursor {
    Latest,
    Cursor(Cursor),
}

impl Default for QueryCursor {
    fn default() -> Self {
        QueryCursor::Latest
    }
}

impl Serialize for QueryCursor {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        match self {
            QueryCursor::Latest => serializer.serialize_str("latest"),
            QueryCursor::Cursor(cursor) => cursor.serialize(serializer),
        }
    }
}

impl<'de> Deserialize<'de> for QueryCursor {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        deserializer.deserialize_any(QueryCursorVisitor)
    }
}

struct QueryCursorVisitor;

impl<'de> Visitor<'de> for QueryCursorVisitor {
    type Value = QueryCursor;

    fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
        formatter.write_str("either the string \"latest\" or a cursor object")
    }

    fn visit_str<E>(self, v: &str) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        println!("[{}] QueryCursorVisitor: \"{}\"", line!(), v);
        if v == "latest" {
            Ok(QueryCursor::Latest)
        } else {
            Err(E::custom("expected the string \"latest\""))
        }
    }

    fn visit_map<A>(self, map: A) -> Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let c: Cursor = Deserialize::deserialize(de::value::MapAccessDeserializer::new(map))?;
        Ok(QueryCursor::Cursor(c))
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[non_exhaustive]
pub struct Cursor {
    pub request: String,
    pub result: ResultCursor,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub diff: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ResultCursor {
    Latest,
    Hash(String),
}

impl Default for ResultCursor {
    fn default() -> Self {
        ResultCursor::Latest
    }
}

impl Serialize for ResultCursor {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        match self {
            ResultCursor::Latest => serializer.serialize_str("latest"),
            ResultCursor::Hash(s) => serializer.serialize_str(s),
        }
    }
}

impl<'de> Deserialize<'de> for ResultCursor {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        deserializer.deserialize_str(ResultCursorVisitor)
    }
}

struct ResultCursorVisitor;

impl<'de> Visitor<'de> for ResultCursorVisitor {
    type Value = ResultCursor;

    fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
        formatter.write_str("either the string \"latest\" or a response hash string")
    }

    fn visit_str<E>(self, v: &str) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        if v == "latest" {
            Ok(ResultCursor::Latest)
        } else {
            //FIXME: validate the hash format
            Ok(ResultCursor::Hash(v.to_string()))
        }
    }
}

#[cfg(test)]
mod test {

    use super::*;

    #[test]
    fn querycursor_serialize() {
        assert_eq!(
            serde_json::to_string(&QueryCursor::Latest).unwrap(),
            r#""latest""#
        );

        assert_eq!(
            serde_json::to_string(&QueryCursor::Cursor(Cursor {
                request: "abcde".to_string(),
                result: ResultCursor::Latest,
                diff: None
            }))
            .unwrap(),
            r#"{"request":"abcde","result":"latest"}"#
        );

        assert_eq!(
            serde_json::to_string(&QueryCursor::Cursor(Cursor {
                request: "abcde".to_string(),
                result: ResultCursor::Hash("AAAA".to_string()),
                diff: Some("1234".to_string())
            }))
            .unwrap(),
            r#"{"request":"abcde","result":"AAAA","diff":"1234"}"#
        );
    }

    #[test]
    fn querycursor_deserialize() {
        assert_eq!(
            serde_json::from_str::<QueryCursor>(r#""latest""#).unwrap(),
            QueryCursor::Latest
        );
        assert!(serde_json::from_str::<QueryCursor>(r#""aaa""#).is_err());
    }

    #[test]
    fn resultcursor_serialize() {
        assert_eq!(
            serde_json::to_string(&ResultCursor::Latest).unwrap(),
            r#""latest""#
        );

        assert_eq!(
            serde_json::to_string(&ResultCursor::Hash("abcde12".to_string())).unwrap(),
            r#""abcde12""#
        );
    }

    #[test]
    fn resultcursor_deserialize() {
        assert_eq!(
            serde_json::from_str::<ResultCursor>(r#""latest""#).unwrap(),
            ResultCursor::Latest
        );
        assert_eq!(
            serde_json::from_str::<ResultCursor>(r#""aaa""#).unwrap(),
            ResultCursor::Hash("aaa".to_string())
        );
    }
}
