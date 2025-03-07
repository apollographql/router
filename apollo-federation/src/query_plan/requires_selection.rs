//! A selection set representation for `FetchNode::requires`:
//!
//! * Does not contain fragment spreads
//! * Is (de)serializable

use apollo_compiler::Name;
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
    pub selections: Vec<Selection>,
}
