use std::fmt::Display;
use std::ops::Range;
use std::str::FromStr;

use apollo_compiler::collections::IndexMap;
use itertools::Itertools;
use serde::Serialize;
use serde_json_bytes::ByteString;
use serde_json_bytes::Map;
use serde_json_bytes::Value as JSON;
use url::Url;

use crate::sources::connect::variable::Namespace;
use crate::sources::connect::variable::VariableError;
use crate::sources::connect::variable::VariableReference;

/// A parser accepting URLTemplate syntax, which is useful both for
/// generating new URL paths from provided variables and for extracting variable
/// values from concrete URL paths.
#[derive(Debug, Clone, Default)]
pub struct URLTemplate {
    /// Scheme + host if this is an absolute URL
    pub base: Option<Url>,
    path: Vec<Component>,
    query: IndexMap<Component, Component>,
}

/// A single component of a path, like `/<component>` or a single query parameter, like `?<something>`.
/// Each component can consist of multiple parts, which are either text or variables.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub(crate) struct Component {
    /// The parts, which together, make up the single path component or query parameter.
    parts: Vec<ValuePart>,
}

/// A piece of a path or query parameter, which is either static text or a variable.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub(crate) enum ValuePart {
    Text(String),
    Var(VariableReference<Namespace>),
}

impl URLTemplate {
    pub(crate) fn path_variables(&self) -> impl Iterator<Item = &VariableReference<Namespace>> {
        self.path.iter().flat_map(Component::variables)
    }

    pub(crate) fn query_variables(&self) -> impl Iterator<Item = &VariableReference<Namespace>> {
        self.query
            .keys()
            .chain(self.query.values())
            .flat_map(Component::variables)
    }
    /// Return all variables in the template in the order they appeared
    pub(crate) fn variables(&self) -> impl Iterator<Item = &VariableReference<Namespace>> {
        self.path_variables().chain(self.query_variables())
    }

    pub fn interpolate_path(&self, vars: &Map<ByteString, JSON>) -> Result<Vec<String>, String> {
        self.path
            .iter()
            .map(|param_value| {
                param_value.interpolate(vars).ok_or_else(|| {
                    format!(
                        "Path parameter {param_value} was missing one or more values in {vars:?}",
                    )
                })
            })
            .collect()
    }

    pub fn interpolate_query(&self, vars: &Map<ByteString, JSON>) -> Vec<(String, String)> {
        self.query
            .iter()
            .filter_map(|(key, param_value)| {
                key.interpolate(vars).zip(param_value.interpolate(vars))
            })
            .collect()
    }
}

#[derive(Debug, PartialEq)]
pub enum Error {
    InvalidVariableNamespace {
        namespace: String,
        location: Range<usize>,
    },
    ParseError {
        message: String,
        location: Option<Range<usize>>,
    },
}

impl Error {
    pub(crate) fn message(&self) -> String {
        match self {
            Error::InvalidVariableNamespace { namespace, .. } => {
                format!("Invalid variable namespace: {namespace}")
            }
            Error::ParseError { message, .. } => message.clone(),
        }
    }
}

impl From<VariableError> for Error {
    fn from(value: VariableError) -> Self {
        match value {
            VariableError::InvalidNamespace {
                namespace,
                location,
            } => Self::InvalidVariableNamespace {
                namespace,
                location,
            },
            VariableError::ParseError { message, location } => Self::ParseError {
                message,
                location: Some(location),
            },
        }
    }
}

impl FromStr for URLTemplate {
    type Err = Error;

    /// Top-level parsing entry point for URLTemplate syntax.
    fn from_str(input: &str) -> Result<Self, Self::Err> {
        let (raw_base, rest) = if let Some(end_of_scheme) = input.find("://") {
            let start_of_authority = end_of_scheme + 3;
            let rest_of_uri = &input[start_of_authority..];
            let end_of_authority = rest_of_uri
                .find('/')
                .or_else(|| rest_of_uri.find('?'))
                .or_else(|| rest_of_uri.find('#'))
                .unwrap_or(rest_of_uri.len())
                + start_of_authority;
            let authority = Some(&input[..end_of_authority]);
            if end_of_authority < input.len() {
                (authority, Some(&input[end_of_authority..]))
            } else {
                (authority, None)
            }
        } else {
            (None, Some(input))
        };
        let base = raw_base
            .map(|raw_base| {
                Url::parse(raw_base).map_err(|err| Error::ParseError {
                    message: err.to_string(),
                    location: Some(0..raw_base.len()),
                })
            })
            .transpose()?;

        let mut prefix_suffix = rest.into_iter().flat_map(|rest| rest.splitn(2, '?'));
        let path_prefix = prefix_suffix.next();
        let query_suffix = prefix_suffix.next();

        let path = path_prefix
            .into_iter()
            .flat_map(|path_prefix| path_prefix.split('/'))
            .filter(|path_part| !path_part.is_empty())
            .map(|path_part| Component::parse(path_part, input))
            .try_collect()?;

        let query = query_suffix
            .into_iter()
            .flat_map(|query_suffix| query_suffix.split('&'))
            .map(|query_part| {
                let (key, value) = query_part.split_once('=').ok_or_else(|| {
                    let start = query_part.as_ptr() as usize - input.as_ptr() as usize;
                    let end = start + query_part.len();
                    Error::ParseError {
                        message: format!("Query parameter {query_part} must have a value"),
                        location: Some(start..end),
                    }
                })?;
                let key = Component::parse(key, input)?;
                let value = Component::parse(value, input)?;
                Ok::<(Component, Component), Self::Err>((key, value))
            })
            .try_collect()?;

        Ok(URLTemplate { base, path, query })
    }
}

