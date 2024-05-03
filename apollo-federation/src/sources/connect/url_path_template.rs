use std::collections::HashSet;
use std::fmt::Display;

use indexmap::IndexMap;
use itertools::Itertools;
use nom::branch::alt;
use nom::bytes::complete::tag;
use nom::character::complete::char;
use nom::character::complete::one_of;
use nom::combinator::opt;
use nom::combinator::recognize;
use nom::multi::many0;
use nom::sequence::pair;
use nom::sequence::preceded;
use nom::sequence::tuple;
use nom::IResult;
use serde::Serialize;
use serde_json_bytes::ByteString;
use serde_json_bytes::Map;
use serde_json_bytes::Value as JSON;

/// A parser accepting URLPathTemplate syntax, which is useful both for
/// generating new URL paths from provided variables and for extracting variable
/// values from concrete URL paths.

#[derive(Debug, PartialEq, Clone, Default)]
pub struct URLPathTemplate {
    path: Vec<ParameterValue>,
    query: IndexMap<String, ParameterValue>,
}

#[derive(Debug, PartialEq, Clone)]
pub struct ParameterValue {
    // The ParameterValue struct represents both path parameter values and query
    // parameter values, allowing zero or more variable expressions separated by
    // nonempty constant text.
    parts: Vec<ValuePart>,
}

#[derive(Debug, PartialEq, Clone)]
pub enum ValuePart {
    Text(String),
    Var(VariableExpression),
}

#[derive(Debug, PartialEq, Clone, Default)]
pub struct VariableExpression {
    // Variable paths are often a single identifier, but may also consist of a
    // sequence of identifiers joined with the . character. We represent dotted
    // paths as a single string, rather than a Vec<String>, and these dotted
    // path strings are expected for the input keys of generate_path and the
    // output keys of extract_vars, rather than a nested JSON object.
    var_path: String,

    // When Some, the batch_separator option indicates the variable is a batch
    // variable, so the value of the variable is expected to be a JSON array,
    // and the separator string separates the batched variable values in the
    // parsed/generated URL path.
    batch_separator: Option<String>,

    // Variables in the URL path are required by default, whereas variables in
    // the query parameter list are optional by default, but can be made
    // mandatory by adding a trailing ! to the variable path.
    required: bool,
}

impl URLPathTemplate {
    // Top-level parsing entry point for URLPathTemplate syntax.
    pub fn parse(input: &str) -> Result<URLPathTemplate, String> {
        let mut prefix_suffix = input.splitn(2, '?');
        let path_prefix = prefix_suffix.next();
        let query_suffix = prefix_suffix.next();
        let mut path = vec![];

        if let Some(path_prefix) = path_prefix {
            for path_part in path_prefix.split('/') {
                if !path_part.is_empty() {
                    path.push(ParameterValue::parse(path_part, true)?);
                }
            }
        }

        let mut query = IndexMap::new();

        if let Some(query_suffix) = query_suffix {
            for query_part in query_suffix.split('&') {
                if let Some((key, value)) = query_part.split_once('=') {
                    query.insert(key.to_string(), ParameterValue::parse(value, false)?);
                }
            }
        }

        Ok(URLPathTemplate { path, query })
    }

    // Given a URLPathTemplate and an IndexMap of variables to be interpolated
    // into its {...} expressions, generate a new URL path String.
    // Guaranteed to return a "/"-prefixed string to make appending to the
    // base url easier.
    pub fn generate_path(&self, vars: &JSON) -> Result<String, String> {
        let mut path = String::new();
        if let Some(var_map) = vars.as_object() {
            for (path_position, param_value) in self.path.iter().enumerate() {
                path.push('/');

                if let Some(value) = param_value.interpolate(var_map)? {
                    path.push_str(value.as_str());
                } else {
                    return Err(format!(
                        "Incomplete path parameter {} at position {} with variables {}",
                        param_value,
                        path_position,
                        JSON::Object(var_map.clone()),
                    ));
                }
            }

            let mut params = vec![];
            for (key, param_value) in &self.query {
                if let Some(value) = param_value.interpolate(var_map)? {
                    params.push(format!("{}={}", key, value));
                }
            }
            if !params.is_empty() {
                path.push('?');
                path.push_str(&params.join("&"));
            }
        } else {
            return Err(format!("Expected object, got {}", vars));
        }

        if path.is_empty() {
            Ok("/".to_string())
        } else if path.starts_with('/') {
            Ok(path)
        } else {
            Ok(format!("/{}", path))
        }
    }

    // Given a URLPathTemplate and a concrete URL path, extract any named/nested
    // variables from the path and return them as a JSON object.
    #[allow(dead_code)]
    fn extract_vars(&self, path: &str) -> Result<JSON, String> {
        let concrete_template = URLPathTemplate::parse(path)?;

        if concrete_template.path.len() != self.path.len() {
            return Err(format!(
                "Path length {} does not match concrete path length {}",
                self.path.len(),
                concrete_template.path.len()
            ));
        }

        let mut var_map = Map::new();

        for (i, path_value) in self.path.iter().enumerate() {
            for (var_path, value) in path_value.extract_vars(&concrete_template.path[i])? {
                var_map.insert(var_path, value);
            }
        }

        // For each query parameter, extract the corresponding variable(s) from
        // the concrete template text.
        for (key, query_value) in self.query.iter() {
            if let Some(concrete_value) = concrete_template.query.get(key) {
                for (var_path, value) in query_value.extract_vars(concrete_value)? {
                    var_map.insert(var_path, value);
                }
            } else {
                // If there is no corresponding query parameter in the concrete
                // URL path, we can't extract variables, which is only a problem
                // if any of the expected variables are required.
                for part in &query_value.parts {
                    if let ValuePart::Var(var) = part {
                        if var.required {
                            return Err(format!(
                                "Missing required query parameter {}={}",
                                key, query_value
                            ));
                        }
                    }
                }
            }
        }

        Ok(JSON::Object(var_map))
    }

