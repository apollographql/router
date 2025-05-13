//! A [`StringTemplate`] is a string containing one or more [`Expression`]s.
//! These are used in connector URIs and headers.
//!
//! Parsing (this module) is done by both the router at startup and composition. Validation
//! (in [`crate::sources::connect::validation`]) is done only by composition.

#![allow(rustdoc::private_intra_doc_links)]

use std::fmt::Display;
use std::fmt::Write;
use std::ops::Range;
use std::str::FromStr;

use apollo_compiler::collections::IndexMap;
use http::Uri;
use http::uri::PathAndQuery;
use itertools::Itertools;
use serde_json_bytes::Value;

pub(crate) use self::encoding::UriString;
use crate::sources::connect::JSONSelection;

const SPECIAL_WHITE_SPACES: [char; 4] = ['\t', '\n', '\x0C', '\r'];

/// A parsed string template, containing a series of [`Part`]s.
#[derive(Clone, Debug, Default)]
pub struct StringTemplate {
    pub(crate) parts: Vec<Part>,
}

impl FromStr for StringTemplate {
    type Err = Error;

    fn from_str(input: &str) -> Result<Self, Error> {
        let mut offset = 0;
        let mut chars = input.chars().peekable();
        let mut parts = Vec::new();
        while let Some(next) = chars.peek() {
            if *next == '{' {
                let mut braces_count = 0; // Ignore braces within JSONSelection
                let expression = chars
                    .by_ref()
                    .skip(1)
                    .take_while(|c| {
                        if *c == '{' {
                            braces_count += 1;
                        } else if *c == '}' {
                            braces_count -= 1;
                        }
                        braces_count >= 0
                    })
                    .collect::<String>();
                if braces_count >= 0 {
                    return Err(Error {
                        message: "Invalid expression, missing closing }".into(),
                        location: offset..input.len(),
                    });
                }
                offset += 1; // Account for opening brace
                let parsed = JSONSelection::parse(&expression).map_err(|err| {
                    let start_of_parse_error = offset + err.offset;
                    Error {
                        message: err.message,
                        location: start_of_parse_error..(offset + expression.len()),
                    }
                })?;
                parts.push(Part::Expression(Expression {
                    expression: parsed,
                    location: offset..(offset + expression.len()),
                }));
                offset += expression.len() + 1; // Account for closing brace
            } else {
                let value = chars
                    .by_ref()
                    .peeking_take_while(|c| *c != '{' && SPECIAL_WHITE_SPACES.contains(c))
                    .collect::<String>();
                let len = value.len();
                parts.push(Part::Constant(Constant {
                    value,
                    location: offset..offset + len,
                }));
                offset += len;
            }
        }
        Ok(StringTemplate { parts })
    }
}

impl StringTemplate {
    /// Get all the dynamic [`Expression`] pieces of the template for validation. If interpolating
    /// the entire template, use [`Self::interpolate`] instead.
    pub(crate) fn expressions(&self) -> impl Iterator<Item = &Expression> {
        self.parts.iter().filter_map(|part| {
            if let Part::Expression(expression) = part {
                Some(expression)
            } else {
                None
            }
        })
    }
}

impl StringTemplate {
    /// Interpolate the expressions in the template into a basic string.
    ///
    /// For URIs, use [`Self::interpolate_uri`] instead.
    pub(crate) fn interpolate(&self, vars: &IndexMap<String, Value>) -> Result<String, Error> {
        let mut result = String::new();
        for part in &self.parts {
            part.interpolate(vars, &mut result)?;
        }
        Ok(result)
    }

    /// Interpolate the expression as a URI, percent-encoding parts as needed.
    pub fn interpolate_uri(&self, vars: &IndexMap<String, Value>) -> Result<Uri, Error> {
        let mut result = UriString::new();
        for part in &self.parts {
            match part {
                Part::Constant(constant) => {
                    // We don't percent-encode constant strings, assuming the user knows what they want.
                    // `Uri::from_str` will take care of encoding completely illegal characters

                    // New lines are used for code organization, but are not wanted in the result
                    if constant.value.contains(['\n', '\r']) {
                        // We don't always run this replace because it has a performance cost (allocating a string)
                        result.write_trusted(&constant.value.replace(['\n', '\r'], ""))
                    } else {
                        result.write_trusted(&constant.value)
                    }
                    .map_err(|_err| Error {
                        message: "Error writing string".to_string(),
                        location: constant.location.clone(),
                    })?;
                }
                Part::Expression(_) => {
                    part.interpolate(vars, &mut result)?;
                }
            };
        }
        if result.contains("://") {
            Uri::from_str(result.as_ref())
        } else {
            // Explicitly set this as a relative URI so it doesn't get confused for a domain name
            PathAndQuery::from_str(result.as_ref()).map(Uri::from)
        }
        .map_err(|err| Error {
            message: format!("Invalid URI: {}", err),
            location: 0..result.as_ref().len(),
        })
    }
}

