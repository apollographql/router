use std::fmt::Display;
use std::str::FromStr;

use apollo_compiler::collections::IndexMap;
use itertools::Itertools;
use serde::Serialize;
use serde_json_bytes::Value;
use url::Url;

use crate::sources::connect::string_template::Error;
use crate::sources::connect::string_template::Expression;
use crate::sources::connect::string_template::StringTemplate;

/// A parser accepting URLTemplate syntax, which is useful both for
/// generating new URL paths from provided variables and for extracting variable
/// values from concrete URL paths.
///
// TODO: In the future, when we add RFC 6570 features, this could contain one big
//     `StringTemplate`, but for now we have to store pieces independently so we can leverage
//     `Url` to do percent-encoding for us.
#[derive(Debug, Clone, Default)]
pub struct URLTemplate {
    /// Scheme + host if this is an absolute URL
    pub base: Option<Url>,
    path: Vec<StringTemplate>,
    query: Vec<(StringTemplate, StringTemplate)>,
}

impl URLTemplate {
    pub(crate) fn path_expressions(&self) -> impl Iterator<Item = &Expression> {
        self.path.iter().flat_map(StringTemplate::expressions)
    }

    pub(crate) fn query_expressions(&self) -> impl Iterator<Item = &Expression> {
        self.query
            .iter()
            .flat_map(|(key, value)| key.expressions().chain(value.expressions()))
    }
    /// Return all variables in the template in the order they appeared
    pub(crate) fn expressions(&self) -> impl Iterator<Item = &Expression> {
        self.path_expressions().chain(self.query_expressions())
    }

    pub fn interpolate_path(&self, vars: &IndexMap<String, Value>) -> Result<Vec<String>, Error> {
        self.path
            .iter()
            .map(|param_value| param_value.interpolate(vars))
            .collect()
    }

    pub fn interpolate_query(
        &self,
        vars: &IndexMap<String, Value>,
    ) -> Result<Vec<(String, String)>, Error> {
        self.query
            .iter()
            .map(|(key, param_value)| Ok((key.interpolate(vars)?, param_value.interpolate(vars)?)))
            .collect()
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
                Url::parse(raw_base).map_err(|err| Error {
                    message: err.to_string(),
                    location: 0..raw_base.len(),
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
            .map(|path_part| {
                StringTemplate::parse(
                    path_part,
                    path_part.as_ptr() as usize - input.as_ptr() as usize,
                )
            })
            .try_collect()?;

        let query = query_suffix
            .into_iter()
            .flat_map(|query_suffix| query_suffix.split('&'))
            .map(|query_part| {
                let offset = query_part.as_ptr() as usize - input.as_ptr() as usize;
                let (key, value) = query_part.split_once('=').ok_or_else(|| {
                    let end = offset + query_part.len();
                    Error {
                        message: format!("Query parameter {query_part} must have a value"),
                        location: offset..end,
                    }
                })?;
                let key = StringTemplate::parse(key, offset)?;
                let value = StringTemplate::parse(
                    value,
                    value.as_ptr() as usize - input.as_ptr() as usize,
                )?;
                Ok::<(StringTemplate, StringTemplate), Self::Err>((key, value))
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

#[cfg(test)]
mod test_parse {
    use insta::assert_debug_snapshot;

    use super::*;

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

    #[test]
    fn nested_braces_in_expression() {
        assert_debug_snapshot!(URLTemplate::from_str("/position/xz/{$this { x { y } } }"));
    }

    #[test]
    fn expression_missing_closing_bracket() {
        assert_debug_snapshot!(URLTemplate::from_str("{$this { x: { y } }"));
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
    use serde_json_bytes::json;

    use super::*;

    #[test]
    fn missing_values_render_as_empty_strings() {
        let template = URLTemplate::from_str(
            "/something/{$args.id}/blah?{$args.filter.field}={$args.filter.value}",
        )
        .unwrap();
        let vars = IndexMap::default();
        assert_eq!(
            template.interpolate_query(&vars).unwrap(),
            &[("".to_string(), "".to_string())],
        );
        assert_eq!(
            template.interpolate_path(&vars).unwrap(),
            &["something".to_string(), "".to_string(), "blah".to_string()],
        );
    }

    #[test]
    fn nulls_render_as_empty_strings() {
        let template = URLTemplate::from_str(
            "/something/{$args.id}/blah?{$args.filter.field}={$args.filter.value}",
        )
        .unwrap();
        let mut vars = IndexMap::default();
        vars.insert(
            String::from("$args"),
            json!({"filter": {"field": null}, "id": null}),
        );
        assert_eq!(
            template.interpolate_query(&vars).unwrap(),
            &[("".to_string(), "".to_string())],
        );
        assert_eq!(
            template.interpolate_path(&vars).unwrap(),
            &["something".to_string(), "".to_string(), "blah".to_string()]
        );
    }

    #[test]
    fn interpolate_path() {
        let template = URLTemplate::from_str("/something/{$args.id->first}/blah").unwrap();
        let mut vars = IndexMap::default();
        vars.insert(String::from("$args"), json!({"id": "value"}));
        assert_eq!(
            template.interpolate_path(&vars).unwrap(),
            &["something".to_string(), "v".to_string(), "blah".to_string()],
            "Path parameter interpolated"
        );
    }

    #[test]
    fn interpolate_query() {
        let template = URLTemplate::from_str(
            "/something/{$args.id}/blah?{$args.filter.field}={$args.filter.value}",
        )
        .unwrap();
        let mut vars = IndexMap::default();
        vars.insert(
            String::from("$args"),
            json!({"filter": {"field": "name", "value": "value"}}),
        );
        vars.insert(
            String::from("$args.filter.value"),
            json!({"filter": { "value": "value"}}),
        );
        assert_eq!(
            template.interpolate_query(&vars).unwrap(),
            &[("name".to_string(), "value".to_string())],
        );
    }
}