impl Display for URLTemplate {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if let Some(base) = &self.base {
            f.write_str(base.to_string().trim_end_matches('/'))?;
        }

        for param_value in &self.path {
            f.write_str("/")?;
            param_value.fmt(f)?;
        }

        if !self.query.is_empty() {
            f.write_str("?")?;
            let mut first = true;
            for (key, param_value) in &self.query {
                if first {
                    first = false;
                } else {
                    f.write_str("&")?;
                }
                key.fmt(f)?;
                f.write_str("=")?;
                param_value.fmt(f)?;
            }
        }

        Ok(())
    }
}

impl Serialize for URLTemplate {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl Component {
    /// Parse `input` as a single path component or query parameter. `url_template` is the entire
    /// string of the URL Template for calculating the position of the variable in the template.
    fn parse(input: &str, url_template: &str) -> Result<Self, Error> {
        // Split the text around any {...} variable expressions, which must be
        // separated by nonempty text.
        let mut parts = vec![];
        let mut remaining = input;

        while let Some((prefix, suffix)) = remaining.split_once('{') {
            if !prefix.is_empty() {
                parts.push(ValuePart::Text(prefix.to_string()));
            }
            remaining = suffix;

            if let Some((var, suffix)) = remaining.split_once('}') {
                let start_offset = var.as_ptr() as usize - url_template.as_ptr() as usize;
                parts.push(ValuePart::Var(VariableReference::parse(var, start_offset)?));
                remaining = suffix;
            } else {
                return Err(Error::ParseError {
                    message: format!(
                        "Missing closing brace in URL suffix {} of path {}",
                        remaining, input
                    ),
                    location: None,
                });
            }
        }

        if !remaining.is_empty() {
            parts.push(ValuePart::Text(remaining.to_string()));
        }

        Ok(Component { parts })
    }

    fn interpolate(&self, vars: &Map<ByteString, JSON>) -> Option<String> {
        let mut value = String::new();

        for part in &self.parts {
            match part {
                ValuePart::Text(text) => {
                    value.push_str(text);
                }
                ValuePart::Var(var) => {
                    if let Some(var_value) = vars.get(var.to_string().as_str()).map(|child_value| {
                        // Need to remove quotes from string values, since the quotes don't
                        // belong in the URL.
                        if let JSON::String(string) = child_value {
                            string.as_str().to_string()
                        } else {
                            child_value.to_string()
                        }
                    }) {
                        value.push_str(&var_value);
                    } else {
                        return None;
                    }
                }
            }
        }

        Some(value)
    }

    #[allow(unused)]
    fn extract_vars(&self, concrete_value: &Component) -> Result<Map<ByteString, JSON>, String> {
        let mut concrete_text = String::new();
        for part in &concrete_value.parts {
            concrete_text.push_str(match part {
                ValuePart::Text(text) => text,
                ValuePart::Var(var) => {
                    return Err(format!("Unexpected variable expression {{{}}}", var));
                }
            });
        }

        let mut concrete_suffix = concrete_text.as_str();
        let mut pending_var: Option<&VariableReference<Namespace>> = None;
        let mut output = Map::new();

        fn add_var_value(
            var: &VariableReference<Namespace>,
            value: &str,
            output: &mut Map<ByteString, JSON>,
        ) {
            let key = ByteString::from(var.to_string());
            if !value.is_empty() {
                output.insert(key, JSON::String(ByteString::from(value)));
            }
        }

        for part in &self.parts {
            match part {
                ValuePart::Text(text) => {
                    if let Some(var) = pending_var {
                        if let Some(start) = concrete_suffix.find(text) {
                            add_var_value(var, &concrete_suffix[..start], &mut output);
                            concrete_suffix = &concrete_suffix[start..];
                        } else {
                            add_var_value(var, concrete_suffix, &mut output);
                            concrete_suffix = "";
                        }
                        pending_var = None;
                    }

                    if concrete_suffix.starts_with(text) {
                        concrete_suffix = &concrete_suffix[text.len()..];
                    } else {
                        return Err(format!(
                            "Constant text {} not found in {}",
                            text, concrete_text
                        ));
                    }
                }
                ValuePart::Var(var) => {
                    if let Some(pending) = pending_var {
                        return Err(format!(
                            "Ambiguous adjacent variable expressions {} and {} in parameter value {}",
                            pending, var, concrete_text
                        ));
                    } else {
                        // This variable's value will be extracted from the
                        // concrete URL by the ValuePart::Text branch above, on
                        // the next iteration of the for loop.
                        pending_var = Some(var);
                    }
                }
            }
        }

        if let Some(var) = pending_var {
            add_var_value(var, concrete_suffix, &mut output);
        }

        Ok(output)
    }

