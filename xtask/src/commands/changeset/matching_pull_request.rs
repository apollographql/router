use graphql_client::GraphQLQuery;

use super::scalars::URI;

#[derive(GraphQLQuery)]
#[graphql(
    schema_path = "src/commands/changeset/github_api_schema.graphql",
    query_path = "src/commands/changeset/matching_pull_request.graphql"
)]
pub struct MatchingPullRequest;
