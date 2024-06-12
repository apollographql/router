use std::path::PathBuf;
use std::pin::Pin;
use std::time::Duration;

use derivative::Derivative;
use derive_more::Display;
use derive_more::From;
use futures::prelude::*;

use crate::router::Event;

type PersistedQueriesStream = Pin<Box<dyn Stream<Item = String> + Send>>;

/// The user supplied persisted queries manifest. Either a static string or a stream for hot reloading.
#[derive(From, Display, Derivative)]
#[derivative(Debug)]
#[non_exhaustive]
pub enum PersistedQueriesSource {
    /// A static persisted queries manifest.
    #[display(fmt = "String")]
    Static { manifest: Option<String> },

    /// A stream of persisted queries manifests.
    #[display(fmt = "Stream")]
    Stream(#[derivative(Debug = "ignore")] PersistedQueriesStream),

    /// A file that may be watched for changes.
    #[display(fmt = "File")]
    File {
        /// The path of the persisted queries manifest file.
        path: PathBuf,

        /// `true` to watch the file for changes and hot apply them.
        watch: bool,

        /// When watching, the delay to wait before applying the new persisted queries manifest.
        /// Note: This variable is deprecated and has no effect.
        #[deprecated]
        delay: Option<Duration>,
    },
    /*/// Apollo managed federation.
    #[display(fmt = "Registry")]
    Registry(UplinkConfig),

    /// A list of URLs to fetch the persisted queries manifest from.
    #[display(fmt = "URLs")]
    URLs {
        /// The URLs to fetch the persisted queries manifest from.
        urls: Vec<Url>,
        /// `true` to watch the URLs for changes and hot apply them.
        watch: bool,
        /// When watching, the delay to wait between each poll.
        period: Duration,
    },*/
}

impl From<&'_ str> for PersistedQueriesSource {
    fn from(s: &'_ str) -> Self {
        Self::Static {
            manifest: Some(s.to_owned()),
        }
    }
}

impl Default for PersistedQueriesSource {
    fn default() -> Self {
        PersistedQueriesSource::Static { manifest: None }
    }
}

impl PersistedQueriesSource {
    /// Convert this schema into a stream regardless of if is static or not. Allows for unified handling later.
    pub(crate) fn into_stream(self) -> impl Stream<Item = Event> {
        match self {
            PersistedQueriesSource::Static { manifest } => {
                stream::once(future::ready(Event::UpdatePersistedQueriesManifest(manifest))).boxed()
            }
            PersistedQueriesSource::Stream(stream) => stream.map(|manifest| Event::UpdatePersistedQueriesManifest(Some(manifest))).boxed(),
            #[allow(deprecated)]
            PersistedQueriesSource::File {
                path,
                watch,
                delay: _,
            } => {
                // Sanity check, does the manifest file exists, if it doesn't then bail.
                if !path.exists() {
                    tracing::error!(
                        "Persisted queries manifest at path '{}' does not exist.",
                        path.to_string_lossy()
                    );
                    stream::once(future::ready(Event::UpdatePersistedQueriesManifest(None))).boxed()
                } else {
                    //The schema file exists try and load it
                    match std::fs::read_to_string(&path) {
                        Ok(manifest) => {
                            if watch {
                                crate::files::watch(&path)
                                    .filter_map(move |_| {
                                        let path = path.clone();
                                        async move {
                                            match tokio::fs::read_to_string(&path).await {
                                                Ok(manifest) => Some(Event::UpdatePersistedQueriesManifest(Some(manifest))),
                                                Err(err) => {
                                                    tracing::error!(reason = %err, "failed to read persisted queries manifest");
                                                    None
                                                }
                                            }
                                        }
                                    })
                                    .boxed()
                            } else {
                                stream::once(future::ready(Event::UpdatePersistedQueriesManifest(Some(manifest)))).boxed()
                            }
                        }
                        Err(err) => {
                            tracing::error!(reason = %err, "failed to read supergraph schema");
                            stream::empty().boxed()
                        }
                    }
                }
            }
            /*PersistedQueriesSource::Registry(uplink_config) => {
                stream_from_uplink::<PersistedQueriesManifestQuery, String>(uplink_config)
                    .filter_map(|res| {
                        future::ready(match res {
                            Ok(manifest) => Some(Event::UpdatePersistedQueriesManifest(Some(manifest))),
                            Err(e) => {
                                tracing::error!("{}", e);
                                None
                            }
                        })
                    })
                    .boxed()
            }*/
            /*PersistedQueriesSource::URLs {
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
            }*/
        }
        .chain(stream::iter(vec![Event::NoMorePersistedQueriesManifest]))
        .boxed()
    }
}

/*
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
*/
