// With regards to ELv2 licensing, this entire file is license key functionality

use std::fmt::Debug;
use std::time::Duration;
use std::time::Instant;
use std::time::SystemTime;

use futures::Stream;
use graphql_client::QueryBody;
use thiserror::Error;
use tokio::sync::mpsc::channel;
use tokio_stream::wrappers::ReceiverStream;
use tracing::instrument::WithSubscriber;
use url::Url;

pub(crate) mod entitlement;
pub(crate) mod entitlement_stream;
pub(crate) mod schema_stream;

const GCP_URL: &str = "https://uplink.api.apollographql.com";
const AWS_URL: &str = "https://aws.uplink.api.apollographql.com";

#[derive(Debug, Error)]
pub(crate) enum Error {
    #[error("http error")]
    Http(#[from] reqwest::Error),

    #[error("fetch failed from all endpoints")]
    FetchFailed,

    #[error("uplink error: code={code} message={message}")]
    UplinkError { code: String, message: String },

    #[error("uplink error, the request will not be retried: code={code} message={message}")]
    UplinkErrorNoRetry { code: String, message: String },
}

pub(crate) struct UplinkRequest {
    api_key: String,
    graph_ref: String,
    id: Option<String>,
}

#[derive(Debug)]
pub(crate) enum UplinkResponse<Response>
where
    Response: Send + Debug + 'static,
{
    New {
        response: Response,
        id: String,
        delay: u64,
        ordering_id: SystemTime,
    },
    Unchanged {
        id: Option<String>,
        delay: Option<u64>,
    },
    Error {
        retry_later: bool,
        code: String,
        message: String,
    },
}

pub(crate) enum Endpoints {
    Fallback {
        urls: Vec<Url>,
    },
    #[allow(dead_code)]
    RoundRobin {
        urls: Vec<Url>,
        current: usize,
    },
}

impl Default for Endpoints {
    fn default() -> Self {
        Self::fallback(
            vec![GCP_URL, AWS_URL]
                .iter()
                .map(|url| Url::parse(url).expect("default urls must be valid"))
                .collect(),
        )
    }
}

impl Endpoints {
    pub(crate) fn fallback(urls: Vec<Url>) -> Self {
        Endpoints::Fallback { urls }
    }
    #[allow(dead_code)]
    pub(crate) fn round_robin(urls: Vec<Url>) -> Self {
        Endpoints::RoundRobin { urls, current: 0 }
    }

    /// Return an iterator of endpoints to check on a poll of uplink.
    /// Fallback will always return URLs in the same order.
    /// Round-robin will return an iterator that cycles over the URLS starting at the next URL
    fn iter<'a>(&'a mut self) -> Box<dyn Iterator<Item = &'a Url> + Send + 'a> {
        match self {
            Endpoints::Fallback { urls } => Box::new(urls.iter()),
            Endpoints::RoundRobin { urls, current } => {
                // Prevent current from getting large.
                *current %= urls.len();

                // The iterator cycles, but will skip to the next untried URL and is finally limited by the number of URLs.
                // This gives us a sliding window of URLs to try on each poll to uplink.
                // The returned iterator will increment current each time it is called.
                Box::new(
                    urls.iter()
                        .cycle()
                        .skip(*current)
                        .map(|url| {
                            *current += 1;
                            url
                        })
                        .take(urls.len()),
                )
            }
        }
    }
}

