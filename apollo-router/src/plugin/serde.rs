//! serde support for commonly used data structures.

use std::fmt::Formatter;
use std::str::FromStr;

use apollo_redaction::Redacted;
use http::HeaderValue;
use http::header::HeaderName;
use regex::Regex;
use serde::Deserializer;
use serde::Serialize;
use serde::Serializer;
use serde::de;
use serde::de::Error;
use serde::de::SeqAccess;
use serde::de::Visitor;

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

impl Visitor<'_> for HeaderNameVisitor {
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

struct HeaderValueVisitor;

impl Visitor<'_> for HeaderValueVisitor {
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

    impl Visitor<'_> for RegexVisitor {
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

pub(crate) fn deserialize_jsonpath<'de, D>(
    deserializer: D,
) -> Result<serde_json_bytes::path::JsonPathInst, D::Error>
where
    D: serde::Deserializer<'de>,
{
    deserializer.deserialize_str(JSONPathVisitor)
}

struct JSONPathVisitor;

impl serde::de::Visitor<'_> for JSONPathVisitor {
    type Value = serde_json_bytes::path::JsonPathInst;

    fn expecting(&self, formatter: &mut Formatter) -> std::fmt::Result {
        write!(formatter, "a JSON path")
    }

    fn visit_str<E>(self, s: &str) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        serde_json_bytes::path::JsonPathInst::from_str(s).map_err(serde::de::Error::custom)
    }
}

/// Serialize the unredacted value of an `Option<Redacted<T>>` field.
///
/// Use with `#[serde(serialize_with)]` when serialization must preserve the original setting.
/// The output contains plaintext secrets and must stay out of diagnostics.
pub(crate) fn serialize_redacted_option<T, R, S>(
    value: &Option<Redacted<T, R>>,
    serializer: S,
) -> Result<S::Ok, S::Error>
where
    T: Serialize,
    S: Serializer,
{
    value.as_ref().map(Redacted::unredact).serialize(serializer)
}

/// Deserialize a secret string, reporting a type mismatch without the rejected value.
///
/// `Redacted`'s own `Deserialize` replaces every error with `failed to parse 'String'`, and
/// serde's usual message would include the value (``invalid type: integer `12345` ``). This
/// reports only its kind, for example `invalid type: integer, expected a string`.
pub(crate) fn deserialize_redacted_string<'de, D>(
    deserializer: D,
) -> Result<Redacted<String>, D::Error>
where
    D: Deserializer<'de>,
{
    // `deserialize_any` lets the visitor report a mismatch: `deserialize_string` would let the
    // deserializer reject the value itself, and its message includes the value.
    deserializer
        .deserialize_any(SecretStringVisitor)
        .map(Redacted::new)
}

/// Removes the `default: null` that a serde `default` adds to an optional secret's schema.
///
/// `deserialize_with` makes a field required unless it also has a serde `default`. Where the
/// field also has `serialize_with`, schemars uses it to advertise that default, which these fields
/// never declared.
pub(crate) fn without_schema_default(schema: &mut schemars::Schema) {
    schema.remove("default");
}

/// Deserialize an optional secret string; see [`deserialize_redacted_string`].
pub(crate) fn deserialize_redacted_string_option<'de, D>(
    deserializer: D,
) -> Result<Option<Redacted<String>>, D::Error>
where
    D: Deserializer<'de>,
{
    struct OptionalSecretStringVisitor;

    impl<'de> Visitor<'de> for OptionalSecretStringVisitor {
        type Value = Option<Redacted<String>>;

        fn expecting(&self, formatter: &mut Formatter) -> std::fmt::Result {
            formatter.write_str("a string or null")
        }

        fn visit_none<E: Error>(self) -> Result<Self::Value, E> {
            Ok(None)
        }

        fn visit_unit<E: Error>(self) -> Result<Self::Value, E> {
            Ok(None)
        }

        fn visit_some<D: Deserializer<'de>>(
            self,
            deserializer: D,
        ) -> Result<Self::Value, D::Error> {
            deserialize_redacted_string(deserializer).map(Some)
        }
    }

    deserializer.deserialize_option(OptionalSecretStringVisitor)
}

struct SecretStringVisitor;

impl SecretStringVisitor {
    fn mismatch<E: Error>(kind: &str) -> E {
        E::custom(format_args!("invalid type: {kind}, expected a string"))
    }
}

impl<'de> Visitor<'de> for SecretStringVisitor {
    type Value = String;

