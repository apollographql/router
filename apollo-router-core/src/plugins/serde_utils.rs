use http::header::HeaderName;
use http::HeaderValue;
use regex::Regex;
use serde::de::{Error, Visitor};
use serde::{de, Deserializer};
use std::fmt::Formatter;
use std::str::FromStr;

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
        HeaderName::try_from(v).map_err(|e| de::Error::custom(format!("Invalid header name {}", e)))
    }
}

pub fn deserialize_header_name<'de, D>(deserializer: D) -> Result<HeaderName, D::Error>
where
    D: Deserializer<'de>,
{
    deserializer.deserialize_str(HeaderNameVisitor)
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
        HeaderValue::try_from(v)
            .map_err(|e| de::Error::custom(format!("Invalid header value {}", e)))
    }
}

pub fn deserialize_header_value<'de, D>(deserializer: D) -> Result<HeaderValue, D::Error>
where
    D: Deserializer<'de>,
{
    deserializer.deserialize_str(HeaderValueVisitor)
}

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
            Regex::from_str(v).map_err(|e| de::Error::custom(format!("{}", e)))
        }
    }
    deserializer.deserialize_str(RegexVisitor)
}
