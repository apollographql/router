use std::time::Duration;

use futures::Stream;
use graphql_client::GraphQLQuery;
use graphql_client::QueryBody;
use graphql_client::Response;
use supergraph_sdl::FetchErrorCode;
use tokio::sync::mpsc::channel;
use tokio_stream::wrappers::ReceiverStream;
use tracing::instrument::WithSubscriber;
use url::Url;

const GCP_URL: &str = "https://uplink.api.apollographql.com/graphql";
const AWS_URL: &str = "https://aws.uplink.api.apollographql.com/graphql";

#[derive(GraphQLQuery)]
#[graphql(
    query_path = "query.graphql",
    schema_path = "uplink.graphql",
    request_derives = "Debug",
    response_derives = "PartialEq, Debug, Deserialize",
    deprecated = "warn"
)]

pub(crate) struct SupergraphSdl;

#[derive(Debug)]
pub enum Error {
    Reqwest(reqwest::Error),
    EmptyResponse,
    UpLink {
        code: FetchErrorCode,
        message: String,
    },
}

impl From<reqwest::Error> for Error {
    fn from(e: reqwest::Error) -> Self {
        Error::Reqwest(e)
    }
}

#[derive(Clone, Debug)]
pub struct Schema {
    pub id: String,
    pub schema: String,
}

/// regularly download a schema from Uplink
pub fn stream_supergraph(
    api_key: String,
    graph_ref: String,
    urls: Option<Vec<Url>>,
    interval: Duration,
) -> impl Stream<Item = Result<Schema, Error>> {
    let (sender, receiver) = channel(2);
    let _ = tokio::task::spawn(async move {
        let mut composition_id = None;

        let mut interval = tokio::time::interval(interval);
        let mut current_url_idx = 0;
        loop {
            match fetch_supergraph(
                api_key.to_string(),
                graph_ref.to_string(),
                composition_id.clone(),
                urls.as_ref().map(|u| &u[current_url_idx]),
            )
            .await
            {
                Ok(value) => match value.router_config {
                    supergraph_sdl::SupergraphSdlRouterConfig::RouterConfigResult(
                        schema_config,
                    ) => {
                        composition_id = Some(schema_config.id.clone());
                        if sender
                            .send(Ok(Schema {
                                id: schema_config.id,
                                schema: schema_config.supergraph_sdl,
                            }))
                            .await
                            .is_err()
                        {
                            break;
                        }
                    }
                    supergraph_sdl::SupergraphSdlRouterConfig::Unchanged => {
                        tracing::trace!("schema did not change");
                    }
                    supergraph_sdl::SupergraphSdlRouterConfig::FetchError(e) => {
                        if let Some(urls) = &urls {
                            current_url_idx = (current_url_idx + 1) % urls.len();
                        }
                        if sender
                            .send(Err(Error::UpLink {
                                code: e.code,
                                message: e.message,
                            }))
                            .await
                            .is_err()
                        {
                            break;
                        }
                    }
                },
                Err(err) => {
                    tracing::error!("error fetching supergraph from Uplink: {:?}", err);
                    if let Some(urls) = &urls {
                        current_url_idx = (current_url_idx + 1) % urls.len();
                    }
                    if sender.send(Err(err)).await.is_err() {
                        break;
                    }
                }
            }

            interval.tick().await;
        }
    })
    .with_current_subscriber();

    ReceiverStream::new(receiver)
}

pub async fn fetch_supergraph(
    api_key: String,
    graph_ref: String,
    composition_id: Option<String>,
    url: Option<&Url>,
) -> Result<supergraph_sdl::ResponseData, Error> {
    let variables = supergraph_sdl::Variables {
        api_key,
        graph_ref,
        if_after_id: composition_id,
    };
    let request_body = SupergraphSdl::build_query(variables);

    let response = match url {
        Some(url) => http_request(url.as_str(), &request_body).await?,
        None => match http_request(GCP_URL, &request_body).await {
            Ok(response) => response,
            Err(e) => {
                tracing::error!("could not get schema from GCP, trying AWS: {:?}", e);
                http_request(AWS_URL, &request_body).await?
            }
        },
    };

    match response.data {
        None => Err(Error::EmptyResponse),
        Some(response_data) => Ok(response_data),
    }
}

async fn http_request(
    url: &str,
    request_body: &QueryBody<supergraph_sdl::Variables>,
) -> Result<Response<supergraph_sdl::ResponseData>, reqwest::Error> {
    let client = reqwest::Client::new();

    let res = client.post(url).json(request_body).send().await?;
    let response_body: Response<supergraph_sdl::ResponseData> = res.json().await?;
    Ok(response_body)
}
