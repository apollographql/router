use std::error::Error as stdError;
use std::fmt::Debug;
use std::time::Duration;
use std::time::Instant;

use futures::Future;
use futures::Stream;
use futures::StreamExt;
use graphql_client::QueryBody;
use thiserror::Error;
use tokio::sync::mpsc::channel;
use tokio_stream::wrappers::ReceiverStream;
use tower::BoxError;
use tracing::instrument::WithSubscriber;
use url::Url;

pub(crate) mod license_enforcement;
pub(crate) mod license_stream;
pub(crate) mod persisted_queries_manifest_stream;
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

#[derive(Debug)]
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

#[derive(Debug, Clone)]
pub enum Endpoints {
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
            [GCP_URL, AWS_URL]
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

/// Configuration for polling Apollo Uplink.
/// This struct does not change on router reloads - they are all sourced from CLI options.
#[derive(Debug, Clone, Default)]
pub struct UplinkConfig {
    /// The Apollo key: `<YOUR_GRAPH_API_KEY>`
    pub apollo_key: String,

    /// The apollo graph reference: `<YOUR_GRAPH_ID>@<VARIANT>`
    pub apollo_graph_ref: String,

    /// The endpoints polled.
    pub endpoints: Option<Endpoints>,

    /// The duration between polling
    pub poll_interval: Duration,

