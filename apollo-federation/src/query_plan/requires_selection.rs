//! A selection set representation for `FetchNode::requires`:
//!
//! * Does not contain fragment spreads
//! * Is (de)serializable

use apollo_compiler::Name;
use apollo_compiler::ast;
use apollo_compiler::name;
use serde::Deserialize;
use serde::Serialize;

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "PascalCase", tag = "kind")]
pub enum Selection {
    Field(Field),
    InlineFragment(InlineFragment),
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Field {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub alias: Option<Name>,
    pub name: Name,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    #[serde(default)]
    pub selections: Vec<Selection>,
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InlineFragment {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub type_condition: Option<Name>,
    /// The `@skip`/`@include` applications on this fragment, in document order.
    ///
    /// When any of these conditions excludes the fragment at execution time, the fragment's
    /// selections must not be part of the entity representation. This is how a `@requires`
    /// whose requiring field is itself under `@skip`/`@include` is expressed: the required
    /// fields are only required when that field is actually selected.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    #[serde(default)]
    pub conditions: Vec<Condition>,
    pub selections: Vec<Selection>,
}

/// A single `@skip(if:)` or `@include(if:)` application.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Condition {
    pub kind: ConditionKind,
    #[serde(rename = "if")]
    pub if_: ConditionValue,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ConditionKind {
    Skip,
    Include,
}

/// The `if:` argument of a `@skip`/`@include` application: either a literal boolean, or the
/// name of a boolean variable of the operation.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(untagged)]
pub enum ConditionValue {
    Boolean(bool),
    Variable(Name),
}

impl ConditionKind {
    pub fn directive_name(self) -> Name {
        match self {
            ConditionKind::Skip => name!("skip"),
            ConditionKind::Include => name!("include"),
        }
    }
}

impl Condition {
    /// Converts a `@skip`/`@include` directive application into a [`Condition`]. Returns `None`
    /// for any other directive, and an error if the application is malformed.
    pub fn from_directive(directive: &ast::Directive) -> Result<Option<Self>, String> {
        let kind = match directive.name.as_str() {
            "skip" => ConditionKind::Skip,
            "include" => ConditionKind::Include,
            _ => return Ok(None),
        };
        let Some(value) = directive.specified_argument_by_name("if") else {
            return Err(format!("@{} is missing its `if` argument", directive.name));
        };
        let if_ = match value.as_ref() {
            ast::Value::Boolean(boolean) => ConditionValue::Boolean(*boolean),
            ast::Value::Variable(name) => ConditionValue::Variable(name.clone()),
            other => {
                return Err(format!(
                    "@{} has invalid `if` argument value {other}",
                    directive.name
                ));
            }
        };
        Ok(Some(Self { kind, if_ }))
    }

    /// Converts this condition back into the equivalent directive application.
    pub fn to_directive(&self) -> ast::Directive {
        let value = match &self.if_ {
            ConditionValue::Boolean(boolean) => ast::Value::Boolean(*boolean),
            ConditionValue::Variable(name) => ast::Value::Variable(name.clone()),
        };
        ast::Directive {
            name: self.kind.directive_name(),
            arguments: vec![
                ast::Argument {
                    name: name!("if"),
                    value: value.into(),
                }
                .into(),
            ],
        }
    }

    /// Whether the fragment this condition applies to is included, given a way to look up the
    /// value of boolean variables. A variable that cannot be resolved is treated as if the
    /// condition did not exclude the fragment.
    pub fn is_included(&self, variable_value: impl Fn(&str) -> Option<bool>) -> bool {
        let value = match &self.if_ {
            ConditionValue::Boolean(boolean) => Some(*boolean),
            ConditionValue::Variable(name) => variable_value(name.as_str()),
        };
        !matches!(
            (self.kind, value),
            (ConditionKind::Skip, Some(true)) | (ConditionKind::Include, Some(false))
        )
    }
}

impl InlineFragment {
    /// Whether this fragment is included, given a way to look up the value of boolean variables.
    pub fn is_included(&self, variable_value: impl Fn(&str) -> Option<bool>) -> bool {
        self.conditions
            .iter()
            .all(|condition| condition.is_included(&variable_value))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn conditions_round_trip_through_json() {
        let fragment = InlineFragment {
            type_condition: Some(name!("T")),
            conditions: vec![
                Condition {
                    kind: ConditionKind::Include,
                    if_: ConditionValue::Variable(name!("c")),
                },
                Condition {
                    kind: ConditionKind::Skip,
                    if_: ConditionValue::Boolean(false),
                },
            ],
            selections: vec![],
        };
        let json = serde_json::to_value(&fragment).unwrap();
        assert_eq!(
            json,
            serde_json::json!({
                "typeCondition": "T",
                "conditions": [
                    { "kind": "include", "if": "c" },
                    { "kind": "skip", "if": false },
                ],
                "selections": [],
            })
        );
        let parsed: InlineFragment = serde_json::from_value(json).unwrap();
        assert_eq!(parsed, fragment);
    }

    #[test]
    fn fragments_without_conditions_deserialize_from_old_plans() {
        let parsed: InlineFragment = serde_json::from_value(serde_json::json!({
            "typeCondition": "T",
            "selections": [],
        }))
        .unwrap();
        assert!(parsed.conditions.is_empty());
    }

    #[test]
    fn evaluates_conditions() {
        let include = |name: &str| Condition {
            kind: ConditionKind::Include,
            if_: ConditionValue::Variable(Name::new(name).unwrap()),
        };
        let skip = |name: &str| Condition {
            kind: ConditionKind::Skip,
            if_: ConditionValue::Variable(Name::new(name).unwrap()),
        };
        let variables = |name: &str| match name {
            "yes" => Some(true),
            "no" => Some(false),
            _ => None,
        };
        assert!(include("yes").is_included(variables));
        assert!(!include("no").is_included(variables));
        assert!(!skip("yes").is_included(variables));
        assert!(skip("no").is_included(variables));
        // Unresolvable variables never exclude.
        assert!(include("unknown").is_included(variables));
        assert!(skip("unknown").is_included(variables));

        let fragment = InlineFragment {
            type_condition: None,
            conditions: vec![include("yes"), skip("yes")],
            selections: vec![],
        };
        assert!(!fragment.is_included(variables));
    }
}
