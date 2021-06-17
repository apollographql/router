//! This is the model object for a QueryPlan
//! Copied from the old codebase

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub struct QueryPlan {
    pub node: Option<PlanNode>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase", tag = "kind")]
pub enum PlanNode {
    Sequence { nodes: Vec<PlanNode> },
    Parallel { nodes: Vec<PlanNode> },
    Fetch(FetchNode),
    Flatten(FlattenNode),
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FetchNode {
    pub service_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub requires: Option<SelectionSet>,
    pub variable_usages: Vec<String>,
    pub operation: GraphQLDocument,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FlattenNode {
    pub path: ResponsePath,
    pub node: Box<PlanNode>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase", tag = "kind")]
pub enum Selection {
    Field(Field),
    InlineFragment(InlineFragment),
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Field {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub alias: Option<String>,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub selections: Option<SelectionSet>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InlineFragment {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub type_condition: Option<String>,
    pub selections: SelectionSet,
}

pub type SelectionSet = Vec<Selection>;
pub type GraphQLDocument = String;
pub type ResponsePath = Vec<String>;

#[cfg(test)]
mod tests {
    use super::*;

    use serde_json::Value;
    const TYPENAME_FIELD_NAME: &'static str = "__typename";

    fn qp_json_string() -> String {
        include_str!("testdata/query_plan.json").to_owned()
    }

    fn query_plan() -> QueryPlan {
        QueryPlan {
            node: Some(PlanNode::Sequence {
                nodes: vec![
                    PlanNode::Fetch(FetchNode {
                        service_name: "product".to_owned(),
                        variable_usages: vec![],
                        requires: None,
                        operation: "{topProducts{__typename ...on Book{__typename isbn}...on Furniture{name}}product(upc:\"1\"){__typename ...on Book{__typename isbn}...on Furniture{name}}}".to_owned(),
                    }),
                    PlanNode::Parallel {
                        nodes: vec![
                            PlanNode::Sequence {
                                nodes: vec![
                                    PlanNode::Flatten(FlattenNode {
                                        path: vec![
                                            "topProducts".to_owned(), "@".to_owned()],
                                        node: Box::new(PlanNode::Fetch(FetchNode {
                                            service_name: "books".to_owned(),
                                            variable_usages: vec![],
                                            requires: Some(vec![
                                                Selection::InlineFragment(InlineFragment {
                                                    type_condition: Some("Book".to_owned()),
                                                    selections: vec![
                                                        Selection::Field(Field {
                                                            alias: None,
                                                            name: TYPENAME_FIELD_NAME.to_owned(),
                                                            selections: None,
                                                        }),
                                                        Selection::Field(Field {
                                                            alias: None,
                                                            name: "isbn".to_owned(),
                                                            selections: None,
                                                        })],
                                                })]),
                                            operation: "query($representations:[_Any!]!){_entities(representations:$representations){...on Book{__typename isbn title year}}}".to_owned(),
                                        })),
                                    }),
                                    PlanNode::Flatten(FlattenNode {
                                        path: vec![
                                            "topProducts".to_owned(),
                                            "@".to_owned()],
                                        node: Box::new(PlanNode::Fetch(FetchNode {
                                            service_name: "product".to_owned(),
                                            variable_usages: vec![],
                                            requires: Some(vec![
                                                Selection::InlineFragment(InlineFragment {
                                                    type_condition: Some("Book".to_owned()),
                                                    selections: vec![
                                                        Selection::Field(Field {
                                                            alias: None,
                                                            name: TYPENAME_FIELD_NAME.to_owned(),
                                                            selections: None,
                                                        }),
                                                        Selection::Field(Field {
                                                            alias: None,
                                                            name: "isbn".to_owned(),
                                                            selections: None,
                                                        }),
                                                        Selection::Field(Field {
                                                            alias: None,
                                                            name: "title".to_owned(),
                                                            selections: None,
                                                        }),
                                                        Selection::Field(Field {
                                                            alias: None,
                                                            name: "year".to_owned(),
                                                            selections: None,
                                                        })],
                                                })]),
                                            operation: "query($representations:[_Any!]!){_entities(representations:$representations){...on Book{name}}}".to_owned(),
                                        })),
                                    })]
                            },
                            PlanNode::Sequence {
                                nodes: vec![
                                    PlanNode::Flatten(FlattenNode {
                                        path: vec!["product".to_owned()],
                                        node: Box::new(PlanNode::Fetch(FetchNode {
                                            service_name: "books".to_owned(),
                                            variable_usages: vec![],
                                            requires: Some(vec![
                                                Selection::InlineFragment(InlineFragment {
                                                    type_condition: Some("Book".to_owned()),
                                                    selections: vec![
                                                        Selection::Field(Field {
                                                            alias: None,
                                                            name: TYPENAME_FIELD_NAME.to_owned(),
                                                            selections: None,
                                                        }),
                                                        Selection::Field(Field {
                                                            alias: None,
                                                            name: "isbn".to_owned(),
                                                            selections: None,
                                                        })],
                                                })]),
                                            operation: "query($representations:[_Any!]!){_entities(representations:$representations){...on Book{__typename isbn title year}}}".to_owned(),
                                        })),
                                    }),
                                    PlanNode::Flatten(FlattenNode {
                                        path: vec!["product".to_owned()],
                                        node: Box::new(PlanNode::Fetch(FetchNode {
                                            service_name: "product".to_owned(),
                                            variable_usages: vec![],
                                            requires: Some(vec![
                                                Selection::InlineFragment(InlineFragment {
                                                    type_condition: Some("Book".to_owned()),
                                                    selections: vec![
                                                        Selection::Field(Field {
                                                            alias: None,
                                                            name: TYPENAME_FIELD_NAME.to_owned(),
                                                            selections: None,
                                                        }),
                                                        Selection::Field(Field {
                                                            alias: None,
                                                            name: "isbn".to_owned(),
                                                            selections: None,
                                                        }),
                                                        Selection::Field(Field {
                                                            alias: None,
                                                            name: "title".to_owned(),
                                                            selections: None,
                                                        }),
                                                        Selection::Field(Field {
                                                            alias: None,
                                                            name: "year".to_owned(),
                                                            selections: None,
                                                        })],
                                                })]),
                                            operation: "query($representations:[_Any!]!){_entities(representations:$representations){...on Book{name}}}".to_owned(),
                                        })),
                                    })]
                            }]
                    }]
            })
        }
    }

    #[test]
    fn query_plan_from_json() {
        assert_eq!(
            serde_json::from_str::<QueryPlan>(qp_json_string().as_str()).unwrap(),
            query_plan()
        );
    }

    #[test]
    fn query_plan_into_json() {
        assert_eq!(
            serde_json::to_value(query_plan()).unwrap(),
            serde_json::from_str::<Value>(qp_json_string().as_str()).unwrap()
        );
    }
}
