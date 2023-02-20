// With regards to ELv2 licensing, this entire file is license key functionality

use std::time::Duration;

use futures::Stream;
use graphql_client::QueryBody;
use thiserror::Error;
use tokio::sync::mpsc::channel;
use tokio_stream::wrappers::ReceiverStream;
use tracing::instrument::WithSubscriber;
use url::Url;

//TODO Remove once everything is hooked up
#[allow(dead_code)]
pub(crate) mod entitlement;
#[allow(dead_code)]
pub(crate) mod entitlement_stream;
pub(crate) mod schema_stream;

const GCP_URL: &str = "https://uplink.api.apollographql.com/graphql";
const AWS_URL: &str = "https://aws.uplink.api.apollographql.com/graphql";

#[derive(Debug, Error)]
pub(crate) enum Error {
    #[error("http error")]
    Http(#[from] reqwest::Error),

    #[error("empty response")]
    EmptyResponse,

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

pub(crate) enum UplinkResponse<Response> {
    Result {
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

pub(crate) enum Endpoints {
    Fallback { urls: Vec<Url> },
    RoundRobin { urls: Vec<Url>, current: usize },
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
    Response: Send + 'static,
{
    let (sender, receiver) = channel(2);
    let task = async move {
        let mut last_id = None;
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

            match fetch::<Query>(&query_body, &mut endpoints.iter(), timeout).await {
                Ok(value) => {
                    let response: UplinkResponse<Response> = value.into();
                    match response {
                        UplinkResponse::Result {
                            id,
                            response,
                            delay,
                        } => {
                            last_id = Some(id);
                            if sender.send(Ok(response)).await.is_err() {
                                break;
                            }

                            interval = Duration::from_secs(delay);
                        }
                        UplinkResponse::Unchanged { id, delay } => {
                            tracing::trace!("uplink response did not change");
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
                            if sender.send(err).await.is_err() {
                                break;
                            }
                            if !retry_later {
                                break;
                            }
                        }
                    }
                }
                Err(err) => {
                    if sender.send(Err(err)).await.is_err() {
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

pub(crate) async fn fetch<Query>(
    request_body: &QueryBody<Query::Variables>,
    urls: &mut impl Iterator<Item = &Url>,
    timeout: Duration,
) -> Result<Query::ResponseData, Error>
where
    Query: graphql_client::GraphQLQuery,
{
    for url in urls {
        match http_request::<Query>(url.as_str(), request_body, timeout).await {
            Ok(response) => {
                return match response.data {
                    None => Err(Error::EmptyResponse),
                    Some(response_data) => Ok(response_data),
                }
            }
            Err(e) => {
                tracing::warn!("failed to fetch from Uplink endpoint {}: {}", url, e);
            }
        };
    }
    Err(Error::FetchFailed)
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
    use http::StatusCode;
    use insta::assert_yaml_snapshot;
    use serde_json::json;
    use url::Url;
    use wiremock::matchers::method;
    use wiremock::matchers::path;
    use wiremock::Mock;
    use wiremock::MockServer;
    use wiremock::Request;
    use wiremock::Respond;
    use wiremock::ResponseTemplate;

    use crate::uplink::entitlement::Entitlement;
    use crate::uplink::entitlement_stream::EntitlementRequest;
    use crate::uplink::stream_from_uplink;
    use crate::uplink::Endpoints;
    use crate::uplink::Error;

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
            .response(response_ok())
            .response(response_ok())
            .build()
            .await;
        MockResponses::builder()
            .mock_server(&mock_server)
            .endpoint(&url2)
            .build()
            .await;

        let results = stream_from_uplink::<EntitlementRequest, Entitlement>(
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
            .response(response_ok())
            .build()
            .await;
        MockResponses::builder()
            .mock_server(&mock_server)
            .response(response_ok())
            .endpoint(&url2)
            .build()
            .await;

        let results = stream_from_uplink::<EntitlementRequest, Entitlement>(
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
            .response(response_ok())
            .build()
            .await;
        let results = stream_from_uplink::<EntitlementRequest, Entitlement>(
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
        let results = stream_from_uplink::<EntitlementRequest, Entitlement>(
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
            .response(response_ok())
            .response(response_ok())
            .build()
            .await;
        MockResponses::builder()
            .mock_server(&mock_server)
            .endpoint(&url3)
            .build()
            .await;
        let results = stream_from_uplink::<EntitlementRequest, Entitlement>(
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
            .response(response_ok())
            .build()
            .await;
        MockResponses::builder()
            .mock_server(&mock_server)
            .endpoint(&url3)
            .response(response_ok())
            .build()
            .await;
        let results = stream_from_uplink::<EntitlementRequest, Entitlement>(
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
        let results = stream_from_uplink::<EntitlementRequest, Entitlement>(
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
            .response(response_ok())
            .response(response_unchanged())
            .response(response_ok())
            .build()
            .await;
        let results = stream_from_uplink::<EntitlementRequest, Entitlement>(
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
        let results = stream_from_uplink::<EntitlementRequest, Entitlement>(
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

    fn to_friendly(r: Result<Entitlement, Error>) -> Result<String, String> {
        match r {
            Ok(_) => Ok("response".to_string()),
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
                .unwrap_or_else(response_fetch_error_no_retry)
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

    fn response_ok() -> ResponseTemplate {
        ResponseTemplate::new(StatusCode::OK).set_body_json(json!(
            {
                "data":{
                    "routerEntitlements": {
                    "__typename": "RouterEntitlementsResult",
                    "id": "1",
                    "minDelaySeconds": 0,
                    "entitlement": {
                        "jwt": "eyJhbGciOiJFZERTQSJ9.eyJpc3MiOiJodHRwczovL3d3dy5hcG9sbG9ncmFwaHFsLmNvbS8iLCJzdWIiOiJhcG9sbG8iLCJhdWQiOiJTRUxGX0hPU1RFRCIsIndhcm5BdCI6MTY3NjgwODAwMCwiaGFsdEF0IjoxNjc4MDE3NjAwfQ.tXexfjZ2SQeqSwkWQ7zD4XBoxS_Hc5x7tSNJ3ln-BCL_GH7i3U9hsIgdRQTczCAjA_jjk34w39DeSV0nTc5WBw"
                        }
                    }
                }
            }))
    }

    fn response_invalid_entitlement() -> ResponseTemplate {
        ResponseTemplate::new(StatusCode::OK).set_body_json(json!(
        {
            "data":{
                "routerEntitlements": {
                    "__typename": "RouterEntitlementsResult",
                    "id": "1",
                    "minDelaySeconds": 0,
                    "entitlement": {
                        "jwt": "invalid"
                        }
                    }
                }
        }))
    }

    fn response_unchanged() -> ResponseTemplate {
        ResponseTemplate::new(StatusCode::OK).set_body_json(json!(
        {
            "data":{
                "routerEntitlements": {
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
                "routerEntitlements": {
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
                "routerEntitlements": {
                    "__typename": "FetchError",
                    "code": "NO_RETRY",
                    "message": "error message",
                }
            }
        }))
    }

    fn response_fetch_error_http() -> ResponseTemplate {
        ResponseTemplate::new(StatusCode::INTERNAL_SERVER_ERROR)
    }
}
