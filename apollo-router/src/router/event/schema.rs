use std::path::PathBuf;
use std::pin::Pin;
use std::time::Duration;

use derivative::Derivative;
use derive_more::Display;
use derive_more::From;
use futures::prelude::*;
use url::Url;

use crate::registry::OciConfig;
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
    #[display("Registry")]
    Registry(UplinkConfig),

    /// A list of URLs to fetch the schema from.
    #[display("URLs")]
    URLs {
        /// The URLs to fetch the schema from.
        urls: Vec<Url>,
    },

    #[display("Registry")]
    OCI {
        config: OciConfig,
        is_external_registry: bool,
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
                let update_schema = UpdateSchema(SchemaState {
                    sdl: schema,
                    launch_id: None,
                    is_external_registry: false,
                });
                stream::once(future::ready(update_schema)).boxed()
            }
            SchemaSource::Stream(stream) => stream
                .map(|sdl| {
                    UpdateSchema(SchemaState {
                        sdl,
                        launch_id: None,
                        is_external_registry: false,
                    })
                })
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
                                                Ok(schema) => {
                                                    let update_schema = UpdateSchema(SchemaState {
                                                        sdl: schema,
                                                        launch_id: None,
                                                        is_external_registry: false,
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
                                    is_external_registry: false,
                                });
                                stream::once(future::ready(update_schema)).boxed()
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
                stream_from_uplink::<SupergraphSdlQuery, SchemaState>(uplink_config)
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
                    .boxed()
            }
            SchemaSource::URLs { urls } => {
                futures::stream::once(async move {
                    fetch_supergraph_from_first_viable_url(&urls).await
                })
                .filter_map(|s| async move { s.map(Event::UpdateSchema) })
                .boxed()
            }
            SchemaSource::OCI { config: oci_config, is_external_registry } => {
                tracing::debug!("using oci as schema source");
                futures::stream::once(async move {
                    match fetch_oci(oci_config).await {
                        Ok(oci_result) => {
                            tracing::debug!("fetched schema from oci registry");
                            Some(SchemaState {
                                sdl: oci_result.schema,
                                launch_id: None,
                                is_external_registry,
                            })
                        }
                        Err(err) => {
                            tracing::error!("error fetching schema from oci registry {}", err);
                            None
                        }
                    }
                })
                    .filter_map(|s| async move { s.map(Event::UpdateSchema) })
                    .boxed()
            }
        }
        .chain(stream::iter(vec![NoMoreSchema]))
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
                        is_external_registry: false,
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
