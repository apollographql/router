use std::path::PathBuf;
use std::pin::Pin;
use std::time::Duration;

use derivative::Derivative;
use derive_more::Display;
use derive_more::From;
use futures::prelude::*;
use url::Url;

use crate::router::Event;
use crate::router::Event::NoMoreSchema;
use crate::router::Event::UpdateSchema;
use crate::uplink::schema_stream::SupergraphSdlQuery;
use crate::uplink::stream_from_uplink;
use crate::uplink::UplinkConfig;

type SchemaStream = Pin<Box<dyn Stream<Item = String> + Send>>;

/// The user supplied schema. Either a static string or a stream for hot reloading.
#[derive(From, Display, Derivative)]
#[derivative(Debug)]
#[non_exhaustive]
pub enum SchemaSource {
    /// A static schema.
    #[display(fmt = "String")]
    Static { schema_sdl: String },

    /// A stream of schema.
    #[display(fmt = "Stream")]
    Stream(#[derivative(Debug = "ignore")] SchemaStream),

    /// A YAML file that may be watched for changes.
    #[display(fmt = "File")]
    File {
        /// The path of the schema file.
        path: PathBuf,

        /// `true` to watch the file for changes and hot apply them.
        watch: bool,

        /// When watching, the delay to wait before applying the new schema.
        /// Note: This variable is deprecated and has no effect.
        #[deprecated]
        delay: Option<Duration>,
    },

    /// Apollo managed federation.
    #[display(fmt = "Registry")]
    Registry(UplinkConfig),

    /// A list of URLs to fetch the schema from.
    #[display(fmt = "URLs")]
    URLs {
        /// The URLs to fetch the schema from.
        urls: Vec<Url>,
        /// `true` to watch the URLs for changes and hot apply them.
        watch: bool,
        /// When watching, the delay to wait between each poll.
        period: Duration,
    },
}

impl From<&'_ str> for SchemaSource {
    fn from(s: &'_ str) -> Self {
        Self::Static {
            schema_sdl: s.to_owned(),
        }
    }
}

impl SchemaSource {
    /// Convert this schema into a stream regardless of if is static or not. Allows for unified handling later.
    pub(crate) fn into_stream(self) -> impl Stream<Item = Event> {
        match self {
            SchemaSource::Static { schema_sdl: schema } => {
                stream::once(future::ready(UpdateSchema(schema))).boxed()
            }
            SchemaSource::Stream(stream) => stream.map(UpdateSchema).boxed(),
            #[allow(deprecated)]
            SchemaSource::File {
                path,
                watch,
                delay: _,
            } => {
                // Sanity check, does the schema file exists, if it doesn't then bail.
                if !path.exists() {
                    tracing::error!(
                        "Supergraph schema at path '{}' does not exist.",
                        path.to_string_lossy()
                    );
                    stream::empty().boxed()
                } else {
                    //The schema file exists try and load it
                    match std::fs::read_to_string(&path) {
                        Ok(schema) => {
                            if watch {
                                crate::files::watch(&path)
                                    .filter_map(move |_| {
                                        let path = path.clone();
                                        async move {
                                            match tokio::fs::read_to_string(&path).await {
                                                Ok(schema) => Some(UpdateSchema(schema)),
                                                Err(err) => {
                                                    tracing::error!(reason = %err, "failed to read supergraph schema");
                                                    None
                                                }
                                            }
                                        }
                                    })
                                    .boxed()
                            } else {
                                stream::once(future::ready(UpdateSchema(schema))).boxed()
                            }
                        }
                        Err(err) => {
                            tracing::error!(reason = %err, "failed to read supergraph schema");
                            stream::empty().boxed()
                        }
                    }
                }
            }
            SchemaSource::Registry(uplink_config) => {
                stream_from_uplink::<SupergraphSdlQuery, String>(uplink_config)
                    .filter_map(|res| {
                        future::ready(match res {
                            Ok(schema) => Some(UpdateSchema(schema)),
                            Err(e) => {
                                tracing::error!("{}", e);
                                None
                            }
                        })
                    })
                    .boxed()
            }
            SchemaSource::URLs {
                urls,
                watch,
                period,
            } => {
                let mut fetcher = match Fetcher::new(urls, period) {
                    Ok(fetcher) => fetcher,
                    Err(err) => {
                        tracing::error!(reason = %err, "failed to fetch supergraph schema");
                        return stream::empty().boxed();
                    }
                };

                if watch {
                    stream::unfold(fetcher, |mut state| async move {
                        if state.first_call {
                            // First call we may terminate the stream if there are no viable urls, None may be returned
                            state
                                .fetch_supergraph_from_first_viable_url()
                                .await
                                .map(|event| (Some(event), state))
                        } else {
                            // Subsequent calls we don't want to terminate the stream, so we always return Some
                            Some(match state.fetch_supergraph_from_first_viable_url().await {
                                None => (None, state),
                                Some(event) => (Some(event), state),
                            })
                        }
                    })
                    .filter_map(|s| async move { s })
                    .boxed()
                } else {
                    futures::stream::once(async move {
                        fetcher.fetch_supergraph_from_first_viable_url().await
                    })
                    .filter_map(|s| async move { s })
                    .boxed()
                }
            }
        }
        .chain(stream::iter(vec![NoMoreSchema]))
        .boxed()
    }
}

