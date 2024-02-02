//! serde support for commonly used data structures.

use std::fmt::Formatter;
use std::str::FromStr;

use access_json::JSONQuery;
use http::header::HeaderName;
use http::HeaderValue;
use jsonpath_rust::JsonPathInst;
use regex::Regex;
use serde::de;
use serde::de::Error;
use serde::de::SeqAccess;
use serde::de::Visitor;
use serde::Deserializer;

/// De-serialize an optional [`HeaderName`].
pub fn deserialize_option_header_name<'de, D>(
    deserializer: D,
) -> Result<Option<HeaderName>, D::Error>
where
    D: Deserializer<'de>,
{
    struct OptionHeaderNameVisitor;

    impl<'de> Visitor<'de> for OptionHeaderNameVisitor {
        type Value = Option<HeaderName>;

        fn expecting(&self, formatter: &mut Formatter) -> std::fmt::Result {
            formatter.write_str("struct HeaderName")
        }

        fn visit_none<E>(self) -> Result<Self::Value, E>
        where
            E: de::Error,
        {
            Ok(None)
        }

        fn visit_some<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
        where
            D: de::Deserializer<'de>,
        {
            Ok(Some(deserializer.deserialize_str(HeaderNameVisitor)?))
        }
    }
    deserializer.deserialize_option(OptionHeaderNameVisitor)
}

/// De-serialize a vector of [`HeaderName`].
pub fn deserialize_vec_header_name<'de, D>(deserializer: D) -> Result<Vec<HeaderName>, D::Error>
where
    D: Deserializer<'de>,
{
    struct VecHeaderNameVisitor;

    impl<'de> Visitor<'de> for VecHeaderNameVisitor {
        type Value = Vec<HeaderName>;

        fn expecting(&self, formatter: &mut Formatter) -> std::fmt::Result {
            formatter.write_str("struct HeaderName")
        }

        fn visit_seq<A>(self, mut seq: A) -> Result<Self::Value, A::Error>
        where
            A: SeqAccess<'de>,
        {
            let mut result = Vec::new();
            while let Some(element) = seq.next_element::<String>()? {
                let header_name = HeaderNameVisitor.visit_string(element)?;
                result.push(header_name);
            }
            Ok(result)
        }
    }
    deserializer.deserialize_seq(VecHeaderNameVisitor)
}

/// De-serialize an optional [`HeaderValue`].
pub fn deserialize_option_header_value<'de, D>(
    deserializer: D,
) -> Result<Option<HeaderValue>, D::Error>
where
    D: Deserializer<'de>,
{
    struct OptionHeaderValueVisitor;

    impl<'de> Visitor<'de> for OptionHeaderValueVisitor {
        type Value = Option<HeaderValue>;

        fn expecting(&self, formatter: &mut Formatter) -> std::fmt::Result {
            formatter.write_str("struct HeaderValue")
        }

        fn visit_none<E>(self) -> Result<Self::Value, E>
        where
            E: de::Error,
        {
            Ok(None)
        }

        fn visit_some<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
        where
            D: de::Deserializer<'de>,
        {
            Ok(Some(deserializer.deserialize_str(HeaderValueVisitor)?))
        }
    }

    deserializer.deserialize_option(OptionHeaderValueVisitor)
}

#[derive(Default)]
struct HeaderNameVisitor;

impl<'de> Visitor<'de> for HeaderNameVisitor {
    type Value = HeaderName;

    fn expecting(&self, formatter: &mut Formatter) -> std::fmt::Result {
        formatter.write_str("struct HeaderName")
    }

    fn visit_str<E>(self, v: &str) -> Result<Self::Value, E>
    where
        E: Error,
    {
        HeaderName::try_from(v).map_err(|e| de::Error::custom(format!("Invalid header name {e}")))
    }
}

/// De-serialize a [`HeaderName`].
pub fn deserialize_header_name<'de, D>(deserializer: D) -> Result<HeaderName, D::Error>
where
    D: Deserializer<'de>,
{
    deserializer.deserialize_str(HeaderNameVisitor)
}

struct JSONQueryVisitor;

impl<'de> Visitor<'de> for JSONQueryVisitor {
    type Value = JSONQuery;

    fn expecting(&self, formatter: &mut Formatter) -> std::fmt::Result {
        formatter.write_str("struct JSONQuery")
    }

    fn visit_str<E>(self, v: &str) -> Result<Self::Value, E>
    where
        E: Error,
    {
        JSONQuery::parse(v)
            .map_err(|e| de::Error::custom(format!("Invalid JSON query path for '{v}' {e}")))
    }
}

/// De-serialize a [`JSONQuery`].
pub fn deserialize_json_query<'de, D>(deserializer: D) -> Result<JSONQuery, D::Error>
where
    D: Deserializer<'de>,
{
    deserializer.deserialize_str(JSONQueryVisitor)
}

struct HeaderValueVisitor;

impl<'de> Visitor<'de> for HeaderValueVisitor {
    type Value = HeaderValue;

    fn expecting(&self, formatter: &mut Formatter) -> std::fmt::Result {
        formatter.write_str("struct HeaderValue")
    }

    fn visit_str<E>(self, v: &str) -> Result<Self::Value, E>
    where
        E: Error,
    {
        HeaderValue::try_from(v).map_err(|e| de::Error::custom(format!("Invalid header value {e}")))
    }
}

/// De-serialize a [`HeaderValue`].
pub fn deserialize_header_value<'de, D>(deserializer: D) -> Result<HeaderValue, D::Error>
where
    D: Deserializer<'de>,
{
    deserializer.deserialize_str(HeaderValueVisitor)
}

/// De-serialize a [`Regex`].
pub fn deserialize_regex<'de, D>(deserializer: D) -> Result<Regex, D::Error>
where
    D: Deserializer<'de>,
{
    struct RegexVisitor;

    impl<'de> Visitor<'de> for RegexVisitor {
        type Value = Regex;

        fn expecting(&self, formatter: &mut Formatter) -> std::fmt::Result {
            formatter.write_str("struct Regex")
        }

        fn visit_str<E>(self, v: &str) -> Result<Self::Value, E>
        where
            E: Error,
        {
            Regex::from_str(v).map_err(|e| de::Error::custom(format!("{e}")))
        }
    }
    deserializer.deserialize_str(RegexVisitor)
}

pub(crate) fn deserialize_jsonpath<'de, D>(deserializer: D) -> Result<JsonPathInst, D::Error>
where
    D: serde::Deserializer<'de>,
{
    deserializer.deserialize_str(JSONPathVisitor)
}

struct JSONPathVisitor;

impl<'de> serde::de::Visitor<'de> for JSONPathVisitor {
    type Value = JsonPathInst;

    fn expecting(&self, formatter: &mut Formatter) -> std::fmt::Result {
        write!(formatter, "a JSON path")
    }

    fn visit_str<E>(self, s: &str) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        JsonPathInst::from_str(s).map_err(serde::de::Error::custom)
    }
}
