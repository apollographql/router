// With regards to ELv2 licensing, this entire file is license key functionality

// tonic does not derive `Eq` for the gRPC message types, which causes a warning from Clippy. The
// current suggestion is to explicitly allow the lint in the module that imports the protos.
// Read more: https://github.com/hyperium/tonic/issues/1056
#![allow(clippy::derive_partial_eq_without_eq)]

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

use self::supergraph_sdl::SupergraphSdlRouterConfigOnFetchError;

const GCP_URL: &str = "https://uplink.api.apollographql.com/graphql";
const AWS_URL: &str = "https://aws.uplink.api.apollographql.com/graphql";

#[derive(GraphQLQuery)]
#[graphql(
    query_path = "src/uplink/query.graphql",
    schema_path = "src/uplink/uplink.graphql",
    request_derives = "Debug",
    response_derives = "PartialEq, Debug, Deserialize",
    deprecated = "warn"
)]

pub(crate) struct SupergraphSdl;

#[derive(Debug)]
pub(crate) enum Error {
    Reqwest(reqwest::Error),
    EmptyResponse,
}

impl From<reqwest::Error> for Error {
    fn from(e: reqwest::Error) -> Self {
        Error::Reqwest(e)
    }
}

#[derive(Clone, Debug)]
pub(crate) struct Schema {
    pub(crate) schema: String,
}

/// regularly download a schema from Uplink
pub(crate) fn stream_supergraph(
    api_key: String,
    graph_ref: String,
    urls: Option<Vec<Url>>,
    mut interval: Duration,
    timeout: Duration,
) -> impl Stream<Item = Result<Schema, String>> {
    let (sender, receiver) = channel(2);
    let task = async move {
        let mut composition_id = None;
        let mut current_url_idx = 0;

        loop {
            let mut nb_errors = 0usize;
            match fetch_supergraph(
                &mut nb_errors,
                api_key.to_string(),
                graph_ref.to_string(),
                composition_id.clone(),
                urls.as_ref().map(|u| &u[current_url_idx]),
                timeout,
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
                                schema: schema_config.supergraph_sdl,
                            }))
                            .await
                            .is_err()
                        {
                            break;
                        }
                        // this will truncate the number of seconds to under u64::MAX, which should be
                        // a large enough delay anyway
                        interval =
                            Duration::from_secs(schema_config.min_delay_seconds.round() as u64);
                    }
                    supergraph_sdl::SupergraphSdlRouterConfig::Unchanged => {
                        tracing::trace!("schema did not change");
                    }
                    supergraph_sdl::SupergraphSdlRouterConfig::FetchError(
                        SupergraphSdlRouterConfigOnFetchError { code, message },
                    ) => {
                        if code == FetchErrorCode::RETRY_LATER {
                            if let Some(urls) = &urls {
                                current_url_idx = (current_url_idx + 1) % urls.len();
                            }

                            if sender
                                .send(Err(format!(
                                    "error downloading the schema from Uplink: {message}"
                                )))
                                .await
                                .is_err()
                            {
                                break;
                            }
                        } else {
                            if sender
                                .send(Err(format!("{code:?} error downloading the schema from Uplink, the router will not try again: {message}")))
                                .await
                                .is_err()
                            {
                                break;
                            }
                            break;
                        }
                    }
                },
                Err(err) => {
                    if let Some(urls) = &urls {
                        current_url_idx = (current_url_idx + 1) % urls.len();
                    }
                    tracing::error!("error downloading the schema from Uplink: {:?}", err);
                }
            }

            tokio::time::sleep(interval).await;
        }
    };
    drop(tokio::task::spawn(task.with_current_subscriber()));

    ReceiverStream::new(receiver)
}

pub(crate) async fn fetch_supergraph(
    nb_errors: &mut usize,
    api_key: String,
    graph_ref: String,
    composition_id: Option<String>,
    url: Option<&Url>,
    timeout: Duration,
) -> Result<supergraph_sdl::ResponseData, Error> {
    let variables = supergraph_sdl::Variables {
        api_key,
        graph_ref,
        if_after_id: composition_id,
    };
    let request_body = SupergraphSdl::build_query(variables);

    let response = match url {
        Some(url) => http_request(url.as_str(), &request_body, timeout).await?,
        None => match http_request(GCP_URL, &request_body, timeout).await {
            Ok(response) => {
                if *nb_errors > 0 {
                    *nb_errors = 0;
                    tracing::info!("successfully retrieved the schema from GCP");
                }
                response
            }
            Err(e) => {
                if *nb_errors == 3 {
                    tracing::error!("could not get schema from GCP, trying AWS: {:?}", e);
                } else {
                    tracing::debug!("could not get schema from GCP, trying AWS: {:?}", e);
                }
                *nb_errors += 1;
                http_request(AWS_URL, &request_body, timeout).await?
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
    timeout: Duration,
) -> Result<Response<supergraph_sdl::ResponseData>, reqwest::Error> {
    let client = reqwest::Client::builder().timeout(timeout).build()?;

    let res = client.post(url).json(request_body).send().await?;
    let response_body: Response<supergraph_sdl::ResponseData> = res.json().await?;
    Ok(response_body)
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