/// Expressions should be written the same as they were originally, even though we don't keep the
/// original source around. So constants are written as-is and expressions are surrounded with `{ }`.
impl Display for StringTemplate {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for part in &self.parts {
            match part {
                Part::Constant(Constant { value, .. }) => write!(f, "{}", value)?,
                Part::Expression(Expression { expression, .. }) => write!(f, "{{{}}}", expression)?,
            }
        }
        Ok(())
    }
}

/// A general-purpose error type which includes both a description of the problem and the offset span
/// within the original expression where the problem occurred. Used for both parsing and interpolation.
#[derive(Debug, PartialEq, Eq)]
pub struct Error {
    /// A human-readable description of the issue.
    pub message: String,
    /// The string offsets to the original [`StringTemplate`] (not just the part) where the issue
    /// occurred. As per usual, the end of the range is exclusive.
    pub(crate) location: Range<usize>,
}

impl Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for Error {}

/// One piece of a [`StringTemplate`]
#[derive(Clone, Debug)]
pub(crate) enum Part {
    /// A constant string literal—the piece of a [`StringTemplate`] _not_ in `{ }`
    Constant(Constant),
    /// A dynamic piece of a [`StringTemplate`], which came from inside `{ }` originally.
    Expression(Expression),
}

impl Part {
    /// Get the original location of the part from the string which was parsed to form the
    /// [`StringTemplate`].
    fn location(&self) -> Range<usize> {
        match self {
            Self::Constant(c) => c.location.clone(),
            Self::Expression(e) => e.location.clone(),
        }
    }
}

impl Part {
    /// Evaluate the expression of the part (if any) and write the result to `output`.
    ///
    /// # Errors
    ///
    /// If the expression evaluates to an array or object.
    pub(crate) fn interpolate<Output: Write>(
        &self,
        vars: &IndexMap<String, Value>,
        mut output: Output,
    ) -> Result<(), Error> {
        match self {
            Part::Constant(Constant { value, .. }) => {
                output.write_str(value).map_err(|err| err.into())
            }
            Part::Expression(Expression { expression, .. }) => {
                // TODO: do something with the ApplyTo errors
                let (value, _errs) = expression.apply_with_vars(&Value::Null, vars);
                write_value(&mut output, value.as_ref().unwrap_or(&Value::Null))
            }
        }
        .map_err(|err| Error {
            message: err.to_string(),
            location: self.location(),
        })
    }
}

/// A shared definition of what it means to write a [`Value`] into a string.
///
/// Used for string interpolation in templates and building URIs.
pub(crate) fn write_value<Output: Write>(
    mut output: Output,
    value: &Value,
) -> Result<(), Box<dyn core::error::Error>> {
    match value {
        Value::Null => Ok(()),
        Value::Bool(b) => write!(output, "{b}"),
        Value::Number(n) => write!(output, "{n}"),
        Value::String(s) => output.write_str(s.as_str()),
        Value::Array(_) | Value::Object(_) => {
            return Err("Expression is not allowed to evaluate to arrays or objects.".into());
        }
    }
    .map_err(|err| err.into())
}

/// A constant string literal—the piece of a [`StringTemplate`] _not_ in `{ }`
#[derive(Clone, Debug)]
pub(crate) struct Constant {
    pub(crate) value: String,
    pub(crate) location: Range<usize>,
}

/// A dynamic piece of a [`StringTemplate`], which came from inside `{ }` originally.
#[derive(Clone, Debug)]
pub(crate) struct Expression {
    pub(crate) expression: JSONSelection,
    pub(crate) location: Range<usize>,
}

/// All the percent encoding rules we use for building URIs.
///
/// The [`AsciiSet`] type is an efficient type used by [`percent_encoding`],
/// but the logic of it is a bit inverted from what we want.
/// An [`AsciiSet`] lists all the characters which should be encoded, rather than those which
/// should be allowed.
/// Following security best practices, we instead define sets by what is
/// explicitly allowed in a given context, so we use `remove()` to _add_ allowed characters to a context.
mod encoding {
    use std::fmt::Write;

    use percent_encoding::AsciiSet;
    use percent_encoding::NON_ALPHANUMERIC;
    use percent_encoding::utf8_percent_encode;