/// Regularly fetch from Uplink
/// If urls are supplied then they will be called round robin  
pub(crate) fn stream_from_uplink<Query, Response>(
    api_key: String,
    graph_ref: String,
    endpoints: Option<Endpoints>,
    mut interval: Duration,
    timeout: Duration,
) -> impl Stream<Item = Result<Response, Error>>
where
    Query: graphql_client::GraphQLQuery,
    <Query as graphql_client::GraphQLQuery>::ResponseData: Into<UplinkResponse<Response>> + Send,
    <Query as graphql_client::GraphQLQuery>::Variables: From<UplinkRequest> + Send + Sync,
    Response: Send + 'static + Debug,
{
    let (sender, receiver) = channel(2);
    let query = query_name::<Query>();
    let task = async move {
        let mut last_id = None;
        let mut last_ordering_id = SystemTime::UNIX_EPOCH;
        let mut endpoints = endpoints.unwrap_or_default();
        loop {
            let query_body = Query::build_query(
                UplinkRequest {
                    graph_ref: graph_ref.to_string(),
                    api_key: api_key.to_string(),
                    id: last_id.clone(),
                }
                .into(),
            );

            match fetch::<Query, Response>(
                &query_body,
                &mut endpoints.iter(),
                timeout,
                last_ordering_id,
            )
            .await
            {
                Ok(response) => {
                    tracing::info!(
                        counter.apollo_router_uplink_fetch_count_total = 1,
                        status = "success",
                        query
                    );
                    match response {
                        UplinkResponse::New {
                            id,
                            response,
                            delay,
                            ordering_id,
                        } => {
                            last_id = Some(id);
                            last_ordering_id = ordering_id;
                            interval = Duration::from_secs(delay);

                            if let Err(e) = sender.send(Ok(response)).await {
                                tracing::debug!("failed to push to stream. This is likely to be because the router is shutting down: {e}");
                                break;
                            }
                        }
                        UplinkResponse::Unchanged { id, delay } => {
                            tracing::debug!("uplink response did not change");
                            // Preserve behavior for schema uplink errors where id and delay are not reset if they are not provided on error.
                            if let Some(id) = id {
                                last_id = Some(id);
                            }
                            if let Some(delay) = delay {
                                interval = Duration::from_secs(delay);
                            }
                        }
                        UplinkResponse::Error {
                            retry_later,
                            message,
                            code,
                        } => {
                            let err = if retry_later {
                                Err(Error::UplinkError { code, message })
                            } else {
                                Err(Error::UplinkErrorNoRetry { code, message })
                            };
                            if let Err(e) = sender.send(err).await {
                                tracing::debug!("failed to send error to uplink stream. This is likely to be because the router is shutting down: {e}");
                                break;
                            }
                            if !retry_later {
                                break;
                            }
                        }
                    }
                }
                Err(err) => {
                    tracing::info!(
                        counter.apollo_router_uplink_fetch_count_total = 1,
                        status = "failure",
                        query
                    );
                    if let Err(e) = sender.send(Err(err)).await {
                        tracing::debug!("failed to send error to uplink stream. This is likely to be because the router is shutting down: {e}");
                        break;
                    }
                }
            }

            tokio::time::sleep(interval).await;
        }
    };
    drop(tokio::task::spawn(task.with_current_subscriber()));

    ReceiverStream::new(receiver)
}