#[derive(thiserror::Error, Debug)]
enum FetcherError {
    #[error("failed to build http client")]
    InitializationError(#[from] reqwest::Error),
}

// Encapsulates fetching the schema from the first viable url.
// It will try each url in order until it finds one that works.
// On the second and subsequent calls it will wait for the period before making the call.
struct Fetcher {
    client: reqwest::Client,
    urls: Vec<Url>,
    period: Duration,
    first_call: bool,
}

impl Fetcher {
    fn new(urls: Vec<Url>, period: Duration) -> Result<Self, FetcherError> {
        Ok(Self {
            client: reqwest::Client::builder()
                .no_gzip()
                .timeout(Duration::from_secs(10))
                .build()
                .map_err(FetcherError::InitializationError)?,
            urls,
            period,
            first_call: true,
        })
    }
    async fn fetch_supergraph_from_first_viable_url(&mut self) -> Option<Event> {
        // If this is not the first call then we need to wait for the period before trying again.
        if !self.first_call {
            tokio::time::sleep(self.period).await;
        }
        self.first_call = false;

        for url in &self.urls {
            match self
                .client
                .get(reqwest::Url::parse(url.as_ref()).unwrap())
                .send()
                .await
            {
                Ok(res) if res.status().is_success() => match res.text().await {
                    Ok(schema) => return Some(UpdateSchema(schema)),
                    Err(err) => {
                        tracing::warn!(
                            url.full = %url,
                            reason = %err,
                            "failed to fetch supergraph schema"
                        )
                    }
                },
                Ok(res) => tracing::warn!(
                    http.response.status_code = res.status().as_u16(),
                    url.full = %url,
                    "failed to fetch supergraph schema"
                ),
                Err(err) => tracing::warn!(
                    url.full = %url,
                    reason = %err,
                    "failed to fetch supergraph schema"
                ),
            }
        }
        tracing::error!("failed to fetch supergraph schema from all urls");
        None
    }
}

#[cfg(test)]
mod tests {
    use std::env::temp_dir;

    use futures::select;
    use test_log::test;
    use tracing_futures::WithSubscriber;
    use wiremock::matchers::method;
    use wiremock::matchers::path;
    use wiremock::Mock;
    use wiremock::MockServer;
    use wiremock::ResponseTemplate;

    use super::*;
    use crate::assert_snapshot_subscriber;
    use crate::files::tests::create_temp_file;
    use crate::files::tests::write_and_flush;

    #[test(tokio::test)]
    async fn schema_by_file_watching() {
        let (path, mut file) = create_temp_file();
        let schema = include_str!("../../testdata/supergraph.graphql");
        write_and_flush(&mut file, schema).await;
        let mut stream = SchemaSource::File {
            path,
            watch: true,
            delay: None,
        }
        .into_stream()
        .boxed();

        // First update is guaranteed
        assert!(matches!(stream.next().await.unwrap(), UpdateSchema(_)));

        // Need different contents, since we won't get an event if content is the same
        let schema_minimal = include_str!("../../testdata/minimal_supergraph.graphql");
        // Modify the file and try again
        write_and_flush(&mut file, schema_minimal).await;
        assert!(matches!(stream.next().await.unwrap(), UpdateSchema(_)));
    }

    #[test(tokio::test)]
    async fn schema_by_file_no_watch() {
        let (path, mut file) = create_temp_file();
        let schema = include_str!("../../testdata/supergraph.graphql");
        write_and_flush(&mut file, schema).await;

        let mut stream = SchemaSource::File {
            path,
            watch: false,
            delay: None,
        }
        .into_stream();
        assert!(matches!(stream.next().await.unwrap(), UpdateSchema(_)));
        assert!(matches!(stream.next().await.unwrap(), NoMoreSchema));
    }

    #[test(tokio::test)]
    async fn schema_by_file_missing() {
        let mut stream = SchemaSource::File {
            path: temp_dir().join("does_not_exist"),
            watch: true,
            delay: None,
        }
        .into_stream();

        // First update fails because the file is invalid.
        assert!(matches!(stream.next().await.unwrap(), NoMoreSchema));
    }

    const SCHEMA_1: &str = "schema1";
    const SCHEMA_2: &str = "schema2";
    #[test(tokio::test)]
    async fn schema_by_url() {
        async {
            let mock_server = MockServer::start().await;
            Mock::given(method("GET"))
                .and(path("/schema1"))
                .respond_with(ResponseTemplate::new(200).set_body_string(SCHEMA_1))
                .mount(&mock_server)
                .await;
            Mock::given(method("GET"))
                .and(path("/schema2"))
                .respond_with(ResponseTemplate::new(200).set_body_string(SCHEMA_2))
                .mount(&mock_server)
                .await;

            let mut stream = SchemaSource::URLs {
                urls: vec![
                    Url::parse(&format!("http://{}/schema1", mock_server.address())).unwrap(),
                    Url::parse(&format!("http://{}/schema2", mock_server.address())).unwrap(),
                ],
                watch: true,
                period: Duration::from_secs(1),
            }
            .into_stream();

            assert!(
                matches!(stream.next().await.unwrap(), UpdateSchema(schema) if schema == SCHEMA_1)
            );
            assert!(
                matches!(stream.next().await.unwrap(), UpdateSchema(schema) if schema == SCHEMA_1)
            );
        }
        .with_subscriber(assert_snapshot_subscriber!())
        .await;
    }

