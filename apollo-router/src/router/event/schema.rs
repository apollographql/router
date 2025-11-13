use std::path::PathBuf;
use std::pin::Pin;
use std::time::Duration;

use derivative::Derivative;
use derive_more::Display;
use derive_more::From;
use futures::prelude::*;
use url::Url;

use crate::registry::fetch_oci;
use crate::router::Event;
use crate::router::Event::NoMoreSchema;
use crate::router::Event::UpdateSchema;
use crate::uplink::UplinkConfig;
use crate::uplink::schema::SchemaState;
use crate::uplink::schema_stream::SupergraphSdlQuery;
use crate::uplink::stream_from_uplink;

type SchemaStream = Pin<Box<dyn Stream<Item = String> + Send>>;

/// The user supplied schema. Either a static string or a stream for hot reloading.
#[derive(From, Display, Derivative)]
#[derivative(Debug)]
#[non_exhaustive]
pub enum SchemaSource {
    /// A static schema.
    #[display("String")]
    Static { schema_sdl: String },

    /// A stream of schema.
    #[display("Stream")]
    Stream(#[derivative(Debug = "ignore")] SchemaStream),

    /// A YAML file that may be watched for changes.
    #[display("File")]
    File {
        /// The path of the schema file.
        path: PathBuf,

        /// `true` to watch the file for changes and hot apply them.
        watch: bool,
    },

    /// Apollo managed federation.
    /// Validation happens in `into_stream()` when the schema is actually fetched.
    #[display("Registry")]
    Registry {
        /// The Apollo key (APOLLO_KEY) - validated in into_stream
        apollo_key: Option<String>,
        /// The Apollo graph reference (APOLLO_GRAPH_REF) - validated in into_stream
        apollo_graph_ref: Option<String>,
        /// The endpoints polled
        endpoints: Option<crate::uplink::Endpoints>,
        /// The duration between polling
        poll_interval: std::time::Duration,
        /// The HTTP client timeout for each poll
        timeout: std::time::Duration,
    },

    /// A list of URLs to fetch the schema from.
    #[display("URLs")]
    URLs {
        /// The URLs to fetch the schema from.
        urls: Vec<Url>,
    },

    /// OCI registry schema source.
    /// Validation happens in `into_stream()` when the schema is actually fetched.
    #[display("OCI")]
    OCI {
        /// The Apollo key (APOLLO_KEY)
        apollo_key: String,
        /// The graph artifact reference (validated in into_stream)
        reference: String,
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
                // If schema is empty, return empty stream without NoMoreSchema - this allows the config stream
                // to handle schema fetching (e.g., from graph_artifact_reference)
                // The config stream will emit UpdateSchema when ready, and we don't want to emit NoMoreSchema
                // prematurely which would cause the state machine to error
                if schema.is_empty() {
                    // Return empty stream without NoMoreSchema - config stream will handle schema
                    stream::empty().boxed()
                } else {
                    let update_schema = UpdateSchema(SchemaState {
                        sdl: schema,
                        launch_id: None,
                    });
                    stream::once(future::ready(update_schema))
                        .chain(stream::iter(vec![NoMoreSchema]))
                        .boxed()
                }
            }
            SchemaSource::Stream(stream) => stream
                .map(|sdl| {
                    UpdateSchema(SchemaState {
                        sdl,
                        launch_id: None,
                    })
                })
                .chain(stream::iter(vec![NoMoreSchema]))
                .boxed(),
            SchemaSource::File {
                path,
                watch,
            } => {
                // Sanity check, does the schema file exists, if it doesn't then bail.
                if !path.exists() {
                    tracing::error!(
                        "Supergraph schema at path '{}' does not exist.",
                        path.to_string_lossy()
                    );
                    stream::iter(vec![NoMoreSchema]).boxed()
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
                                                Ok(schema) => {
                                                    let update_schema = UpdateSchema(SchemaState {
                                                        sdl: schema,
                                                        launch_id: None,
                                                    });
                                                    Some(update_schema)
                                                }
                                                Err(err) => {
                                                    tracing::error!(reason = %err, "failed to read supergraph schema");
                                                    None
                                                }
                                            }
                                        }
                                    })
                                    .boxed()
                            } else {
                                let update_schema = UpdateSchema(SchemaState {
                                    sdl: schema,
                                    launch_id: None,
                                });
                                stream::once(future::ready(update_schema))
                                    .chain(stream::iter(vec![NoMoreSchema]))
                                    .boxed()
                            }
                        }
                        Err(err) => {
                            tracing::error!(reason = %err, "failed to read supergraph schema");
                            stream::iter(vec![NoMoreSchema]).boxed()
                        }
                    }
                }
            }
            SchemaSource::Registry {
                apollo_key,
                apollo_graph_ref,
                endpoints,
                poll_interval,
                timeout,
            } => {
                // Validate required fields and create UplinkConfig
                let uplink_config = match (apollo_key, apollo_graph_ref) {
                    (Some(key), Some(graph_ref)) => Some(UplinkConfig {
                        apollo_key: key,
                        apollo_graph_ref: graph_ref,
                        endpoints,
                        poll_interval,
                        timeout,
                    }),
                    (None, _) => {
                        tracing::error!("APOLLO_KEY is required for uplink schema source");
                        None
                    }
                    (_, None) => {
                        tracing::error!("APOLLO_GRAPH_REF is required for uplink schema source");
                        None
                    }
                };

                if let Some(config) = uplink_config {
                    stream_from_uplink::<SupergraphSdlQuery, SchemaState>(config)
                        .filter_map(|res| {
                            future::ready(match res {
                                Ok(schema) => {
                                    let update_schema = UpdateSchema(schema);
                                    Some(update_schema)
                                }
                                Err(e) => {
                                    tracing::error!("{}", e);
                                    None
                                }
                            })
                        })
                        .chain(stream::iter(vec![NoMoreSchema]))
                        .boxed()
                } else {
                    stream::iter(vec![NoMoreSchema]).boxed()
                }
            }
            SchemaSource::URLs { urls } => {
                futures::stream::once(async move {
                    fetch_supergraph_from_first_viable_url(&urls).await
                })
                .filter_map(|s| async move { s.map(Event::UpdateSchema) })
                .chain(stream::iter(vec![NoMoreSchema]))
                .boxed()
            }
            SchemaSource::OCI {
                apollo_key,
                reference,
            } => {
                // Validate OCI reference and create config
                let oci_config = match crate::registry::validate_oci_reference(&reference) {
                    Ok((validated_reference, _oci_reference_type)) => {
                        Some(crate::registry::OciConfig {
                            apollo_key,
                            reference: validated_reference,
                        })
                    }
                    Err(err) => {
                        tracing::error!("Invalid graph_artifact_reference: {}", err);
                        None
                    }
                };

                if let Some(config) = oci_config {
                    tracing::debug!("using oci as schema source");
                    futures::stream::once(async move {
                        match fetch_oci(config).await {
                            Ok(oci_result) => {
                                tracing::debug!("fetched schema from oci registry");
                                Some(SchemaState {
                                    sdl: oci_result.schema,
                                    launch_id: None,
                                })
                            }
                            Err(err) => {
                                tracing::error!("error fetching schema from oci registry {}", err);
                                None
                            }
                        }
                    })
                        .filter_map(|s| async move { s.map(Event::UpdateSchema) })
                        .chain(stream::iter(vec![NoMoreSchema]))
                        .boxed()
                } else {
                    stream::iter(vec![NoMoreSchema]).boxed()
                }
            }
        }
        // Note: NoMoreSchema is now chained per-variant above, not here
        // This allows empty Static schemas to not emit NoMoreSchema prematurely
        // when the config stream will provide the schema
        .boxed()
    }
}