    /// The HTTP client timeout for each poll
    pub timeout: Duration,
}

impl UplinkConfig {
    /// Mock uplink configuration options for use in tests
    /// A nice pattern is to use wiremock to start an uplink mocker and pass the URL here.
    pub fn for_tests(uplink_endpoints: Endpoints) -> Self {
        Self {
            apollo_key: "key".to_string(),
            apollo_graph_ref: "graph".to_string(),
            endpoints: Some(uplink_endpoints),
            poll_interval: Duration::from_secs(2),
            timeout: Duration::from_secs(5),
        }
    }
}

/// Regularly fetch from Uplink
/// If urls are supplied then they will be called round robin
pub(crate) fn stream_from_uplink<Query, Response>(
    uplink_config: UplinkConfig,
) -> impl Stream<Item = Result<Response, Error>>
where
    Query: graphql_client::GraphQLQuery,
    <Query as graphql_client::GraphQLQuery>::ResponseData: Into<UplinkResponse<Response>> + Send,
    <Query as graphql_client::GraphQLQuery>::Variables: From<UplinkRequest> + Send + Sync,
    Response: Send + 'static + Debug,
{
    stream_from_uplink_transforming_new_response::<Query, Response, Response>(
        uplink_config,
        |response| Box::new(Box::pin(async { Ok(response) })),
    )
}

/// Like stream_from_uplink, but applies an async transformation function to the
/// result of the HTTP fetch if the response is an UplinkResponse::New. If this
/// function returns Err, we fail over to the next Uplink endpoint, just like if
/// the HTTP fetch itself failed. This serves the use case where an Uplink
/// endpoint's response includes another URL located close to the Uplink
/// endpoint; if that second URL is down, we want to try the next Uplink
/// endpoint rather than fully giving up.
pub(crate) fn stream_from_uplink_transforming_new_response<Query, Response, TransformedResponse>(
    mut uplink_config: UplinkConfig,
    transform_new_response: impl Fn(
            Response,
        )
            -> Box<dyn Future<Output = Result<TransformedResponse, BoxError>> + Send + Unpin>
        + Send
        + Sync
        + 'static,
) -> impl Stream<Item = Result<TransformedResponse, Error>>
where
    Query: graphql_client::GraphQLQuery,
    <Query as graphql_client::GraphQLQuery>::ResponseData: Into<UplinkResponse<Response>> + Send,
    <Query as graphql_client::GraphQLQuery>::Variables: From<UplinkRequest> + Send + Sync,
    Response: Send + 'static + Debug,
    TransformedResponse: Send + 'static + Debug,
{
    let query = query_name::<Query>();
    let (sender, receiver) = channel(2);
    let client = match reqwest::Client::builder()
        .timeout(uplink_config.timeout)
        .build()
    {
        Ok(client) => client,
        Err(err) => {
            tracing::error!("unable to create client to query uplink: {err}", err = err);
            return futures::stream::empty().boxed();
        }
    };

    let task = async move {
        let mut last_id = None;
        let mut endpoints = uplink_config.endpoints.unwrap_or_default();
        loop {
            let variables = UplinkRequest {
                graph_ref: uplink_config.apollo_graph_ref.to_string(),
                api_key: uplink_config.apollo_key.to_string(),
                id: last_id.clone(),
            };

            let query_body = Query::build_query(variables.into());

            match fetch::<Query, Response, TransformedResponse>(
                &client,
                &query_body,
                &mut endpoints.iter(),
                &transform_new_response,
            )
            .await
            {
                Ok(response) => {
                    tracing::info!(
                        monotonic_counter.apollo_router_uplink_fetch_count_total = 1u64,
                        status = "success",
                        query
                    );
                    match response {
                        UplinkResponse::New {
                            id,
                            response,
                            delay,
                        } => {
                            last_id = Some(id);
                            uplink_config.poll_interval = Duration::from_secs(delay);

                            if let Err(e) = sender.send(Ok(response)).await {
                                tracing::debug!("failed to push to stream. This is likely to be because the router is shutting down: {e}");
                                break;
                            }
                        }
                        UplinkResponse::Unchanged { id, delay } => {
                            // Preserve behavior for schema uplink errors where id and delay are not reset if they are not provided on error.
                            if let Some(id) = id {
                                last_id = Some(id);
                            }
                            if let Some(delay) = delay {
                                uplink_config.poll_interval = Duration::from_secs(delay);
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
                        monotonic_counter.apollo_router_uplink_fetch_count_total = 1u64,
                        status = "failure",
                        query
                    );
                    if let Err(e) = sender.send(Err(err)).await {
                        tracing::debug!("failed to send error to uplink stream. This is likely to be because the router is shutting down: {e}");
                        break;
                    }
                }
            }

            tokio::time::sleep(uplink_config.poll_interval).await;
        }
    };
    drop(tokio::task::spawn(task.with_current_subscriber()));

    ReceiverStream::new(receiver).boxed()
}

pub(crate) async fn fetch<Query, Response, TransformedResponse>(
    client: &reqwest::Client,
    request_body: &QueryBody<Query::Variables>,
    urls: &mut impl Iterator<Item = &Url>,
    // See stream_from_uplink_transforming_new_response for an explanation of
    // this argument.
    transform_new_response: &(impl Fn(
        Response,
    ) -> Box<dyn Future<Output = Result<TransformedResponse, BoxError>> + Send + Unpin>
          + Send
          + Sync
          + 'static),
) -> Result<UplinkResponse<TransformedResponse>, Error>
where
    Query: graphql_client::GraphQLQuery,
    <Query as graphql_client::GraphQLQuery>::ResponseData: Into<UplinkResponse<Response>> + Send,
    <Query as graphql_client::GraphQLQuery>::Variables: From<UplinkRequest> + Send + Sync,
    Response: Send + Debug + 'static,
    TransformedResponse: Send + Debug + 'static,
{
    let query = query_name::<Query>();
    for url in urls {
        let now = Instant::now();
        match http_request::<Query>(client, url.as_str(), request_body).await {
            Ok(response) => match response.data.map(Into::into) {
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
                Some(UplinkResponse::New {
                    response,
                    id,
                    delay,
                }) => {
                    tracing::info!(
                        histogram.apollo_router_uplink_fetch_duration_seconds =
                            now.elapsed().as_secs_f64(),
                        query,
                        url = url.to_string(),
                        "kind" = "new"
                    );
                    match transform_new_response(response).await {
                        Ok(res) => {
                            return Ok(UplinkResponse::New {
                                response: res,
                                id,
                                delay,
                            })
                        }
                        Err(err) => {
                            tracing::debug!(
                                    "failed to process results of Uplink response from {}: {}. Other endpoints will be tried",
                                    url,
                                    err
                                );
                            continue;
                        }
                    }
                }
                Some(UplinkResponse::Unchanged { id, delay }) => {
                    tracing::info!(
                        histogram.apollo_router_uplink_fetch_duration_seconds =
                            now.elapsed().as_secs_f64(),
                        query,
                        url = url.to_string(),
                        "kind" = "unchanged"
                    );
                    return Ok(UplinkResponse::Unchanged { id, delay });
                }
                Some(UplinkResponse::Error {
                    message,
                    code,
                    retry_later,
                }) => {
                    tracing::info!(
                        histogram.apollo_router_uplink_fetch_duration_seconds =
                            now.elapsed().as_secs_f64(),
                        query,
                        url = url.to_string(),
                        "kind" = "uplink_error",
                        error = message,
                        code
                    );
                    return Ok(UplinkResponse::Error {
                        message,
                        code,
                        retry_later,
                    });
                }
            },
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
    client: &reqwest::Client,
    url: &str,
    request_body: &QueryBody<Query::Variables>,
) -> Result<graphql_client::Response<Query::ResponseData>, reqwest::Error>
where
    Query: graphql_client::GraphQLQuery,
{
    // It is possible that istio-proxy is re-configuring networking beneath us. If it is, we'll see an error something like this:
    // level: "ERROR"
    // message: "fetch failed from all endpoints"
    // target: "apollo_router::router::event::schema"
    // timestamp: "2023-08-01T10:40:28.831196Z"
    // That's deeply confusing and very hard to debug. Let's try to help by printing out a helpful error message here
    let res = client
        .post(url)
        .json(request_body)
        .send()
        .await
        .map_err(|e| {
            if let Some(hyper_err) = e.source() {
                if let Some(os_err) = hyper_err.source() {
                    if os_err.to_string().contains("tcp connect error: Cannot assign requested address (os error 99)") {
                        tracing::warn!("If your router is executing within a kubernetes pod, this failure may be caused by istio-proxy injection. See https://github.com/apollographql/router/issues/3533 for more details about how to solve this");
                    }
                }
            }
            e
        })?;
    tracing::debug!("uplink response {:?}", res);
    let response_body: graphql_client::Response<Query::ResponseData> = res.json().await?;
    Ok(response_body)
}

#[cfg(test)]
mod test {
    use std::collections::VecDeque;
    use std::sync::Mutex;
    use std::time::Duration;

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
    use crate::uplink::stream_from_uplink_transforming_new_response;
    use crate::uplink::Endpoints;
    use crate::uplink::Error;
    use crate::uplink::UplinkConfig;
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

    #[allow(dead_code)]
    #[derive(Debug)]
    struct TransformedQueryResult {
        name: String,
        halved_ordering: i64,
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

    fn mock_uplink_config_with_fallback_urls(urls: Vec<Url>) -> UplinkConfig {
        UplinkConfig {
            apollo_key: "dummy_key".to_string(),
            apollo_graph_ref: "dummy_graph_ref".to_string(),
            endpoints: Some(Endpoints::fallback(urls)),
            poll_interval: Duration::from_secs(0),
            timeout: Duration::from_secs(1),
        }
    }

    fn mock_uplink_config_with_round_robin_urls(urls: Vec<Url>) -> UplinkConfig {
        UplinkConfig {
            apollo_key: "dummy_key".to_string(),
            apollo_graph_ref: "dummy_graph_ref".to_string(),
            endpoints: Some(Endpoints::round_robin(urls)),
            poll_interval: Duration::from_secs(0),
            timeout: Duration::from_secs(1),
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
            mock_uplink_config_with_fallback_urls(vec![url1, url2]),
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
            mock_uplink_config_with_round_robin_urls(vec![url1, url2]),
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
            mock_uplink_config_with_fallback_urls(vec![url1, url2]),
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
            mock_uplink_config_with_fallback_urls(vec![url1, url2]),
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
            mock_uplink_config_with_fallback_urls(vec![url1, url2, url3]),
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
            mock_uplink_config_with_fallback_urls(vec![url1, url2, url3]),
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
            mock_uplink_config_with_round_robin_urls(vec![url1, url2, url3]),
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
            mock_uplink_config_with_round_robin_urls(vec![url1, url2, url3]),
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
            .response(response_invalid_license())
            .build()
            .await;
        let results = stream_from_uplink::<TestQuery, QueryResult>(
            mock_uplink_config_with_round_robin_urls(vec![url1, url2, url3]),
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
            mock_uplink_config_with_round_robin_urls(vec![url1, url2, url3]),
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
            mock_uplink_config_with_round_robin_urls(vec![url1, url2]),
        )
        .take(1)
        .collect::<Vec<_>>()
        .await;
        assert_yaml_snapshot!(results.into_iter().map(to_friendly).collect::<Vec<_>>());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn stream_from_uplink_transforming_new_response_first_response_transform_fails() {
        let (mock_server, url1, url2, _url3) = init_mock_server().await;
        MockResponses::builder()
            .mock_server(&mock_server)
            .endpoint(&url1)
            .response(response_ok(15))
            .build()
            .await;
        MockResponses::builder()
            .mock_server(&mock_server)
            .endpoint(&url2)
            .response(response_ok(100))
            .build()
            .await;
        let results = stream_from_uplink_transforming_new_response::<
            TestQuery,
            QueryResult,
            TransformedQueryResult,
        >(
            mock_uplink_config_with_fallback_urls(vec![url1, url2]),
            |result| {
                Box::new(Box::pin(async move {
                    let QueryResult { name, ordering } = result;
                    if ordering % 2 == 0 {
                        // This will trigger on url2's response.
                        Ok(TransformedQueryResult {
                            name,
                            halved_ordering: ordering / 2,
                        })
                    } else {
                        // This will trigger on url1's response.
                        Err("cannot halve an odd number".into())
                    }
                }))
            },
        )
        .take(1)
        .collect::<Vec<_>>()
        .await;
        assert_yaml_snapshot!(results.into_iter().map(to_friendly).collect::<Vec<_>>());
    }

    fn to_friendly<R: std::fmt::Debug>(r: Result<R, Error>) -> Result<String, String> {
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

    fn response_invalid_license() -> ResponseTemplate {
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