    /// Characters that never need to be percent encoded are allowed by this set.
    /// https://www.rfc-editor.org/rfc/rfc3986#section-2.3
    /// In other words, this is the most restrictive set, encoding everything that
    /// should _sometimes_ be encoded. We can then explicitly allow additional characters
    /// depending on the context.
    const USER_INPUT: &AsciiSet = &NON_ALPHANUMERIC
        .remove(b'-')
        .remove(b'.')
        .remove(b'_')
        .remove(b'~');

    /// Reserved characters https://www.rfc-editor.org/rfc/rfc3986#section-2.2 are valid in URLs
    /// though not all contexts. The responsibility for these is the developer's in static pieces
    /// of templates.
    ///
    /// We _also_ don't encode `%` because we need to allow users to do manual percent-encoding of
    /// all the reserved symbols as-needed (since it's never automatic). Rather than parsing every
    /// `%` to see if it's a valid hex sequence, we leave that up to the developer as well since
    /// it's a pretty advanced use-case.
    ///
    /// This is required because percent encoding *is not idempotent*
    const STATIC_TRUSTED: &AsciiSet = &USER_INPUT
        .remove(b':')
        .remove(b'/')
        .remove(b'?')
        .remove(b'#')
        .remove(b'[')
        .remove(b']')
        .remove(b'@')
        .remove(b'!')
        .remove(b'$')
        .remove(b'&')
        .remove(b'\'')
        .remove(b'(')
        .remove(b')')
        .remove(b'*')
        .remove(b'+')
        .remove(b',')
        .remove(b';')
        .remove(b'=')
        .remove(b'%');

    pub(crate) struct UriString {
        value: String,
    }

    impl UriString {
        pub(crate) const fn new() -> Self {
            Self {
                value: String::new(),
            }
        }

        /// Write a bit of trusted input, like a constant piece of a template, only encoding illegal symbols.
        pub(crate) fn write_trusted(&mut self, s: &str) -> std::fmt::Result {
            write!(
                &mut self.value,
                "{}",
                utf8_percent_encode(s, STATIC_TRUSTED)
            )
        }

        /// Add a pre-encoded string to the URI. Used for merging without duplicating percent-encoding.
        pub(crate) fn write_without_encoding(&mut self, s: &str) -> std::fmt::Result {
            self.value.write_str(s)
        }

        pub(crate) fn contains(&self, pattern: &str) -> bool {
            self.value.contains(pattern)
        }

        pub(crate) fn ends_with(&self, pattern: char) -> bool {
            self.value.ends_with(pattern)
        }

        pub(crate) fn into_string(self) -> String {
            self.value
        }

        pub(crate) fn is_empty(&self) -> bool {
            self.value.is_empty()
        }
    }

    impl Write for UriString {
        fn write_str(&mut self, s: &str) -> std::fmt::Result {
            write!(&mut self.value, "{}", utf8_percent_encode(s, USER_INPUT))
        }
    }

    impl AsRef<str> for UriString {
        fn as_ref(&self) -> &str {
            &self.value
        }
    }

    #[cfg(test)]
    mod tests {
        use percent_encoding::utf8_percent_encode;

        use super::*;