    pub fn required_parameters(&self) -> Vec<String> {
        let mut parameters = HashSet::new();
        for param_value in &self.path {
            parameters.extend(param_value.required_parameters());
        }
        for param_value in self.query.values() {
            parameters.extend(param_value.required_parameters());
        }
        // sorted for a stable SDL
        parameters.into_iter().sorted().collect()
    }
}

impl Display for URLPathTemplate {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
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

impl Serialize for URLPathTemplate {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl ParameterValue {
    fn parse(input: &str, required_by_default: bool) -> Result<Self, String> {
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
                parts.push(ValuePart::Var(VariableExpression::parse(
                    var,
                    required_by_default,
                )?));
                remaining = suffix;
            } else {
                return Err(format!(
                    "Missing closing brace in URL suffix {} of path {}",
                    remaining, input
                ));
            }
        }

        if !remaining.is_empty() {
            parts.push(ValuePart::Text(remaining.to_string()));
        }

        // Enforce that variable expressions must be separated by nonempty text
        // delimiters, though the parameter value may start or end with variable
        // expressions without preceding/following text.
        let mut prev_part_was_var = false;
        for part in &parts {
            if let ValuePart::Var(_) = part {
                if prev_part_was_var {
                    return Err(format!(
                        "Ambiguous adjacent variable expressions in {}",
                        input,
                    ));
                }
                prev_part_was_var = true;
            } else {
                prev_part_was_var = false;
            }
        }

        Ok(ParameterValue { parts })
    }

    fn interpolate(&self, vars: &Map<ByteString, JSON>) -> Result<Option<String>, String> {
        let mut value = String::new();
        let mut missing_vars = vec![];
        let mut some_vars_required = false;

        for part in &self.parts {
            match part {
                ValuePart::Text(text) => {
                    value.push_str(text);
                }
                ValuePart::Var(var) => {
                    if let Some(var_value) = var.interpolate(vars)? {
                        value.push_str(&var_value);
                    } else {
                        missing_vars.push(var);
                    }
                    if var.required {
                        some_vars_required = true;
                    }
                }
            }
        }

        // If any variable fails to interpolate, the whole ParameterValue fails
        // to interpolate. This can be harmless if none of the variables are
        // required, but if any of the variables are required (not just the
        // variables that failed to interpolate), then the whole ParameterValue
        // is required, so any missing variable becomes an error.
        if let Some(missing) = missing_vars.into_iter().next() {
            if some_vars_required {
                return Err(format!(
                    "Missing variable {} for required parameter {} given variables {}",
                    missing.var_path,
                    self,
                    JSON::Object(vars.clone()),
                ));
            } else {
                return Ok(None);
            }
        }

        Ok(Some(value))
    }