    fn variables(&self) -> impl Iterator<Item = &VariableReference<Namespace>> {
        self.parts.iter().filter_map(|part| match part {
            ValuePart::Text(_) => None,
            ValuePart::Var(var) => Some(var),
        })
    }
}

impl Display for Component {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for part in &self.parts {
            part.fmt(f)?;
        }
        Ok(())
    }
}

impl Serialize for Component {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl Serialize for ValuePart {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl Display for ValuePart {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ValuePart::Text(text) => {
                f.write_str(text)?;
            }
            ValuePart::Var(var) => {
                f.write_str("{")?;
                var.fmt(f)?;
                f.write_str("}")?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod test_parse {
    use insta::assert_debug_snapshot;
    use pretty_assertions::assert_eq;

    use super::*;

    // TODO: test invalid variable names / expressions

    #[test]
    fn test_path_list() {
        assert_debug_snapshot!(URLTemplate::from_str("/abc"));

        assert_debug_snapshot!(URLTemplate::from_str("/abc/def"));

        assert_debug_snapshot!(URLTemplate::from_str("/abc/{$args.def}"));

        assert_debug_snapshot!(URLTemplate::from_str("/abc/{$this.def.thing}/ghi"));
    }

    #[test]
    fn test_url_path_template_parse() {
        assert_debug_snapshot!(URLTemplate::from_str("/users/{$config.user_id}?a=b"));

        assert_debug_snapshot!(URLTemplate::from_str(
            "/users/{$this.user_id}?a={$args.b}&c={$args.d}&e={$args.f.g}"
        ));

        assert_debug_snapshot!(URLTemplate::from_str(
            "/users/{$this.id}?a={$config.b}#junk"
        ));

        assert_debug_snapshot!(URLTemplate::from_str("/location/{$this.lat},{$this.lon}"));
    }

    #[test]
    fn test_invalid_variable_name() {
        let err = URLTemplate::from_str("/something/{$blah.stuff}/more").unwrap_err();
        assert_eq!(
            err,
            Error::InvalidVariableNamespace {
                namespace: "$blah".into(),
                location: 12..17
            }
        );
    }

    #[test]
    fn basic_absolute_url() {
        assert_debug_snapshot!(URLTemplate::from_str("http://example.com"));
    }

    #[test]
    fn absolute_url_with_path() {
        assert_debug_snapshot!(URLTemplate::from_str("http://example.com/abc/def"));
    }

    #[test]
    fn absolute_url_with_path_variable() {
        assert_debug_snapshot!(URLTemplate::from_str("http://example.com/{$args.abc}/def"));
    }

    #[test]
    fn absolute_url_with_query() {
        assert_debug_snapshot!(URLTemplate::from_str("http://example.com?abc=def"));
    }

    #[test]
    fn absolute_url_with_query_variable() {
        assert_debug_snapshot!(URLTemplate::from_str("http://example.com?abc={$args.abc}"));
    }

    #[test]
    fn variable_param_key() {
        assert_debug_snapshot!(URLTemplate::from_str(
            "?{$args.filter.field}={$args.filter.value}"
        ));
    }
}

#[cfg(test)]
#[rstest::rstest]
#[case("/users/{$this.user_id}?a={$this.b}&c={$this.d}&e={$this.f.g}")]
#[case("/position/{$this.x},{$this.y}")]
#[case("/position/xyz({$this.x},{$this.y},{$this.z})")]
#[case("/position?xyz=({$this.x},{$this.y},{$this.z})")]
fn test_display_trait(#[case] template: &str) {
    assert_eq!(
        URLTemplate::from_str(template).unwrap().to_string(),
        template.to_string()
    );
}

#[cfg(test)]
mod test_interpolate {
    use pretty_assertions::assert_eq;

    use super::*;

    #[test]
    fn query_key_variable() {
        let template = URLTemplate::from_str("?{$args.filter.field}={$args.filter.value}").unwrap();
        let mut vars = Map::new();
        assert_eq!(
            template.interpolate_query(&vars),
            Vec::new(),
            "When there are no variables, there should be no query parameters"
        );

        vars.insert(
            ByteString::from("$args.filter.field"),
            JSON::String(ByteString::from("name")),
        );
        assert_eq!(
            template.interpolate_query(&vars),
            Vec::new(),
            "When a query param value is missing, the query parameter should be skipped"
        );

        vars.insert(
            ByteString::from("$args.filter.value"),
            JSON::String(ByteString::from("value")),
        );
        assert_eq!(
            template.interpolate_query(&vars),
            vec![("name".to_string(), "value".to_string())],
            "When both variables present, query parameter interpolated"
        );

        vars.remove("$args.filter.field");
        assert_eq!(
            template.interpolate_query(&vars),
            Vec::new(),
            "Missing key, query parameter skipped"
        );
    }
}
