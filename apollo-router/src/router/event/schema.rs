use std::path::PathBuf;
use std::pin::Pin;
use std::time::Duration;

use derivative::Derivative;
use derive_more::Display;
use derive_more::From;
use futures::prelude::*;

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
                        "Schema file at path '{}' does not exist.",
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
                                                    tracing::error!("{}", err);
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
                            tracing::error!("Failed to read schema: {}", err);
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
        }
        .chain(stream::iter(vec![NoMoreSchema]))
    }
}

#[cfg(test)]
mod tests {
    use std::env::temp_dir;

    use test_log::test;

    use super::*;
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
}