    fn extract_vars(
        &self,
        concrete_value: &ParameterValue,
    ) -> Result<Map<ByteString, JSON>, String> {
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
        let mut pending_var: Option<&VariableExpression> = None;
        let mut output = Map::new();

        fn add_var_value(
            var: &VariableExpression,
            value: &str,
            output: &mut Map<ByteString, JSON>,
        ) {
            let key = ByteString::from(var.var_path.as_str());
            if let Some(separator) = &var.batch_separator {
                let mut values = vec![];
                for value in value.split(separator) {
                    if !value.is_empty() {
                        values.push(JSON::String(ByteString::from(value)));
                    }
                }
                output.insert(key, JSON::Array(values));
            } else if !value.is_empty() {
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

    fn required_parameters(&self) -> Vec<String> {
        let mut parameters = vec![];
        for part in &self.parts {
            match part {
                ValuePart::Text(_) => {}
                ValuePart::Var(var) => {
                    if var.required {
                        parameters.push(var.var_path.clone());
                    }
                }
            }
        }
        parameters
    }
}

impl Display for ParameterValue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for part in &self.parts {
            part.fmt(f)?;
        }
        Ok(())
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

impl Serialize for ParameterValue {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl VariableExpression {
    // TODO Figure out if this required parameter is really the best way to
    // handle ! variables.
    fn parse(input: &str, required: bool) -> Result<Self, String> {
        tuple((
            nom_parse_identifier_path,
            opt(char('!')),
            opt(pair(one_of(",;|+ "), tag("..."))),
        ))(input)
        .map_err(|err| format!("Error parsing variable expression {}: {}", input, err))
        .and_then(
            |(remaining, (var_path, exclamation_point, batch_separator))| {
                if remaining.is_empty() {
                    Ok(VariableExpression {
                        var_path,
                        required: exclamation_point.is_some() || required,
                        batch_separator: batch_separator
                            .map(|(separator, _)| separator.to_string()),
                    })
                } else {
                    Err(format!(
                        "Unexpected trailing characters {} in variable expression {}",
                        remaining, input
                    ))
                }
            },
        )
    }

    fn interpolate(&self, vars: &Map<ByteString, JSON>) -> Result<Option<String>, String> {
        let var_path_bytes = ByteString::from(self.var_path.as_str());
        if let Some(child_value) = vars.get(&var_path_bytes) {
            if let Some(separator) = &self.batch_separator {
                if let JSON::Array(array) = child_value {
                    let mut value_strings = vec![];
                    for value in array {
                        value_strings.push(self.value_as_string(value));
                    }
                    if value_strings.is_empty() {
                        return Ok(None);
                    } else {
                        return Ok(Some(value_strings.join(separator)));
                    }
                }
                // Fall through to handle non-array values as single batch inputs.
            }
            Ok(Some(self.value_as_string(child_value)))
        } else if self.required {
            return Err(format!(
                "Missing required variable {} in {}",
                self.var_path,
                JSON::Object(vars.clone()),
            ));
        } else {
            return Ok(None);
        }
    }

    fn value_as_string(&self, value: &JSON) -> String {
        // Need to remove quotes from string values, since the quotes don't
        // belong in the URL.
        if let JSON::String(string) = value {
            string.as_str().to_string()
        } else {
            value.to_string()
        }
    }
}

impl Display for VariableExpression {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.var_path)?;
        if self.required {
            f.write_str("!")?;
        }
        if let Some(separator) = &self.batch_separator {
            f.write_str(separator)?;
            f.write_str("...")?;
        }
        Ok(())
    }
}

fn nom_parse_identifier_possible_namespace(input: &str) -> IResult<&str, &str> {
    recognize(alt((tag("$this"), nom_parse_identifier)))(input)
}

fn nom_parse_identifier(input: &str) -> IResult<&str, &str> {
    recognize(pair(
        one_of("abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ_"),
        many0(one_of(
            "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ_0123456789",
        )),
    ))(input)
}

fn nom_parse_identifier_path(input: &str) -> IResult<&str, String> {
    let (input, first) = nom_parse_identifier_possible_namespace(input)?;
    let (input, mut rest) = many0(preceded(char('.'), nom_parse_identifier))(input)?;
    let mut identifier_path = vec![first];
    identifier_path.append(&mut rest);
    Ok((input, identifier_path.join(".")))
}

#[cfg(test)]
mod tests {
    use serde_json_bytes::json;

    use super::*;

    #[test]
    fn test_parse_identifier() {
        assert_eq!(nom_parse_identifier("abc"), Ok(("", "abc")));
        assert_eq!(nom_parse_identifier("abc123"), Ok(("", "abc123")));
        assert_eq!(nom_parse_identifier("abc_123"), Ok(("", "abc_123")));
        assert_eq!(nom_parse_identifier("abc-123"), Ok(("-123", "abc")));
    }

    #[test]
    fn test_parse_identifier_path() {
        assert_eq!(
            nom_parse_identifier_path("abc"),
            Ok(("", "abc".to_string())),
        );
        assert_eq!(
            nom_parse_identifier_path("abc.def"),
            Ok(("", "abc.def".to_string())),
        );
        assert_eq!(
            nom_parse_identifier_path("abc.def.ghi"),
            Ok(("", "abc.def.ghi".to_string())),
        );
        assert_eq!(
            nom_parse_identifier_path("$this.def.ghi"),
            Ok(("", "$this.def.ghi".to_string())),
        );

        assert!(nom_parse_identifier_path("$anything.def.ghi").is_err());
        assert_eq!(
            nom_parse_identifier_path("abc.$this.ghi"),
            Ok((".$this.ghi", "abc".to_string())),
        );
    }

    #[test]
    fn test_path_list() {
        assert_eq!(
            URLPathTemplate::parse("/abc"),
            Ok(URLPathTemplate {
                path: vec![ParameterValue {
                    parts: vec![ValuePart::Text("abc".to_string())],
                },],
                ..Default::default()
            }),
        );

        assert_eq!(
            URLPathTemplate::parse("/abc/def"),
            Ok(URLPathTemplate {
                path: vec![
                    ParameterValue {
                        parts: vec![ValuePart::Text("abc".to_string())],
                    },
                    ParameterValue {
                        parts: vec![ValuePart::Text("def".to_string())],
                    },
                ],
                ..Default::default()
            }),
        );

        assert_eq!(
            URLPathTemplate::parse("/abc/{def}"),
            Ok(URLPathTemplate {
                path: vec![
                    ParameterValue {
                        parts: vec![ValuePart::Text("abc".to_string())],
                    },
                    ParameterValue {
                        parts: vec![ValuePart::Var(VariableExpression {
                            var_path: "def".to_string(),
                            required: true,
                            ..Default::default()
                        })],
                    },
                ],
                ..Default::default()
            }),
        );

        assert_eq!(
            URLPathTemplate::parse("/abc/{def}/ghi"),
            Ok(URLPathTemplate {
                path: vec![
                    ParameterValue {
                        parts: vec![ValuePart::Text("abc".to_string())],
                    },
                    ParameterValue {
                        parts: vec![ValuePart::Var(VariableExpression {
                            var_path: "def".to_string(),
                            required: true,
                            ..Default::default()
                        })],
                    },
                    ParameterValue {
                        parts: vec![ValuePart::Text("ghi".to_string())],
                    },
                ],
                ..Default::default()
            }),
        );
    }

    #[test]
    fn test_url_path_template_parse() {
        assert_eq!(
            URLPathTemplate::parse("/users/{user_id}?a=b"),
            Ok(URLPathTemplate {
                path: vec![
                    ParameterValue {
                        parts: vec![ValuePart::Text("users".to_string())],
                    },
                    ParameterValue {
                        parts: vec![ValuePart::Var(VariableExpression {
                            var_path: "user_id".to_string(),
                            required: true,
                            ..Default::default()
                        })],
                    },
                ],
                query: IndexMap::from([(
                    "a".to_string(),
                    ParameterValue {
                        parts: vec![ValuePart::Text("b".to_string())],
                    }
                )]),
            }),
        );

        assert_eq!(
            URLPathTemplate::parse("/users/{user_id}?a={b}&c={d!}&e={f.g}"),
            Ok(URLPathTemplate {
                path: vec![
                    ParameterValue {
                        parts: vec![ValuePart::Text("users".to_string())],
                    },
                    ParameterValue {
                        parts: vec![ValuePart::Var(VariableExpression {
                            var_path: "user_id".to_string(),
                            required: true,
                            ..Default::default()
                        })],
                    },
                ],
                query: IndexMap::from([
                    (
                        "e".to_string(),
                        ParameterValue {
                            parts: vec![ValuePart::Var(VariableExpression {
                                var_path: "f.g".to_string(),
                                ..Default::default()
                            })],
                        },
                    ),
                    (
                        "a".to_string(),
                        ParameterValue {
                            parts: vec![ValuePart::Var(VariableExpression {
                                var_path: "b".to_string(),
                                ..Default::default()
                            })],
                        },
                    ),
                    (
                        "c".to_string(),
                        ParameterValue {
                            parts: vec![ValuePart::Var(VariableExpression {
                                var_path: "d".to_string(),
                                required: true,
                                ..Default::default()
                            })],
                        },
                    ),
                ]),
            }),
        );

        assert_eq!(
            URLPathTemplate::parse("/users/{id}?a={b}#junk"),
            Ok(URLPathTemplate {
                path: vec![
                    ParameterValue {
                        parts: vec![ValuePart::Text("users".to_string())],
                    },
                    ParameterValue {
                        parts: vec![ValuePart::Var(VariableExpression {
                            var_path: "id".to_string(),
                            required: true,
                            ..Default::default()
                        })],
                    },
                ],
                query: IndexMap::from([(
                    "a".to_string(),
                    ParameterValue {
                        parts: vec![
                            ValuePart::Var(VariableExpression {
                                var_path: "b".to_string(),
                                ..Default::default()
                            }),
                            ValuePart::Text("#junk".to_string()),
                        ],
                    },
                )]),
            }),
        );

        assert_eq!(
            URLPathTemplate::parse("/location/{lat},{lon}"),
            Ok(URLPathTemplate {
                path: vec![
                    ParameterValue {
                        parts: vec![ValuePart::Text("location".to_string())],
                    },
                    ParameterValue {
                        parts: vec![
                            ValuePart::Var(VariableExpression {
                                var_path: "lat".to_string(),
                                required: true,
                                ..Default::default()
                            }),
                            ValuePart::Text(",".to_string()),
                            ValuePart::Var(VariableExpression {
                                var_path: "lon".to_string(),
                                required: true,
                                ..Default::default()
                            }),
                        ],
                    },
                ],
                ..Default::default()
            }),
        );

        assert_eq!(
            URLPathTemplate::parse("/point3/{x},{y},{z}?a={b}"),
            Ok(URLPathTemplate {
                path: vec![
                    ParameterValue {
                        parts: vec![ValuePart::Text("point3".to_string())],
                    },
                    ParameterValue {
                        parts: vec![
                            ValuePart::Var(VariableExpression {
                                var_path: "x".to_string(),
                                required: true,
                                ..Default::default()
                            }),
                            ValuePart::Text(",".to_string()),
                            ValuePart::Var(VariableExpression {
                                var_path: "y".to_string(),
                                required: true,
                                ..Default::default()
                            }),
                            ValuePart::Text(",".to_string()),
                            ValuePart::Var(VariableExpression {
                                var_path: "z".to_string(),
                                required: true,
                                ..Default::default()
                            }),
                        ],
                    },
                ],
                query: IndexMap::from([(
                    "a".to_string(),
                    ParameterValue {
                        parts: vec![ValuePart::Var(VariableExpression {
                            var_path: "b".to_string(),
                            ..Default::default()
                        })],
                    },
                )]),
            }),
        );
    }

    #[test]
    fn test_generate_path() {
        let template = URLPathTemplate::parse("/users/{user_id}?a={b}&c={d!}&e={f.g}").unwrap();

        assert_eq!(
            template.generate_path(&json!("not an object")),
            Err(r#"Expected object, got "not an object""#.to_string()),
        );

        assert_eq!(
            template.generate_path(&json!({
                // A variables object without any properties
            })),
            Err("Missing required variable user_id in {}".to_string()),
        );

        assert_eq!(
            template.generate_path(&json!({
                "user_id": 123,
                "b": "456",
                "d": 789,
                "f.g": "abc",
            })),
            Ok("/users/123?a=456&c=789&e=abc".to_string()),
        );

        assert_eq!(
            template.generate_path(&json!({
                "user_id": 123,
                "d": 789,
                "f": "not an object",
            })),
            Ok("/users/123?c=789".to_string()),
        );

        assert_eq!(
            template.generate_path(&json!({
                "b": "456",
                "f.g": "abc",
                "user_id": "123",
            })),
            Err(
                r#"Missing required variable d in {"b":"456","f.g":"abc","user_id":"123"}"#
                    .to_string()
            ),
        );

        assert_eq!(
            template.generate_path(&json!({
                // The order of the variables should not matter.
                "d": "789",
                "b": "456",
                "user_id": "123",
            })),
            Ok("/users/123?a=456&c=789".to_string()),
        );

        assert_eq!(
            template.generate_path(&json!({
                "user_id": "123",
                "b": "a",
                "d": "c",
                "f.g": "e",
                // Extra variables should be ignored.
                "extra": "ignored",
            })),
            Ok("/users/123?a=a&c=c&e=e".to_string()),
        );

        let template_with_nested_required_var =
            URLPathTemplate::parse("/repositories/{user.login}/{repo.name}?testing={a.b.c!}")
                .unwrap();

        assert_eq!(
            template_with_nested_required_var.generate_path(&json!({
                "repo.name": "repo",
                "user.login": "user",
            })),
            Err(
                r#"Missing required variable a.b.c in {"repo.name":"repo","user.login":"user"}"#
                    .to_string()
            ),
        );

        assert_eq!(
            template_with_nested_required_var.generate_path(&json!({
                "user.login": "user",
                "repo.name": "repo",
                "a.b.c": "value",
            })),
            Ok("/repositories/user/repo?testing=value".to_string()),
        );
    }

    #[test]
    fn test_generate_path_empty() {
        assert_eq!(
            URLPathTemplate::parse("")
                .unwrap()
                .generate_path(&json!({}))
                .unwrap(),
            "/".to_string()
        );

        assert_eq!(
            URLPathTemplate::parse("/")
                .unwrap()
                .generate_path(&json!({}))
                .unwrap(),
            "/".to_string()
        );

        assert_eq!(
            URLPathTemplate::parse("?foo=bar")
                .unwrap()
                .generate_path(&json!({}))
                .unwrap(),
            "/?foo=bar".to_string()
        );
    }

    #[test]
    fn test_batch_expressions() {
        assert_eq!(
            URLPathTemplate::parse("/users?ids={id,...}"),
            Ok(URLPathTemplate {
                path: vec![ParameterValue {
                    parts: vec![ValuePart::Text("users".to_string())],
                }],
                query: IndexMap::from([(
                    "ids".to_string(),
                    ParameterValue {
                        parts: vec![ValuePart::Var(VariableExpression {
                            var_path: "id".to_string(),
                            batch_separator: Some(",".to_string()),
                            ..Default::default()
                        })],
                    },
                )]),
            }),
        );

        assert_eq!(
            URLPathTemplate::parse("/v1/products?ids={id ...}&names={name|...}"),
            Ok(URLPathTemplate {
                path: vec![
                    ParameterValue {
                        parts: vec![ValuePart::Text("v1".to_string())]
                    },
                    ParameterValue {
                        parts: vec![ValuePart::Text("products".to_string())]
                    },
                ],
                query: IndexMap::from([
                    (
                        "ids".to_string(),
                        ParameterValue {
                            parts: vec![ValuePart::Var(VariableExpression {
                                var_path: "id".to_string(),
                                batch_separator: Some(" ".to_string()),
                                ..Default::default()
                            })],
                        },
                    ),
                    (
                        "names".to_string(),
                        ParameterValue {
                            parts: vec![ValuePart::Var(VariableExpression {
                                var_path: "name".to_string(),
                                batch_separator: Some("|".to_string()),
                                ..Default::default()
                            })],
                        },
                    ),
                ]),
            }),
        );

        assert_eq!(
            URLPathTemplate::parse("/people?ids={person.id,...}"),
            Ok(URLPathTemplate {
                path: vec![ParameterValue {
                    parts: vec![ValuePart::Text("people".to_string())],
                }],
                query: IndexMap::from([(
                    "ids".to_string(),
                    ParameterValue {
                        parts: vec![ValuePart::Var(VariableExpression {
                            var_path: "person.id".to_string(),
                            batch_separator: Some(",".to_string()),
                            ..Default::default()
                        })],
                    },
                )]),
            }),
        );

        assert_eq!(
            URLPathTemplate::parse("/people/{uid}/notes?ids={note_id;...}"),
            Ok(URLPathTemplate {
                path: vec![
                    ParameterValue {
                        parts: vec![ValuePart::Text("people".to_string())],
                    },
                    ParameterValue {
                        parts: vec![ValuePart::Var(VariableExpression {
                            var_path: "uid".to_string(),
                            required: true,
                            ..Default::default()
                        })],
                    },
                    ParameterValue {
                        parts: vec![ValuePart::Text("notes".to_string())],
                    },
                ],
                query: IndexMap::from([(
                    "ids".to_string(),
                    ParameterValue {
                        parts: vec![ValuePart::Var(VariableExpression {
                            var_path: "note_id".to_string(),
                            batch_separator: Some(";".to_string()),
                            ..Default::default()
                        })],
                    },
                )]),
            }),
        );

        assert_eq!(
            URLPathTemplate::parse("/people/by_uid:{uid}/notes?ids=[{note_id;...}]"),
            Ok(URLPathTemplate {
                path: vec![
                    ParameterValue {
                        parts: vec![ValuePart::Text("people".to_string())],
                    },
                    ParameterValue {
                        parts: vec![
                            ValuePart::Text("by_uid:".to_string()),
                            ValuePart::Var(VariableExpression {
                                var_path: "uid".to_string(),
                                required: true,
                                ..Default::default()
                            }),
                        ],
                    },
                    ParameterValue {
                        parts: vec![ValuePart::Text("notes".to_string())],
                    },
                ],

                query: IndexMap::from([(
                    "ids".to_string(),
                    ParameterValue {
                        parts: vec![
                            ValuePart::Text("[".to_string()),
                            ValuePart::Var(VariableExpression {
                                var_path: "note_id".to_string(),
                                batch_separator: Some(";".to_string()),
                                ..Default::default()
                            }),
                            ValuePart::Text("]".to_string()),
                        ],
                    },
                )]),
            }),
        );
    }

    #[test]
    fn test_batch_generation() {
        let template = URLPathTemplate::parse("/users?ids={id,...}").unwrap();

        assert_eq!(
            template.generate_path(&json!({
                "id": [1, 2, 3],
            })),
            Ok("/users?ids=1,2,3".to_string()),
        );

        assert_eq!(
            template.generate_path(&json!({
                "id": [1],
            })),
            Ok("/users?ids=1".to_string()),
        );

        assert_eq!(
            template.generate_path(&json!({
                "id": [],
            })),
            Ok("/users".to_string()),
        );

        assert_eq!(
            template.generate_path(&json!({
                "id": [1, 2, 3],
                "extra": "ignored",
            })),
            Ok("/users?ids=1,2,3".to_string()),
        );

        let template = URLPathTemplate::parse("/users?ids={id;...}&names={name|...}").unwrap();

        assert_eq!(
            template.generate_path(&json!({
                "id": [1, 2, 3],
                "name": ["a", "b", "c"],
            })),
            Ok("/users?ids=1;2;3&names=a|b|c".to_string()),
        );

        assert_eq!(
            template.generate_path(&json!({
                "id": 123,
                "name": "456",
            })),
            Ok("/users?ids=123&names=456".to_string()),
        );
    }

    #[test]
    fn test_extract_vars_from_url_path() {
        let repo_template = URLPathTemplate::parse("/repository/{user.login}/{repo.name}").unwrap();

        assert_eq!(
            repo_template.extract_vars("/repository/user/repo"),
            Ok(json!({
                "user.login": "user",
                "repo.name": "repo",
            })),
        );

        let template_with_query_params = URLPathTemplate::parse(
            "/contacts/{cid}/notes/{nid}?testing={a.b.c!}&testing2={a.b.d}&type={type}",
        )
        .unwrap();

        assert_eq!(
            template_with_query_params
                .extract_vars("/contacts/123/notes/456?testing=abc&testing2=def&type=ghi"),
            Ok(json!({
                "cid": "123",
                "nid": "456",
                "a.b.c": "abc",
                "a.b.d": "def",
                "type": "ghi",
            })),
        );

        assert_eq!(
            template_with_query_params
                .extract_vars("/contacts/123/notes/456?testing2=def&type=ghi"),
            Err("Missing required query parameter testing={a.b.c!}".to_string()),
        );

        assert_eq!(
            template_with_query_params.extract_vars("/contacts/123/notes/456?testing=789"),
            Ok(json!({
                "cid": "123",
                "nid": "456",
                "a.b.c": "789",
            })),
        );

        assert_eq!(
            template_with_query_params.extract_vars("/contacts/123/notes/{nid}?testing=abc"),
            Err("Unexpected variable expression {nid!}".to_string()),
        );

        assert_eq!(
            template_with_query_params.extract_vars("/contacts/123/notes/456?testing={wrong}"),
            Err("Unexpected variable expression {wrong}".to_string()),
        );

        assert_eq!(
            template_with_query_params.extract_vars("/wrong/123/notes/456?testing=abc"),
            Err("Constant text contacts not found in wrong".to_string()),
        );

        assert_eq!(
            template_with_query_params.extract_vars("/contacts/123/wrong/456?testing=abc"),
            Err("Constant text notes not found in wrong".to_string()),
        );

        let template_with_constant_query_param =
            URLPathTemplate::parse("/contacts/{cid}?constant=asdf&required={a!}&optional={b}")
                .unwrap();

        assert_eq!(
            template_with_constant_query_param
                .extract_vars("/contacts/123?required=456&optional=789"),
            // Since constant-valued query parameters do not affect the
            // extracted variables, we don't need to fail when they are missing
            // from a given URL.
            Ok(json!({
                "cid": "123",
                "a": "456",
                "b": "789",
            })),
        );

        assert_eq!(
            template_with_constant_query_param.generate_path(&json!({
                "cid": "123",
                "a": "456",
            })),
            Ok("/contacts/123?constant=asdf&required=456".to_string()),
        );

        assert_eq!(
            template_with_constant_query_param
                .extract_vars("/contacts/123?required=456&constant=asdf"),
            Ok(json!({
                "cid": "123",
                "a": "456",
            })),
        );

        assert_eq!(
            template_with_constant_query_param
                .extract_vars("/contacts/123?optional=789&required=456&constant=asdf"),
            Ok(json!({
                "cid": "123",
                "a": "456",
                "b": "789",
            })),
        );

        let template_with_constant_path_part =
            URLPathTemplate::parse("/users/123/notes/{nid}").unwrap();

        assert_eq!(
            template_with_constant_path_part.extract_vars("/users/123/notes/456"),
            Ok(json!({
                "nid": "456",
            })),
        );

        assert_eq!(
            template_with_constant_path_part.extract_vars("/users/123/notes/456?ignored=true"),
            Ok(json!({
                "nid": "456",
            })),
        );

        assert_eq!(
            template_with_constant_path_part.extract_vars("/users/abc/notes/456"),
            Err("Constant text 123 not found in abc".to_string()),
        );
    }

    #[test]
    fn test_multi_variable_parameter_values() {
        let template = URLPathTemplate::parse(
            "/locations/xyz({x},{y},{z})?required={b},{c};{d!}&optional=[{e},{f}]",
        )
        .unwrap();

        assert_eq!(
            template.generate_path(&json!({
                "x": 1,
                "y": 2,
                "z": 3,
                "b": 4,
                "c": 5,
                "d": 6,
                "e": 7,
                "f": 8,
            })),
            Ok("/locations/xyz(1,2,3)?required=4,5;6&optional=[7,8]".to_string()),
        );

        assert_eq!(
            template.generate_path(&json!({
                "x": 1,
                "y": 2,
                "z": 3,
                "b": 4,
                "c": 5,
                "d": 6,
                "e": 7,
                // "f": 8,
            })),
            Ok("/locations/xyz(1,2,3)?required=4,5;6".to_string()),
        );

        assert_eq!(
            template.generate_path(&json!({
                "x": 1,
                "y": 2,
                "z": 3,
                "b": 4,
                "c": 5,
                "d": 6,
                // "e": 7,
                "f": 8,
            })),
            Ok("/locations/xyz(1,2,3)?required=4,5;6".to_string()),
        );

        assert_eq!(
            template.generate_path(&json!({
                "x": 1,
                "y": 2,
                "z": 3,
                "b": 4,
                "c": 5,
                "d": 6,
            })),
            Ok("/locations/xyz(1,2,3)?required=4,5;6".to_string()),
        );

        assert_eq!(
            template.generate_path(&json!({
                // "x": 1,
                "y": 2,
                "z": 3,
            })),
            Err(r#"Missing required variable x in {"y":2,"z":3}"#.to_string()),
        );

        assert_eq!(
            template.generate_path(&json!({
                "x": 1,
                "y": 2,
                // "z": 3,
            })),
            Err(r#"Missing required variable z in {"x":1,"y":2}"#.to_string()),
        );

        assert_eq!(
            template.generate_path(&json!({
                "b": 4,
                "c": 5,
                "x": 1,
                "y": 2,
                "z": 3,
                // "d": 6,
            })),
            Err(r#"Missing required variable d in {"b":4,"c":5,"x":1,"y":2,"z":3}"#.to_string()),
        );

        assert_eq!(
            template.generate_path(&json!({
                "b": 4,
                // "c": 5,
                "d": 6,
                "x": 1,
                "y": 2,
                "z": 3,
            })),
            Err(r#"Missing variable c for required parameter {b},{c};{d!} given variables {"b":4,"d":6,"x":1,"y":2,"z":3}"#.to_string()),
        );

        assert_eq!(
            template.generate_path(&json!({
                // "b": 4,
                // "c": 5,
                "d": 6,
                "x": 1,
                "y": 2,
                "z": 3,
            })),
            Err(r#"Missing variable b for required parameter {b},{c};{d!} given variables {"d":6,"x":1,"y":2,"z":3}"#.to_string()),
        );

        assert_eq!(
            URLPathTemplate::parse(
                "/locations/xyz({x}{y}{z})?required={b},{c};{d!}&optional=[{e}{f},{g}]"
            ),
            Err("Ambiguous adjacent variable expressions in xyz({x}{y}{z})".to_string()),
        );

        assert_eq!(
            URLPathTemplate::parse(
                "/locations/xyz({x},{y},{z})?required={b}{c};{d!}&optional=[{e}{f},{g}]"
            ),
            Err("Ambiguous adjacent variable expressions in {b}{c};{d!}".to_string()),
        );

        assert_eq!(
            URLPathTemplate::parse(
                "/locations/xyz({x},{y},{z})?required={b},{c};{d!}&optional=[{e};{f}{g}]"
            ),
            Err("Ambiguous adjacent variable expressions in [{e};{f}{g}]".to_string()),
        );

        assert_eq!(
            template.extract_vars("/locations/xyz(1,2,3)?required=4,5;6&optional=[7,8]"),
            Ok(json!({
                "x": "1",
                "y": "2",
                "z": "3",
                "b": "4",
                "c": "5",
                "d": "6",
                "e": "7",
                "f": "8",
            })),
        );

        assert_eq!(
            template.extract_vars("/locations/xyz(3,2,1)?required=-5,10.1;2"),
            Ok(json!({
                "x": "3",
                "y": "2",
                "z": "1",
                "b": "-5",
                "c": "10.1",
                "d": "2",
            })),
        );

        assert_eq!(
            template.extract_vars("/locations/xyz(3,2,1)?optional=[-5,10.1;2]&required=6,7;8"),
            Ok(json!({
                "x": "3",
                "y": "2",
                "z": "1",
                "b": "6",
                "c": "7",
                "d": "8",
                "e": "-5",
                "f": "10.1;2",
            })),
        );

        assert_eq!(
            template.extract_vars("/locations/xyz(3,2,1?required=4,5;6)"),
            Err("Constant text ) not found in xyz(3,2,1".to_string()),
        );

        assert_eq!(
            template.extract_vars("/locations/xyz(3,2,1)?required=4,5,6"),
            Err("Constant text ; not found in 4,5,6".to_string()),
        );

        assert_eq!(
            template.extract_vars("/locations/xyz(3,2,1)?optional=[p,q]&required=4,5;6"),
            Ok(json!({
                "x": "3",
                "y": "2",
                "z": "1",
                "b": "4",
                "c": "5",
                "d": "6",
                "e": "p",
                "f": "q",
            })),
        );

        assert_eq!(
            template.extract_vars("/locations/xyz(3,2,1)?optional=(r,s)&required=4,5;6"),
            Err("Constant text [ not found in (r,s)".to_string()),
        );

        assert_eq!(
            template.extract_vars("/locations/xyz(3,2,1)?optional=[r,s)&required=4,5;6"),
            Err("Constant text ] not found in [r,s)".to_string()),
        );

        assert_eq!(
            template.extract_vars("/locations/xyz(1.25,2,3.5)?required=(4,5.1;6.6,7)"),
            Ok(json!({
                "x": "1.25",
                "y": "2",
                "z": "3.5",
                "b": "(4",
                "c": "5.1",
                "d": "6.6,7)",
            })),
        );

        let line_template =
            URLPathTemplate::parse("/line/{p1.x},{p1.y},{p1.z}/{p2.x},{p2.y},{p2.z}").unwrap();

        assert_eq!(
            line_template.generate_path(&json!({
                "p1.x": 1,
                "p1.y": 2,
                "p1.z": 3,
                "p2.x": 4,
                "p2.y": 5,
                "p2.z": 6,
            })),
            Ok("/line/1,2,3/4,5,6".to_string()),
        );

        assert_eq!(
            line_template.generate_path(&json!({
                "p1.x": 1,
                "p1.y": 2,
                "p1.z": 3,
                "p2.x": 4,
                "p2.y": 5,
                // "p2.z": 6,
            })),
            Err(r#"Missing required variable p2.z in {"p1.x":1,"p1.y":2,"p1.z":3,"p2.x":4,"p2.y":5}"#.to_string()),
        );

        assert_eq!(
            line_template.generate_path(&json!({
                "p1.x": 1,
                // "p1.y": 2,
                "p1.z": 3,
                "p2.x": 4,
                "p2.y": 5,
                "p2.z": 6,
            })),
            Err(r#"Missing required variable p1.y in {"p1.x":1,"p1.z":3,"p2.x":4,"p2.y":5,"p2.z":6}"#.to_string()),
        );

        assert_eq!(
            line_template.extract_vars("/line/6.6,5.5,4.4/3.3,2.2,1.1"),
            Ok(json!({
                "p1.x": "6.6",
                "p1.y": "5.5",
                "p1.z": "4.4",
                "p2.x": "3.3",
                "p2.y": "2.2",
                "p2.z": "1.1",
            })),
        );

        assert_eq!(
            line_template.extract_vars("/line/(6,5,4)/[3,2,1]"),
            Ok(json!({
                "p1.x": "(6",
                "p1.y": "5",
                "p1.z": "4)",
                "p2.x": "[3",
                "p2.y": "2",
                "p2.z": "1]",
            })),
        );

        assert_eq!(
            line_template.extract_vars("/line/6.6,5.5,4.4/3.3,2.2"),
            Err("Constant text , not found in 3.3,2.2".to_string()),
        );
    }

    #[test]
    fn test_extract_batch_vars() {
        let template_comma = URLPathTemplate::parse("/users?ids=[{id,...}]").unwrap();

        assert_eq!(
            template_comma.extract_vars("/users?ids=[1,2,3]"),
            Ok(json!({
                "id": ["1", "2", "3"],
            })),
        );

        assert_eq!(
            template_comma.extract_vars("/users?ids=[]"),
            Ok(json!({
                "id": [],
            })),
        );

        assert_eq!(
            template_comma.extract_vars("/users?ids=[123]&extra=ignored"),
            Ok(json!({
                "id": ["123"],
            })),
        );

        let template_semicolon = URLPathTemplate::parse("/columns/{a,...};{b,...}").unwrap();

        assert_eq!(
            template_semicolon.extract_vars("/columns/1;2"),
            Ok(json!({
                "a": ["1"],
                "b": ["2"],
            })),
        );

        assert_eq!(
            template_semicolon.extract_vars("/columns/1,2;3"),
            Ok(json!({
                "a": ["1", "2"],
                "b": ["3"],
            })),
        );

        assert_eq!(
            template_semicolon.extract_vars("/columns/1;2,3"),
            Ok(json!({
                "a": ["1"],
                "b": ["2", "3"],
            })),
        );

        assert_eq!(
            template_semicolon.extract_vars("/columns/1;2;3"),
            Ok(json!({
                "a": ["1"],
                "b": ["2;3"],
            })),
        );

        assert_eq!(
            template_semicolon.extract_vars("/columns/;3,2,1?extra=ignored"),
            Ok(json!({
                "a": [],
                "b": ["3", "2", "1"],
            })),
        );

        assert_eq!(
            template_semicolon.extract_vars("/columns/1,2,3;"),
            Ok(json!({
                "a": ["1", "2", "3"],
                "b": [],
            })),
        );

        assert_eq!(
            template_semicolon.extract_vars("/columns/1,2,3;9,8,7,6"),
            Ok(json!({
                "a": ["1", "2", "3"],
                "b": ["9", "8", "7", "6"],
            })),
        );

        assert_eq!(
            template_semicolon.extract_vars("/columns/;?extra=ignored"),
            Ok(json!({
                "a": [],
                "b": [],
            })),
        );
    }

    #[test]
    fn test_display_trait() {
        assert_eq!(
            format!(
                "{}",
                URLPathTemplate::parse("/users/{user_id}?a={b}&c={d!}&e={f.g}").unwrap()
            ),
            "/users/{user_id!}?a={b}&c={d!}&e={f.g}".to_string(),
        );

        assert_eq!(
            format!(
                "{}",
                URLPathTemplate::parse("/users/{user_id}?a={b}&c={d!}&e={f.g}").unwrap()
            ),
            "/users/{user_id!}?a={b}&c={d!}&e={f.g}".to_string(),
        );

        assert_eq!(
            format!(
                "{}",
                URLPathTemplate::parse("/users/{user_id}?a={b}&c={d!}&e={f.g}").unwrap()
            ),
            "/users/{user_id!}?a={b}&c={d!}&e={f.g}".to_string(),
        );

        assert_eq!(
            format!(
                "{}",
                URLPathTemplate::parse("/users?ids={id,...}&names={name|...}").unwrap()
            ),
            "/users?ids={id,...}&names={name|...}".to_string(),
        );

        assert_eq!(
            format!(
                "{}",
                URLPathTemplate::parse("/users?ids={id!,...}&names={user.name|...}").unwrap()
            ),
            "/users?ids={id!,...}&names={user.name|...}".to_string(),
        );

        assert_eq!(
            format!("{}", URLPathTemplate::parse("/position/{x},{y}").unwrap(),),
            "/position/{x!},{y!}".to_string(),
        );

        assert_eq!(
            format!(
                "{}",
                URLPathTemplate::parse("/position/xyz({x},{y},{z})").unwrap(),
            ),
            "/position/xyz({x!},{y!},{z!})".to_string(),
        );

        assert_eq!(
            format!(
                "{}",
                URLPathTemplate::parse("/position?xyz=({x},{y},{z})").unwrap(),
            ),
            "/position?xyz=({x},{y},{z})".to_string(),
        );
    }

    #[test]
    fn test_required_parameters() {
        assert_eq!(
            URLPathTemplate::parse("/users/{user_id}?a={b}&c={d.e!}&e={f.g}")
                .unwrap()
                .required_parameters(),
            vec!["d.e", "user_id"],
        );

        assert_eq!(
            URLPathTemplate::parse("/users?ids={id,...}&names={name|...}")
                .unwrap()
                .required_parameters(),
            Vec::<String>::new(),
        );

        assert_eq!(
            URLPathTemplate::parse("/users?ids={id!,...}&names={user.name|...}")
                .unwrap()
                .required_parameters(),
            vec!["id"],
        );

        assert_eq!(
            URLPathTemplate::parse("/position/{x},{y}")
                .unwrap()
                .required_parameters(),
            vec!["x", "y"],
        );

        assert_eq!(
            URLPathTemplate::parse("/position/xyz({x},{y},{z})")
                .unwrap()
                .required_parameters(),
            vec!["x", "y", "z"],
        );

        assert_eq!(
            URLPathTemplate::parse("/position?xyz=({x!},{y},{z!})")
                .unwrap()
                .required_parameters(),
            vec!["x", "z"],
        );

        assert_eq!(
            URLPathTemplate::parse("/users/{id}?user_id={id}")
                .unwrap()
                .required_parameters(),
            vec!["id"],
        );

        assert_eq!(
            URLPathTemplate::parse("/users/{$this.id}?foo={$this.bar!}")
                .unwrap()
                .required_parameters(),
            vec!["$this.bar", "$this.id"],
        );
    }
}
