use std::fmt::Display;
use std::ops::Range;
use std::str::FromStr;

use apollo_compiler::collections::IndexMap;
use itertools::Itertools;
use serde::Serialize;
use serde_json_bytes::ByteString;
use serde_json_bytes::Map;
use serde_json_bytes::Value as JSON;
use strum::IntoEnumIterator;
use strum_macros::EnumIter;
use url::Url;

/// A parser accepting URLTemplate syntax, which is useful both for
/// generating new URL paths from provided variables and for extracting variable
/// values from concrete URL paths.
#[derive(Debug, PartialEq, Clone, Default)]
pub struct URLTemplate {
    /// Scheme + host if this is an absolute URL
    pub base: Option<Url>,
    path: Vec<Component>,
    query: IndexMap<String, Component>,
}

/// A single component of a path, like `/<component>` or a single query parameter, like `?<something>`.
/// Each component can consist of multiple parts, which are either text or variables.
#[derive(Debug, PartialEq, Clone)]
pub struct Component {
    /// The parts, which together, make up the single path component or query parameter.
    parts: Vec<ValuePart>,
}

/// A piece of a path or query parameter, which is either static text or a variable.
#[derive(Clone, Debug, PartialEq)]
pub enum ValuePart {
    Text(String),
    Var(Variable),
}

impl URLTemplate {
    // TODO: enforce that path params come from required schema elements
    pub fn path_variables(&self) -> impl Iterator<Item = &Variable> {
        self.path.iter().flat_map(Component::variables)
    }

    pub fn query_variables(&self) -> impl Iterator<Item = &Variable> {
        self.query.values().flat_map(Component::variables)
    }
    /// Return all variables in the template in the order they appeared
    pub fn variables(&self) -> impl Iterator<Item = &Variable> {
        self.path_variables().chain(self.query_variables())
    }

    pub fn interpolate_path(&self, vars: &Map<ByteString, JSON>) -> Result<Vec<String>, String> {
        self.path.iter().enumerate().map(|(path_position, param_value)| {
            param_value.interpolate(vars).ok_or_else(|| format!(
                "Incomplete path parameter {param_value} at position {path_position} with variables {vars:?}",
            ))
        }).collect()
    }

    pub fn interpolate_query(&self, vars: &Map<ByteString, JSON>) -> Vec<(String, String)> {
        self.query
            .iter()
            .filter_map(|(key, param_value)| {
                param_value
                    .interpolate(vars)
                    .map(|value| (key.to_string(), value))
            })
            .collect()
    }
}

#[derive(Debug)]
pub struct Error {
    pub(crate) message: String,
    pub(crate) location: Option<Range<usize>>,
}

impl FromStr for URLTemplate {
    type Err = Error;

    /// Top-level parsing entry point for URLTemplate syntax.
    fn from_str(input: &str) -> Result<Self, Self::Err> {
        let (base, path) = if let Some((scheme, rest)) = input.split_once("://") {
            if let Some((host, path)) = rest.split_once('/') {
                (
                    Some(
                        Url::parse(&format!("{}://{}", scheme, host)).map_err(|err| Error {
                            message: err.to_string(),
                            location: Some(0..(input.len() - path.len())),
                        })?,
                    ),
                    path,
                )
            } else {
                (
                    Some(Url::parse(input).map_err(|err| Error {
                        message: err.to_string(),
                        location: None,
                    })?),
                    "",
                )
            }
        } else {
            (None, input)
        };
        let mut prefix_suffix = path.splitn(2, '?');
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
            .filter_map(|query_part| {
                let (key, value) = query_part.split_once('=')?;
                Some(Component::parse(value, input).map(|value| (key.to_string(), value)))
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
                f.write_str(key)?;
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
                parts.push(ValuePart::Var(Variable::parse(var, url_template)?));
                remaining = suffix;
            } else {
                return Err(Error {
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
                    if let Some(var_value) = var.interpolate(vars) {
                        value.push_str(&var_value);
                    } else {
                        return None;
                    }
                }
            }
        }

        Some(value)
    }

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
        let mut pending_var: Option<&Variable> = None;
        let mut output = Map::new();

        fn add_var_value(var: &Variable, value: &str, output: &mut Map<ByteString, JSON>) {
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

    fn variables(&self) -> impl Iterator<Item = &Variable> {
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

/// A variable expression, starting with `$`, that can be used in JSONSelection or URLTemplate.
///
/// This is basically a subset of JSONSelection's `PathSelection`, but with fewer features.
#[derive(Debug, Clone, Hash, PartialEq, Eq)]
pub struct Variable {
    pub var_type: VariableType,
    pub path: String,
    /// Where this variable is in the string template it came from—0-indexed offset.
    pub location: Range<usize>,
}

/// The supported types of variables for URLs, a subset of `KnownVariable` in JSONSelection
#[derive(Clone, Copy, Debug, EnumIter, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum VariableType {
    Args,
    This,
    Config,
}

impl VariableType {
    const fn as_str(&self) -> &'static str {
        match self {
            VariableType::Args => "$args",
            VariableType::This => "$this",
            VariableType::Config => "$config",
        }
    }
}

impl Display for VariableType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for VariableType {
    type Err = String;

    fn from_str(input: &str) -> Result<Self, Self::Err> {
        Self::iter()
            .find(|var_type| var_type.as_str() == input)
            .ok_or_else(|| {
                format!(
                    "Variable type must be one of {}, got {input}",
                    Self::iter().map(|var_type| var_type.as_str()).join(", ")
                )
            })
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

impl Variable {
    /// Parse `input` as a variable. `url_template` should be the _entire_ URL template, for
    /// example `http.GET`, for calculating the position of the variable in the template.
    fn parse(input: &str, url_template: &str) -> Result<Self, Error> {
        let start = input.as_ptr() as usize - url_template.as_ptr() as usize;
        let end = start + input.len();
        let location = start..end;

        let mut parts = input.split('.');
        let Some(var_type_str) = parts.next() else {
            return Err(Error {
                message: format!("Variable expression {input} can't be empty"),
                location: Some(location),
            });
        };

        let var_type = VariableType::from_str(var_type_str).map_err(|message| Error {
            message,
            location: Some(location.clone()),
        })?;
        let path = parts.join(".");
        if path.is_empty() {
            return Err(Error {
                message: format!(
                    "Variable expression {input} must have a path after the variable type",
                ),
                location: Some(location),
            });
        }

        Ok(Self {
            var_type,
            path,
            location,
        })
    }

    fn interpolate(&self, vars: &Map<ByteString, JSON>) -> Option<String> {
        let full_path = format!(
            "{var_type}.{path}",
            var_type = self.var_type,
            path = self.path
        );
        vars.get(full_path.as_str()).map(|child_value| {
            // Need to remove quotes from string values, since the quotes don't
            // belong in the URL.
            if let JSON::String(string) = child_value {
                string.as_str().to_string()
            } else {
                child_value.to_string()
            }
        })
    }
}

impl Display for Variable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.var_type.as_str())?;
        f.write_str(".")?;
        f.write_str(&self.path)
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
            err.message,
            "Variable type must be one of $args, $this, $config, got $blah"
        );
        assert_eq!(err.location, Some(12..23));
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