pub(crate) async fn fetch<Query, Response>(
    request_body: &QueryBody<Query::Variables>,
    urls: &mut impl Iterator<Item = &Url>,
    timeout: Duration,
    last_ordering_id: SystemTime,
) -> Result<UplinkResponse<Response>, Error>
where
    Query: graphql_client::GraphQLQuery,
    <Query as graphql_client::GraphQLQuery>::ResponseData: Into<UplinkResponse<Response>> + Send,
    <Query as graphql_client::GraphQLQuery>::Variables: From<UplinkRequest> + Send + Sync,
    Response: Send + Debug + 'static,
{
    let query = query_name::<Query>();
    for url in urls {
        let now = Instant::now();
        match http_request::<Query>(url.as_str(), request_body, timeout).await {
            Ok(response) => {
                let response = response.data.map(Into::into);

                match &response {
                    None => {
                        tracing::info!(
                            histogram.apollo_router_uplink_fetch_duration_seconds =
                                now.elapsed().as_secs_f64(),
                            query,
                            url = url.to_string(),
                            "kind" = "uplink_error",
                            error = "empty response from uplink",
                        );
                    }
                    Some(UplinkResponse::New { ordering_id, .. })
                        if ordering_id > &last_ordering_id =>
                    {
                        tracing::info!(
                            histogram.apollo_router_uplink_fetch_duration_seconds =
                                now.elapsed().as_secs_f64(),
                            query,
                            url = url.to_string(),
                            "kind" = "new"
                        );
                        return Ok(response.expect("we are in the some branch, qed"));
                    }
                    Some(UplinkResponse::New { .. }) => {
                        tracing::info!(
                            histogram.apollo_router_uplink_fetch_duration_seconds =
                                now.elapsed().as_secs_f64(),
                            query,
                            url = url.to_string(),
                            "kind" = "ignored"
                        );

                        tracing::debug!(
                            "ignoring uplink event as is was equal to or older than our last known message. Other endpoints will be tried"
                        );
                        return Ok(UplinkResponse::Unchanged {
                            id: None,
                            delay: None,
                        });
                    }
                    Some(UplinkResponse::Unchanged { .. }) => {
                        tracing::info!(
                            histogram.apollo_router_uplink_fetch_duration_seconds =
                                now.elapsed().as_secs_f64(),
                            query,
                            url = url.to_string(),
                            "kind" = "unchanged"
                        );
                        return Ok(response.expect("we are in the some branch, qed"));
                    }
                    Some(UplinkResponse::Error { message, code, .. }) => {
                        tracing::info!(
                            histogram.apollo_router_uplink_fetch_duration_seconds =
                                now.elapsed().as_secs_f64(),
                            query,
                            url = url.to_string(),
                            "kind" = "uplink_error",
                            error = message,
                            code
                        );
                        return Ok(response.expect("we are in the some branch, qed"));
                    }
                }
            }
            Err(e) => {
                tracing::info!(
                    histogram.apollo_router_uplink_fetch_duration_seconds =
                        now.elapsed().as_secs_f64(),
                    query = std::any::type_name::<Query>(),
                    url = url.to_string(),
                    "kind" = "http_error",
                    error = e.to_string(),
                    code = e.status().unwrap_or_default().as_str()
                );
                tracing::debug!(
                    "failed to fetch from Uplink endpoint {}: {}. Other endpoints will be tried",
                    url,
                    e
                );
            }
        };
    }
    Err(Error::FetchFailed)
}

fn query_name<Query>() -> &'static str {
    let mut query = std::any::type_name::<Query>();
    query = query
        .strip_suffix("Query")
        .expect("Uplink structs mut be named xxxQuery")
        .get(query.rfind("::").map(|index| index + 2).unwrap_or_default()..)
        .expect("cannot fail");
    query
}

async fn http_request<Query>(
    url: &str,
    request_body: &QueryBody<Query::Variables>,
    timeout: Duration,
) -> Result<graphql_client::Response<Query::ResponseData>, reqwest::Error>
where
    Query: graphql_client::GraphQLQuery,
{
    let client = reqwest::Client::builder().timeout(timeout).build()?;
    let res = client.post(url).json(request_body).send().await?;
    tracing::debug!("uplink response {:?}", res);
    let response_body: graphql_client::Response<Query::ResponseData> = res.json().await?;
    Ok(response_body)
}

#[cfg(test)]
mod test {
    use std::collections::VecDeque;
    use std::sync::Mutex;
    use std::time::Duration;
    use std::time::SystemTime;

    use buildstructor::buildstructor;
    use futures::StreamExt;
    use graphql_client::GraphQLQuery;
    use http::StatusCode;
    use insta::assert_yaml_snapshot;
    use serde_json::json;
    use test_query::FetchErrorCode;
    use test_query::TestQueryUplinkQuery;
    use url::Url;
    use wiremock::matchers::method;
    use wiremock::matchers::path;
    use wiremock::Mock;
    use wiremock::MockServer;
    use wiremock::Request;
    use wiremock::Respond;
    use wiremock::ResponseTemplate;

    use crate::uplink::stream_from_uplink;
    use crate::uplink::Endpoints;
    use crate::uplink::Error;
    use crate::uplink::UplinkRequest;
    use crate::uplink::UplinkResponse;

