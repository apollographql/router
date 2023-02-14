// THIS FILE IS GENERATED
// THIS FILE IS GENERATED
// THIS FILE IS GENERATED
// See the instructions in `./mod.rs` for how to regenerate it.  It is
// generated based on the operation that sits alongside it in this same file.
// Unfortunately, this comment will not be preserved and needs to be manually
// preserved if it's desired to keep it around.  Luckily, I don't think this
// operation will change very often.
// THIS FILE IS GENERATED
// THIS FILE IS GENERATED
// THIS FILE IS GENERATED

#![allow(clippy::all, warnings)]
pub struct MatchingPullRequest;
pub mod matching_pull_request {
    #![allow(dead_code)]
    use std::result::Result;
    pub const OPERATION_NAME: &str = "MatchingPullRequest";
    pub const QUERY : & str = "# This operation is used to generate Rust code which lives in a file directly\n# next to this with the same name but a `.rs` extension.  For instructions on\n# how to generate the code, see the top of `./mod.rs`.\nfragment PrInfo on PullRequest {\n  url\n  number\n  author {\n    __typename\n    login\n  }\n  title\n  closingIssuesReferences(last: 4) {\n    nodes {\n      url\n      number\n      repository {\n        nameWithOwner\n      }\n    }\n  }\n  body\n}\nfragment PrSearchResult on SearchResultItemConnection {\n  issueCount\n  nodes {\n    __typename\n    ...PrInfo\n  }\n }\n\nquery MatchingPullRequest($search: String!) {\n  search(\n    type: ISSUE\n    query: $search\n    first: 1\n  ) {\n    ...PrSearchResult\n  }\n}\n" ;
    use serde::Deserialize;
    use serde::Serialize;

    use super::*;
    #[allow(dead_code)]
    type Boolean = bool;
    #[allow(dead_code)]
    type Float = f64;
    #[allow(dead_code)]
    type Int = i64;
    #[allow(dead_code)]
    type ID = String;
    type URI = crate::commands::changeset::scalars::URI;
    #[derive(Serialize)]
    pub struct Variables {
        pub search: String,
    }
    impl Variables {}
    #[derive(Deserialize, Debug)]
    pub struct PrInfo {
        pub url: URI,
        pub number: Int,
        pub author: Option<PrInfoAuthor>,
        pub title: String,
        #[serde(rename = "closingIssuesReferences")]
        pub closing_issues_references: Option<PrInfoClosingIssuesReferences>,
        pub body: String,
    }
    #[derive(Deserialize, Debug)]
    pub struct PrInfoAuthor {
        pub login: String,
        #[serde(flatten)]
        pub on: PrInfoAuthorOn,
    }
    #[derive(Deserialize, Debug)]
    #[serde(tag = "__typename")]
    pub enum PrInfoAuthorOn {
        Bot,
        EnterpriseUserAccount,
        Mannequin,
        Organization,
        User,
    }
    #[derive(Deserialize, Debug)]
    pub struct PrInfoClosingIssuesReferences {
        pub nodes: Option<Vec<Option<PrInfoClosingIssuesReferencesNodes>>>,
    }
    #[derive(Deserialize, Debug)]
    pub struct PrInfoClosingIssuesReferencesNodes {
        pub url: URI,
        pub number: Int,
        pub repository: PrInfoClosingIssuesReferencesNodesRepository,
    }
    #[derive(Deserialize, Debug)]
    pub struct PrInfoClosingIssuesReferencesNodesRepository {
        #[serde(rename = "nameWithOwner")]
        pub name_with_owner: String,
    }
    #[derive(Deserialize, Debug)]
    pub struct PrSearchResult {
        #[serde(rename = "issueCount")]
        pub issue_count: Int,
        pub nodes: Option<Vec<Option<PrSearchResultNodes>>>,
    }
    #[derive(Deserialize, Debug)]
    #[serde(tag = "__typename")]
    pub enum PrSearchResultNodes {
        App,
        Discussion,
        Issue,
        MarketplaceListing,
        Organization,
        PullRequest(PrSearchResultNodesOnPullRequest),
        Repository,
        User,
    }
    pub type PrSearchResultNodesOnPullRequest = PrInfo;
    #[derive(Deserialize, Debug)]
    pub struct ResponseData {
        pub search: MatchingPullRequestSearch,
    }
    pub type MatchingPullRequestSearch = PrSearchResult;
}
impl graphql_client::GraphQLQuery for MatchingPullRequest {
    type Variables = matching_pull_request::Variables;
    type ResponseData = matching_pull_request::ResponseData;
    fn build_query(variables: Self::Variables) -> ::graphql_client::QueryBody<Self::Variables> {
        graphql_client::QueryBody {
            variables,
            query: matching_pull_request::QUERY,
            operation_name: matching_pull_request::OPERATION_NAME,
        }
    }
}