    fn expecting(&self, formatter: &mut Formatter) -> std::fmt::Result {
        formatter.write_str("a string")
    }

    fn visit_str<E: Error>(self, value: &str) -> Result<Self::Value, E> {
        Ok(value.to_string())
    }

    fn visit_string<E: Error>(self, value: String) -> Result<Self::Value, E> {
        Ok(value)
    }

    fn visit_bool<E: Error>(self, _: bool) -> Result<Self::Value, E> {
        Err(Self::mismatch("boolean"))
    }

    fn visit_i64<E: Error>(self, _: i64) -> Result<Self::Value, E> {
        Err(Self::mismatch("integer"))
    }

    fn visit_i128<E: Error>(self, _: i128) -> Result<Self::Value, E> {
        Err(Self::mismatch("integer"))
    }

    fn visit_u64<E: Error>(self, _: u64) -> Result<Self::Value, E> {
        Err(Self::mismatch("integer"))
    }

    fn visit_u128<E: Error>(self, _: u128) -> Result<Self::Value, E> {
        Err(Self::mismatch("integer"))
    }

    fn visit_f64<E: Error>(self, _: f64) -> Result<Self::Value, E> {
        Err(Self::mismatch("floating point number"))
    }

    fn visit_bytes<E: Error>(self, _: &[u8]) -> Result<Self::Value, E> {
        Err(Self::mismatch("byte array"))
    }

    fn visit_none<E: Error>(self) -> Result<Self::Value, E> {
        Err(Self::mismatch("null"))
    }

    fn visit_unit<E: Error>(self) -> Result<Self::Value, E> {
        Err(Self::mismatch("null"))
    }

    fn visit_some<D: Deserializer<'de>>(self, _: D) -> Result<Self::Value, D::Error> {
        Err(Self::mismatch("optional value"))
    }

    fn visit_seq<A: SeqAccess<'de>>(self, _: A) -> Result<Self::Value, A::Error> {
        Err(Self::mismatch("sequence"))
    }

    fn visit_map<A: de::MapAccess<'de>>(self, _: A) -> Result<Self::Value, A::Error> {
        Err(Self::mismatch("map"))
    }

    fn visit_enum<A: de::EnumAccess<'de>>(self, _: A) -> Result<Self::Value, A::Error> {
        Err(Self::mismatch("enum"))
    }
}

#[cfg(test)]
mod secret_string_tests {
    use serde::Deserialize;
    use serde_json::json;

    use super::*;

    #[derive(Debug, Deserialize)]
    struct Secrets {
        #[serde(deserialize_with = "deserialize_redacted_string")]
        required: Redacted<String>,
        #[serde(deserialize_with = "deserialize_redacted_string_option", default)]
        optional: Option<Redacted<String>>,
    }

    fn error_for(input: serde_json::Value) -> String {
        serde_json::from_value::<Secrets>(input)
            .expect_err("the input has the wrong type")
            .to_string()
    }

    #[test]
    fn strings_are_accepted() {
        let secrets: Secrets =
            serde_json::from_value(json!({"required": "one", "optional": "two"})).unwrap();
        assert_eq!(secrets.required.unredact(), "one");
        assert_eq!(secrets.optional.unwrap().unredact(), "two");

        let secrets: Secrets = serde_json::from_value(json!({"required": "one"})).unwrap();
        assert!(secrets.optional.is_none());
        let secrets: Secrets =
            serde_json::from_value(json!({"required": "one", "optional": null})).unwrap();
        assert!(secrets.optional.is_none());
    }

    #[test]
    fn a_type_mismatch_names_its_kind_without_the_value() {
        for (value, kind) in [
            (json!(12345), "integer"),
            (json!(-12345), "integer"),
            (json!(123.45), "floating point number"),
            (json!(true), "boolean"),
            (json!(null), "null"),
            (json!(["12345"]), "sequence"),
            (json!({"value": "12345"}), "map"),
        ] {
            let error = error_for(json!({ "required": value }));
            assert!(
                error.contains(&format!("invalid type: {kind}, expected a string")),
                "{error}"
            );
            assert!(!error.contains("12345"), "{error}");
            assert!(!error.contains("123.45"), "{error}");
        }

        let error = error_for(json!({"required": "one", "optional": 12345}));
        assert!(
            error.contains("invalid type: integer, expected a string"),
            "{error}"
        );
        assert!(!error.contains("12345"), "{error}");
    }
}