    #[derive(GraphQLQuery)]
    #[graphql(
        query_path = "src/uplink/testdata/test_query.graphql",
        schema_path = "src/uplink/testdata/test_uplink.graphql",
        request_derives = "Debug",
        response_derives = "PartialEq, Debug, Deserialize",
        deprecated = "warn"
    )]
    pub(crate) struct TestQuery {}

    #[derive(Debug, Eq, PartialEq)]
    struct QueryResult {
        name: String,
        ordering: i64,
    }

    impl From<UplinkRequest> for test_query::Variables {
        fn from(req: UplinkRequest) -> Self {
            test_query::Variables {
                api_key: req.api_key,
                graph_ref: req.graph_ref,
                if_after_id: req.id,
            }
        }
    }

    impl From<test_query::ResponseData> for UplinkResponse<QueryResult> {
        fn from(response: test_query::ResponseData) -> Self {
            match response.uplink_query {
                TestQueryUplinkQuery::New(response) => UplinkResponse::New {
                    id: response.id,
                    delay: response.min_delay_seconds as u64,
                    ordering_id: SystemTime::UNIX_EPOCH
                        + Duration::from_secs(response.data.ordering as u64),
                    response: QueryResult {
                        name: response.data.name,
                        ordering: response.data.ordering,
                    },
                },
                TestQueryUplinkQuery::Unchanged(response) => UplinkResponse::Unchanged {
                    id: Some(response.id),
                    delay: Some(response.min_delay_seconds as u64),
                },
                TestQueryUplinkQuery::FetchError(error) => UplinkResponse::Error {
                    retry_later: error.code == FetchErrorCode::RETRY_LATER,
                    code: match error.code {
                        FetchErrorCode::AUTHENTICATION_FAILED => {
                            "AUTHENTICATION_FAILED".to_string()
                        }
                        FetchErrorCode::ACCESS_DENIED => "ACCESS_DENIED".to_string(),
                        FetchErrorCode::UNKNOWN_REF => "UNKNOWN_REF".to_string(),
                        FetchErrorCode::RETRY_LATER => "RETRY_LATER".to_string(),
                        FetchErrorCode::Other(other) => other,
                    },
                    message: error.message,
                },
            }
        }
    }

    #[test]
    #[cfg(not(windows))] // Don’t bother with line ending differences
    fn test_uplink_schema_is_up_to_date() {
        use std::path::PathBuf;

        use introspector_gadget::blocking::GraphQLClient;
        use introspector_gadget::introspect;
        use introspector_gadget::introspect::GraphIntrospectInput;

        let client = GraphQLClient::new(
            "https://uplink.api.apollographql.com/",
            reqwest::blocking::Client::new(),
        );

        let should_retry = true;
        let introspection_response = introspect::run(
            GraphIntrospectInput {
                headers: Default::default(),
            },
            &client,
            should_retry,
        )
        .unwrap();
        if introspection_response.schema_sdl != include_str!("uplink.graphql") {
            let path = PathBuf::from(std::env::var_os("OUT_DIR").unwrap()).join("uplink.graphql");
            std::fs::write(&path, introspection_response.schema_sdl).unwrap();
            panic!(
                "\n\nUplink schema is out of date. Run this command to update it:\n\n    \
                mv {} apollo-router/src/uplink/uplink.graphql\n\n",
                path.to_str().unwrap()
            );
        }
    }

    #[test]
    fn test_round_robin_endpoints() {
        let url1 = Url::parse("http://example1.com").expect("url must be valid");
        let url2 = Url::parse("http://example2.com").expect("url must be valid");
        let mut endpoints = Endpoints::round_robin(vec![url1.clone(), url2.clone()]);
        assert_eq!(endpoints.iter().collect::<Vec<_>>(), vec![&url1, &url2]);
        assert_eq!(endpoints.iter().next(), Some(&url1));
        assert_eq!(endpoints.iter().collect::<Vec<_>>(), vec![&url2, &url1]);
    }

    #[test]
    fn test_fallback_endpoints() {
        let url1 = Url::parse("http://example1.com").expect("url must be valid");
        let url2 = Url::parse("http://example2.com").expect("url must be valid");
        let mut endpoints = Endpoints::fallback(vec![url1.clone(), url2.clone()]);
        assert_eq!(endpoints.iter().collect::<Vec<_>>(), vec![&url1, &url2]);
        assert_eq!(endpoints.iter().next(), Some(&url1));
        assert_eq!(endpoints.iter().collect::<Vec<_>>(), vec![&url1, &url2]);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn stream_from_uplink_fallback() {
        let (mock_server, url1, url2, _url3) = init_mock_server().await;
        MockResponses::builder()
            .mock_server(&mock_server)
            .endpoint(&url1)
            .response(response_ok(1))
            .response(response_ok(2))
            .build()
            .await;
        MockResponses::builder()
            .mock_server(&mock_server)
            .endpoint(&url2)
            .build()
            .await;

        let results = stream_from_uplink::<TestQuery, QueryResult>(
            "dummy_key".to_string(),
            "dummy_graph_ref".to_string(),
            Some(Endpoints::fallback(vec![url1, url2])),
            Duration::from_secs(0),
            Duration::from_secs(1),
        )
        .take(2)
        .collect::<Vec<_>>()
        .await;
        assert_yaml_snapshot!(results.into_iter().map(to_friendly).collect::<Vec<_>>());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn stream_from_uplink_round_robin() {
        let (mock_server, url1, url2, _url3) = init_mock_server().await;
        MockResponses::builder()
            .mock_server(&mock_server)
            .endpoint(&url1)
            .response(response_ok(1))
            .build()
            .await;
        MockResponses::builder()
            .mock_server(&mock_server)
            .response(response_ok(2))
            .endpoint(&url2)
            .build()
            .await;

        let results = stream_from_uplink::<TestQuery, QueryResult>(
            "dummy_key".to_string(),
            "dummy_graph_ref".to_string(),
            Some(Endpoints::round_robin(vec![url1, url2])),
            Duration::from_secs(0),
            Duration::from_secs(1),
        )
        .take(2)
        .collect::<Vec<_>>()
        .await;
        assert_yaml_snapshot!(results.into_iter().map(to_friendly).collect::<Vec<_>>());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn stream_from_uplink_error_retry() {
        let (mock_server, url1, url2, _url3) = init_mock_server().await;
        MockResponses::builder()
            .mock_server(&mock_server)
            .endpoint(&url1)
            .response(response_fetch_error_retry())
            .response(response_ok(1))
            .build()
            .await;
        let results = stream_from_uplink::<TestQuery, QueryResult>(
            "dummy_key".to_string(),
            "dummy_graph_ref".to_string(),
            Some(Endpoints::fallback(vec![url1, url2])),
            Duration::from_secs(0),
            Duration::from_secs(1),
        )
        .take(2)
        .collect::<Vec<_>>()
        .await;
        assert_yaml_snapshot!(results.into_iter().map(to_friendly).collect::<Vec<_>>());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn stream_from_uplink_error_no_retry() {
        let (mock_server, url1, url2, _url3) = init_mock_server().await;
        MockResponses::builder()
            .mock_server(&mock_server)
            .endpoint(&url1)
            .response(response_fetch_error_no_retry())
            .build()
            .await;
        let results = stream_from_uplink::<TestQuery, QueryResult>(
            "dummy_key".to_string(),
            "dummy_graph_ref".to_string(),
            Some(Endpoints::fallback(vec![url1, url2])),
            Duration::from_secs(0),
            Duration::from_secs(1),
        )
        .collect::<Vec<_>>()
        .await;
        assert_yaml_snapshot!(results.into_iter().map(to_friendly).collect::<Vec<_>>());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn stream_from_uplink_error_http_fallback() {
        let (mock_server, url1, url2, url3) = init_mock_server().await;
        MockResponses::builder()
            .mock_server(&mock_server)
            .endpoint(&url1)
            .response(response_fetch_error_http())
            .response(response_fetch_error_http())
            .build()
            .await;
        MockResponses::builder()
            .mock_server(&mock_server)
            .endpoint(&url2)
            .response(response_ok(1))
            .response(response_ok(2))
            .build()
            .await;
        MockResponses::builder()
            .mock_server(&mock_server)
            .endpoint(&url3)
            .build()
            .await;
        let results = stream_from_uplink::<TestQuery, QueryResult>(
            "dummy_key".to_string(),
            "dummy_graph_ref".to_string(),
            Some(Endpoints::fallback(vec![url1, url2, url3])),
            Duration::from_secs(0),
            Duration::from_secs(1),
        )
        .take(2)
        .collect::<Vec<_>>()
        .await;
        assert_yaml_snapshot!(results.into_iter().map(to_friendly).collect::<Vec<_>>());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn stream_from_uplink_empty_http_fallback() {
        let (mock_server, url1, url2, url3) = init_mock_server().await;
        MockResponses::builder()
            .mock_server(&mock_server)
            .endpoint(&url1)
            .response(response_empty())
            .response(response_empty())
            .build()
            .await;
        MockResponses::builder()
            .mock_server(&mock_server)
            .endpoint(&url2)
            .response(response_ok(1))
            .response(response_ok(2))
            .build()
            .await;
        MockResponses::builder()
            .mock_server(&mock_server)
            .endpoint(&url3)
            .build()
            .await;
        let results = stream_from_uplink::<TestQuery, QueryResult>(
            "dummy_key".to_string(),
            "dummy_graph_ref".to_string(),
            Some(Endpoints::fallback(vec![url1, url2, url3])),
            Duration::from_secs(0),
            Duration::from_secs(1),
        )
        .take(2)
        .collect::<Vec<_>>()
        .await;
        assert_yaml_snapshot!(results.into_iter().map(to_friendly).collect::<Vec<_>>());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn stream_from_uplink_error_http_round_robin() {
        let (mock_server, url1, url2, url3) = init_mock_server().await;
        MockResponses::builder()
            .mock_server(&mock_server)
            .endpoint(&url1)
            .response(response_fetch_error_http())
            .build()
            .await;
        MockResponses::builder()
            .mock_server(&mock_server)
            .endpoint(&url2)
            .response(response_ok(1))
            .build()
            .await;
        MockResponses::builder()
            .mock_server(&mock_server)
            .endpoint(&url3)
            .response(response_ok(2))
            .build()
            .await;
        let results = stream_from_uplink::<TestQuery, QueryResult>(
            "dummy_key".to_string(),
            "dummy_graph_ref".to_string(),
            Some(Endpoints::round_robin(vec![url1, url2, url3])),
            Duration::from_secs(0),
            Duration::from_secs(1),
        )
        .take(2)
        .collect::<Vec<_>>()
        .await;
        assert_yaml_snapshot!(results.into_iter().map(to_friendly).collect::<Vec<_>>());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn stream_from_uplink_empty_http_round_robin() {
        let (mock_server, url1, url2, url3) = init_mock_server().await;
        MockResponses::builder()
            .mock_server(&mock_server)
            .endpoint(&url1)
            .response(response_empty())
            .build()
            .await;
        MockResponses::builder()
            .mock_server(&mock_server)
            .endpoint(&url2)
            .response(response_ok(1))
            .build()
            .await;
        MockResponses::builder()
            .mock_server(&mock_server)
            .endpoint(&url3)
            .response(response_ok(2))
            .build()
            .await;
        let results = stream_from_uplink::<TestQuery, QueryResult>(
            "dummy_key".to_string(),
            "dummy_graph_ref".to_string(),
            Some(Endpoints::round_robin(vec![url1, url2, url3])),
            Duration::from_secs(0),
            Duration::from_secs(1),
        )
        .take(2)
        .collect::<Vec<_>>()
        .await;
        assert_yaml_snapshot!(results.into_iter().map(to_friendly).collect::<Vec<_>>());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn stream_from_uplink_invalid() {
        let (mock_server, url1, url2, url3) = init_mock_server().await;
        MockResponses::builder()
            .mock_server(&mock_server)
            .endpoint(&url1)
            .response(response_invalid_entitlement())
            .build()
            .await;
        let results = stream_from_uplink::<TestQuery, QueryResult>(
            "dummy_key".to_string(),
            "dummy_graph_ref".to_string(),
            Some(Endpoints::round_robin(vec![url1, url2, url3])),
            Duration::from_secs(0),
            Duration::from_secs(1),
        )
        .take(1)
        .collect::<Vec<_>>()
        .await;
        assert_yaml_snapshot!(results.into_iter().map(to_friendly).collect::<Vec<_>>());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn stream_from_uplink_unchanged() {
        let (mock_server, url1, url2, url3) = init_mock_server().await;
        MockResponses::builder()
            .mock_server(&mock_server)
            .endpoint(&url1)
            .response(response_ok(1))
            .response(response_unchanged())
            .response(response_ok(2))
            .build()
            .await;
        let results = stream_from_uplink::<TestQuery, QueryResult>(
            "dummy_key".to_string(),
            "dummy_graph_ref".to_string(),
            Some(Endpoints::round_robin(vec![url1, url2, url3])),
            Duration::from_secs(0),
            Duration::from_secs(1),
        )
        .take(2)
        .collect::<Vec<_>>()
        .await;
        assert_yaml_snapshot!(results.into_iter().map(to_friendly).collect::<Vec<_>>());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn stream_from_uplink_failed_from_all() {
        let (mock_server, url1, url2, _url3) = init_mock_server().await;
        MockResponses::builder()
            .mock_server(&mock_server)
            .endpoint(&url1)
            .response(response_fetch_error_http())
            .build()
            .await;
        MockResponses::builder()
            .mock_server(&mock_server)
            .endpoint(&url2)
            .response(response_fetch_error_http())
            .build()
            .await;
        let results = stream_from_uplink::<TestQuery, QueryResult>(
            "dummy_key".to_string(),
            "dummy_graph_ref".to_string(),
            Some(Endpoints::round_robin(vec![url1, url2])),
            Duration::from_secs(0),
            Duration::from_secs(1),
        )
        .take(1)
        .collect::<Vec<_>>()
        .await;
        assert_yaml_snapshot!(results.into_iter().map(to_friendly).collect::<Vec<_>>());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn stream_with_ordering_skip_old() {
        let (mock_server, url1, url2, _url3) = init_mock_server().await;
        MockResponses::builder()
            .mock_server(&mock_server)
            .endpoint(&url1)
            .response(response_ok(2))
            .response(response_ok(3))
            .build()
            .await;
        MockResponses::builder()
            .mock_server(&mock_server)
            .endpoint(&url2)
            .response(response_ok(1))
            .build()
            .await;

        let results = stream_from_uplink::<TestQuery, QueryResult>(
            "dummy_key".to_string(),
            "dummy_graph_ref".to_string(),
            Some(Endpoints::round_robin(vec![url1, url2])),
            Duration::from_secs(0),
            Duration::from_secs(1),
        )
        .take(2)
        .collect::<Vec<_>>()
        .await;
        assert_yaml_snapshot!(results.into_iter().map(to_friendly).collect::<Vec<_>>());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn stream_with_ordering_skip_epoch() {
        let (mock_server, url1, url2, _url3) = init_mock_server().await;
        MockResponses::builder()
            .mock_server(&mock_server)
            .endpoint(&url1)
            .response(response_ok(0))
            .build()
            .await;
        MockResponses::builder()
            .mock_server(&mock_server)
            .endpoint(&url2)
            .response(response_ok(1))
            .build()
            .await;

        let results = stream_from_uplink::<TestQuery, QueryResult>(
            "dummy_key".to_string(),
            "dummy_graph_ref".to_string(),
            Some(Endpoints::round_robin(vec![url1, url2])),
            Duration::from_secs(0),
            Duration::from_secs(1),
        )
        .take(1)
        .collect::<Vec<_>>()
        .await;
        assert_yaml_snapshot!(results.into_iter().map(to_friendly).collect::<Vec<_>>());
    }

    fn to_friendly(r: Result<QueryResult, Error>) -> Result<String, String> {
        match r {
            Ok(e) => Ok(format!("result {:?}", e)),
            Err(e) => Err(e.to_string()),
        }
    }

    async fn init_mock_server() -> (MockServer, Url, Url, Url) {
        let mock_server = MockServer::start().await;
        let url1 =
            Url::parse(&format!("{}/endpoint1", mock_server.uri())).expect("url must be valid");
        let url2 =
            Url::parse(&format!("{}/endpoint2", mock_server.uri())).expect("url must be valid");
        let url3 =
            Url::parse(&format!("{}/endpoint3", mock_server.uri())).expect("url must be valid");
        (mock_server, url1, url2, url3)
    }

    struct MockResponses {
        responses: Mutex<VecDeque<ResponseTemplate>>,
    }

    impl Respond for MockResponses {
        fn respond(&self, _request: &Request) -> ResponseTemplate {
            self.responses
                .lock()
                .expect("lock poisoned")
                .pop_front()
                .unwrap_or_else(response_fetch_error_test_error)
        }
    }

    #[buildstructor]
    impl MockResponses {
        #[builder(entry = "builder")]
        async fn setup<'a>(
            mock_server: &'a MockServer,
            endpoint: &'a Url,
            responses: Vec<ResponseTemplate>,
        ) {
            let len = responses.len() as u64;
            Mock::given(method("POST"))
                .and(path(endpoint.path()))
                .respond_with(Self {
                    responses: Mutex::new(responses.into()),
                })
                .expect(len..len + 2)
                .mount(mock_server)
                .await;
        }
    }

    fn response_ok(ordering: u64) -> ResponseTemplate {
        ResponseTemplate::new(StatusCode::OK).set_body_json(json!(
        {
            "data":{
                "uplinkQuery": {
                "__typename": "New",
                "id": ordering.to_string(),
                "minDelaySeconds": 0,
                "data": {
                    "name": "ok",
                    "ordering": ordering,
                    }
                }
            }
        }))
    }

    fn response_invalid_entitlement() -> ResponseTemplate {
        ResponseTemplate::new(StatusCode::OK).set_body_json(json!(
        {
            "data":{
                "uplinkQuery": {
                    "__typename": "New",
                    "id": "3",
                    "minDelaySeconds": 0,
                    "garbage": "garbage"
                    }
                }
        }))
    }

    fn response_unchanged() -> ResponseTemplate {
        ResponseTemplate::new(StatusCode::OK).set_body_json(json!(
        {
            "data":{
                "uplinkQuery": {
                    "__typename": "Unchanged",
                    "id": "2",
                    "minDelaySeconds": 0,
                }
            }
        }))
    }

    fn response_fetch_error_retry() -> ResponseTemplate {
        ResponseTemplate::new(StatusCode::OK).set_body_json(json!(
        {
            "data":{
                "uplinkQuery": {
                    "__typename": "FetchError",
                    "code": "RETRY_LATER",
                    "message": "error message",
                }
            }
        }))
    }

    fn response_fetch_error_no_retry() -> ResponseTemplate {
        ResponseTemplate::new(StatusCode::OK).set_body_json(json!(
        {
            "data":{
                "uplinkQuery": {
                    "__typename": "FetchError",
                    "code": "NO_RETRY",
                    "message": "error message",
                }
            }
        }))
    }

    fn response_fetch_error_test_error() -> ResponseTemplate {
        ResponseTemplate::new(StatusCode::OK).set_body_json(json!(
        {
            "data":{
                "uplinkQuery": {
                    "__typename": "FetchError",
                    "code": "NO_RETRY",
                    "message": "unexpected mock request, make sure you have set up appropriate responses",
                }
            }
        }))
    }

    fn response_fetch_error_http() -> ResponseTemplate {
        ResponseTemplate::new(StatusCode::INTERNAL_SERVER_ERROR)
    }

    fn response_empty() -> ResponseTemplate {
        ResponseTemplate::new(StatusCode::OK).set_body_json(json!({ "data": null }))
    }
}