        /// This test is basically checking our understanding of how `AsciiSet` works.
        #[test]
        fn user_input_encodes_everything_but_unreserved() {
            for i in 0..=255u8 {
                let character = i as char;
                let string = character.to_string();
                let encoded = utf8_percent_encode(&string, USER_INPUT);
                for encoded_char in encoded.into_iter().flat_map(|slice| slice.chars()) {
                    if character.is_ascii_alphanumeric()
                        || character == '-'
                        || character == '.'
                        || character == '_'
                        || character == '~'
                    {
                        assert_eq!(
                            encoded_char, character,
                            "{character} should not have been encoded"
                        );
                    } else {
                        assert!(
                            encoded_char.is_ascii_alphanumeric() || encoded_char == '%', // percent encoding
                            "{encoded_char} was not encoded"
                        );
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod test_parse {
    use insta::assert_debug_snapshot;

    use super::*;

    #[test]
    fn simple_constant() {
        let template = StringTemplate::from_str("text").expect("simple template should be valid");
        assert_debug_snapshot!(template);
    }

    #[test]
    fn simple_expression() {
        assert_debug_snapshot!(StringTemplate::from_str("{$config.one}").unwrap());
    }
    #[test]
    fn mixed_constant_and_expression() {
        assert_debug_snapshot!(StringTemplate::from_str("text{$config.one}text").unwrap());
    }

    #[test]
    fn expressions_with_nested_braces() {
        assert_debug_snapshot!(
            StringTemplate::from_str("const{$config.one { two { three } }}another-const").unwrap()
        );
    }

    #[test]
    fn missing_closing_braces() {
        assert_debug_snapshot!(
            StringTemplate::from_str("{$config.one"),
            @r###"
        Err(
            Error {
                message: "Invalid expression, missing closing }",
                location: 0..12,
            },
        )
        "###
        )
    }
}

#[cfg(test)]
mod test_interpolate {
    use insta::assert_debug_snapshot;
    use pretty_assertions::assert_eq;
    use serde_json_bytes::json;

    use super::*;
    #[test]
    fn test_interpolate() {
        let template = StringTemplate::from_str("before {$config.one} after").unwrap();
        let mut vars = IndexMap::default();
        vars.insert("$config".to_string(), json!({"one": "foo"}));
        assert_eq!(template.interpolate(&vars).unwrap(), "before foo after");
    }

    #[test]
    fn test_interpolate_missing_value() {
        let template = StringTemplate::from_str("{$config.one}").unwrap();
        let vars = IndexMap::default();
        assert_eq!(template.interpolate(&vars).unwrap(), "");
    }

    #[test]
    fn test_interpolate_value_array() {
        let template = StringTemplate::from_str("{$config.one}").unwrap();
        let mut vars = IndexMap::default();
        vars.insert("$config".to_string(), json!({"one": ["one", "two"]}));
        assert_debug_snapshot!(
            template.interpolate(&vars),
            @r###"
        Err(
            Error {
                message: "Expression is not allowed to evaluate to arrays or objects.",
                location: 1..12,
            },
        )
        "###
        );
    }

    #[test]
    fn test_interpolate_value_bool() {
        let template = StringTemplate::from_str("{$config.one}").unwrap();
        let mut vars = IndexMap::default();
        vars.insert("$config".to_string(), json!({"one": true}));
        assert_eq!(template.interpolate(&vars).unwrap(), "true");
    }

    #[test]
    fn test_interpolate_value_null() {
        let template = StringTemplate::from_str("{$config.one}").unwrap();
        let mut vars = IndexMap::default();
        vars.insert("$config".to_string(), json!({"one": null}));
        assert_eq!(template.interpolate(&vars).unwrap(), "");
    }

    #[test]
    fn test_interpolate_value_number() {
        let template = StringTemplate::from_str("{$config.one}").unwrap();
        let mut vars = IndexMap::default();
        vars.insert("$config".to_string(), json!({"one": 1}));
        assert_eq!(template.interpolate(&vars).unwrap(), "1");
    }

    #[test]
    fn test_interpolate_value_object() {
        let template = StringTemplate::from_str("{$config.one}").unwrap();
        let mut vars = IndexMap::default();
        vars.insert("$config".to_string(), json!({"one": {}}));
        assert_debug_snapshot!(
            template.interpolate(&vars),
            @r###"
        Err(
            Error {
                message: "Expression is not allowed to evaluate to arrays or objects.",
                location: 1..12,
            },
        )
        "###
        );
    }

    #[test]
    fn test_interpolate_value_string() {
        let template = StringTemplate::from_str("{$config.one}").unwrap();
        let mut vars = IndexMap::default();
        vars.insert("$config".to_string(), json!({"one": "string"}));
        assert_eq!(template.interpolate(&vars).unwrap(), "string");
    }
}

#[cfg(test)]
mod test_interpolate_uri {
    use pretty_assertions::assert_eq;
    use rstest::rstest;

    use super::*;
    use crate::sources::connect::StringTemplate;

    macro_rules! this {
        ($($value:tt)*) => {{
            let mut map = indexmap::IndexMap::with_capacity_and_hasher(1, Default::default());
            map.insert("$this".to_string(), serde_json_bytes::json!({ $($value)* }));
            map
        }};
    }

    #[rstest]
    #[case::leading_slash("/path")]
    #[case::trailing_slash("path/")]
    #[case::sandwich_slash("/path/")]
    #[case::no_slash("path")]
    #[case::query_params("?something&something")]
    #[case::fragment("#blah")]
    fn relative_uris(#[case] val: &str) {
        let template = StringTemplate::from_str(val).unwrap();
        let uri = template
            .interpolate_uri(&Default::default())
            .expect("case was valid URI");
        assert!(uri.path_and_query().is_some());
        assert!(uri.authority().is_none());
    }

    #[rstest]
    #[case::http("http://example.com/something")]
    #[case::https("https://example.com/something")]
    #[case::ipv4("http://127.0.0.1/something")]
    #[case::ipv6("http://[::1]/something")]
    #[case::with_port("http://localhost:8080/something")]
    fn absolute_uris(#[case] val: &str) {
        let template = StringTemplate::from_str(val).unwrap();
        let uri = template
            .interpolate_uri(&Default::default())
            .expect("case was valid URI");
        assert!(uri.path_and_query().is_some());
        assert!(uri.authority().is_some());
        assert!(uri.scheme().is_some());
        assert_eq!(uri.to_string(), val);
    }

    /// Values are all strings, they can't have semantic value for HTTP. That means no dynamic paths,
    /// no nested query params, etc. When we expand values, we have to make sure they're safe.
    #[test]
    fn expression_encoding() {
        let vars = &this! {
            "path": "/some/path",
            "question_mark": "a?b",
            "ampersand": "a&b=b",
            "hash": "a#b",
        };

        let template = StringTemplate::from_str("http://localhost/{$this.path}/{$this.question_mark}?a={$this.ampersand}&c={$this.hash}")
            .expect("Failed to parse URL template");
        let url = template
            .interpolate_uri(vars)
            .expect("Failed to generate URL");

        assert_eq!(
            url.to_string(),
            "http://localhost/%2Fsome%2Fpath/a%3Fb?a=a%26b%3Db&c=a%23b"
        );
    }

    /// The resulting values of each expression are always [`Value`]s, for which we have a
    /// set way of encoding each as a string.
    #[test]
    fn json_value_serialization() {
        // `extra` would be illegal (we don't serialize arrays), but any unused values should be ignored
        let vars = &this! {
            "int": 1,
            "float": 1.2,
            "bool": true,
            "null": null,
            "string": "string",
            "extra": []
        };

        let template = StringTemplate::from_str(
            "/{$this.int}/{$this.float}/{$this.bool}/{$this.null}/{$this.string}",
        )
        .unwrap();

        let uri = template.interpolate(vars).expect("Failed to interpolate");

        assert_eq!(uri, "/1/1.2/true//string")
    }

    #[test]
    fn special_symbols_in_literal() {
        let literal = "/?brackets=[]&comma=,&parens=()&semi=;&colon=:&at=@&dollar=$&excl=!&plus=+&astr=*&quot='";
        let template = StringTemplate::from_str(literal).expect("Failed to parse URL template");
        let url = template
            .interpolate_uri(&Default::default())
            .expect("Failed to generate URL");

        assert_eq!(url.to_string(), literal);
    }

    /// If a user writes a string template that includes _illegal_ characters which must be encoded,
    /// we still encode them to avoid runtime errors.
    #[test]
    fn auto_encode_illegal_literal_characters() {
        let template = StringTemplate::from_str("https://example.com/😈 \\")
            .expect("Failed to parse URL template");

        let url = template
            .interpolate_uri(&Default::default())
            .expect("Failed to generate URL");
        assert_eq!(url.to_string(), "https://example.com/%F0%9F%98%88%20%5C")
    }

    /// Because we don't encode a bunch of characters that are situationally disallowed
    /// (for flexibility of the connector author), we also need to allow that they can manually
    /// percent encode characters themselves as-needed.
    #[test]
    fn allow_manual_percent_encoding() {
        let template = StringTemplate::from_str("https://example.com/%20")
            .expect("Failed to parse URL template");

        let url = template
            .interpolate_uri(&Default::default())
            .expect("Failed to generate URL");
        assert_eq!(url.to_string(), "https://example.com/%20")
    }

    /// Multi-line GraphQL strings are super useful for long templates. We need to make sure they're
    /// properly handled when generating URIs, though. New lines should be ignored.
    #[test]
    fn multi_line_templates() {
        let template = StringTemplate::from_str(
            "https://example.com\n/broken\npath\n/path\n?param=value\n&param=\r\nvalue&\nparam\n=\nvalue",
        )
        .expect("Failed to parse URL template");
        let url = template
            .interpolate_uri(&Default::default())
            .expect("Failed to generate URL");

        assert_eq!(
            url.to_string(),
            "https://example.com/brokenpath/path?param=value&param=value&param=value"
        )
    }
}

#[cfg(test)]
mod test_get_expressions {
    use super::*;

    #[test]
    fn test_variable_references() {
        let value =
            StringTemplate::from_str("a {$this.a.b.c} b {$args.a.b.c} c {$config.a.b.c}").unwrap();
        let references: Vec<_> = value
            .expressions()
            .map(|e| e.expression.to_string())
            .collect();
        assert_eq!(
            references,
            vec!["$this.a.b.c", "$args.a.b.c", "$config.a.b.c"]
        );
    }
}