// Encapsulates fetching the schema from the first viable url.
// It will try each url in order until it finds one that works.
async fn fetch_supergraph_from_first_viable_url(urls: &[Url]) -> Option<SchemaState> {
    let Ok(client) = reqwest::Client::builder()
        .no_gzip()
        .timeout(Duration::from_secs(10))
        .build()
    else {
        tracing::error!("failed to create HTTP client to fetch supergraph schema");
        return None;
    };
    for url in urls {
        match client
            .get(reqwest::Url::parse(url.as_ref()).unwrap())
            .send()
            .await
        {
            Ok(res) if res.status().is_success() => match res.text().await {
                Ok(schema) => {
                    return Some(SchemaState {
                        sdl: schema,
                        launch_id: None,
                    });
                }
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

#[cfg(test)]
mod tests {
    use std::env::temp_dir;

    use test_log::test;
    use tracing_futures::WithSubscriber;
    use wiremock::Mock;
    use wiremock::MockServer;
    use wiremock::ResponseTemplate;
    use wiremock::matchers::method;
    use wiremock::matchers::path;

    use super::*;
    use crate::assert_snapshot_subscriber;
    use crate::files::tests::create_temp_file;
    use crate::files::tests::write_and_flush;

    #[test(tokio::test)]
    async fn schema_by_file_watching() {
        let (path, mut file) = create_temp_file();
        let schema = include_str!("../../testdata/supergraph.graphql");
        write_and_flush(&mut file, schema).await;
        let mut stream = SchemaSource::File { path, watch: true }
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

        let mut stream = SchemaSource::File { path, watch: false }.into_stream();
        assert!(matches!(stream.next().await.unwrap(), UpdateSchema(_)));
        assert!(matches!(stream.next().await.unwrap(), NoMoreSchema));
    }

    #[test(tokio::test)]
    async fn schema_by_file_missing() {
        let mut stream = SchemaSource::File {
            path: temp_dir().join("does_not_exist"),
            watch: true,
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
            }
            .into_stream();

            assert!(
                matches!(stream.next().await.unwrap(), UpdateSchema(schema) if schema.sdl == SCHEMA_1)
            );
            assert!(matches!(stream.next().await.unwrap(), NoMoreSchema));
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
            }
            .into_stream();

            assert!(
                matches!(stream.next().await.unwrap(), UpdateSchema(schema) if schema.sdl == SCHEMA_2)
            );
            assert!(matches!(stream.next().await.unwrap(), NoMoreSchema));
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
            }
            .into_stream();

            assert!(matches!(stream.next().await.unwrap(), NoMoreSchema));
        }
        .with_subscriber(assert_snapshot_subscriber!({
            "[].fields[\"url.full\"]" => "[url.full]"
        }))
        .await;
    }
}