    #[test(tokio::test)]
    async fn schema_by_url_fallback() {
        async {
            let mock_server = MockServer::start().await;
            Mock::given(method("GET"))
                .and(path("/schema1"))
                .respond_with(ResponseTemplate::new(400))
                .mount(&mock_server)
                .await;
            Mock::given(method("GET"))
                .and(path("/schema2"))
                .respond_with(ResponseTemplate::new(200).set_body_string(SCHEMA_2))
                .mount(&mock_server)
                .await;

            let mut stream = SchemaSource::URLs {
                urls: vec![
                    Url::parse(&format!("http://{}/schema1", mock_server.address())).unwrap(),
                    Url::parse(&format!("http://{}/schema2", mock_server.address())).unwrap(),
                ],
                watch: true,
                period: Duration::from_secs(1),
            }
            .into_stream();

            assert!(
                matches!(stream.next().await.unwrap(), UpdateSchema(schema) if schema == SCHEMA_2)
            );
            assert!(
                matches!(stream.next().await.unwrap(), UpdateSchema(schema) if schema == SCHEMA_2)
            );
        }
        .with_subscriber(assert_snapshot_subscriber!({
            "[].fields[\"url.full\"]" => "[url.full]"
        }))
        .await;
    }

    #[test(tokio::test)]
    async fn schema_by_url_all_fail() {
        async {
            let mock_server = MockServer::start().await;
            Mock::given(method("GET"))
                .and(path("/schema1"))
                .respond_with(ResponseTemplate::new(400))
                .mount(&mock_server)
                .await;
            Mock::given(method("GET"))
                .and(path("/schema2"))
                .respond_with(ResponseTemplate::new(400))
                .mount(&mock_server)
                .await;

            let mut stream = SchemaSource::URLs {
                urls: vec![
                    Url::parse(&format!("http://{}/schema1", mock_server.address())).unwrap(),
                    Url::parse(&format!("http://{}/schema2", mock_server.address())).unwrap(),
                ],
                watch: true,
                period: Duration::from_secs(1),
            }
            .into_stream();

            assert!(matches!(stream.next().await.unwrap(), NoMoreSchema));
        }
        .with_subscriber(assert_snapshot_subscriber!({
            "[].fields[\"url.full\"]" => "[url.full]"
        }))
        .await;
    }
    #[test(tokio::test)]
    async fn schema_success_fail_success() {
        async {
            let mock_server = MockServer::start().await;
            let mut stream = SchemaSource::URLs {
                urls: vec![
                    Url::parse(&format!("http://{}/schema1", mock_server.address())).unwrap(),
                ],
                watch: true,
                period: Duration::from_secs(1),
            }
            .into_stream()
            .boxed()
            .fuse();

            let success = Mock::given(method("GET"))
                .and(path("/schema1"))
                .respond_with(ResponseTemplate::new(200).set_body_string(SCHEMA_1))
                .mount_as_scoped(&mock_server)
                .await;

            assert!(
                matches!(stream.next().await.unwrap(), UpdateSchema(schema) if schema == SCHEMA_1)
            );

            drop(success);

            // Next call will timeout
            assert!(select! {
                _res = stream.next() => false,
                _res = tokio::time::sleep(Duration::from_secs(2)).boxed().fuse() => true,

            });

            // Now we should get the schema again if the endpoint is back
            Mock::given(method("GET"))
                .and(path("/schema1"))
                .respond_with(ResponseTemplate::new(200).set_body_string(SCHEMA_1))
                .mount(&mock_server)
                .await;

            assert!(
                matches!(stream.next().await.unwrap(), UpdateSchema(schema) if schema == SCHEMA_1)
            );
        }
        .with_subscriber(assert_snapshot_subscriber!({
            "[].fields[\"url.full\"]" => "[url.full]"
        }))
        .await;
    }

    #[test(tokio::test)]
    async fn schema_no_watch() {
        async {
            let mock_server = MockServer::start().await;
            Mock::given(method("GET"))
                .and(path("/schema1"))
                .respond_with(ResponseTemplate::new(200).set_body_string(SCHEMA_1))
                .mount(&mock_server)
                .await;

            let mut stream = SchemaSource::URLs {
                urls: vec![
                    Url::parse(&format!("http://{}/schema1", mock_server.address())).unwrap(),
                ],
                watch: false,
                period: Duration::from_secs(1),
            }
            .into_stream();

            assert!(
                matches!(stream.next().await.unwrap(), UpdateSchema(schema) if schema == SCHEMA_1)
            );
            assert!(matches!(stream.next().await.unwrap(), NoMoreSchema));
        }
        .with_subscriber(assert_snapshot_subscriber!())
        .await;
    }
}
