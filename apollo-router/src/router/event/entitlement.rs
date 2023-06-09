// With regards to ELv2 licensing, this entire file is license key functionality

use std::path::PathBuf;
use std::pin::Pin;
use std::str::FromStr;
use std::time::Duration;

use derivative::Derivative;
use derive_more::Display;
use derive_more::From;
use futures::prelude::*;
use url::Url;

use crate::router::Event;
use crate::router::Event::NoMoreEntitlement;
use crate::uplink::entitlement::Entitlement;
use crate::uplink::entitlement_stream::EntitlementQuery;
use crate::uplink::entitlement_stream::EntitlementStreamExt;
use crate::uplink::stream_from_uplink;
use crate::uplink::Endpoints;

type EntitlementStream = Pin<Box<dyn Stream<Item = Entitlement> + Send>>;

/// Entitlement controls availability of certain features of the Router.
/// This API experimental and is subject to change outside of semver.
#[derive(From, Display, Derivative)]
#[derivative(Debug)]
#[non_exhaustive]
pub enum EntitlementSource {
    /// A static entitlement. EXPERIMENTAL and not subject to semver.
    #[display(fmt = "Static")]
    Static { entitlement: Entitlement },

    /// An entitlement supplied via APOLLO_ROUTER_ENTITLEMENT. EXPERIMENTAL and not subject to semver.
    #[display(fmt = "Env")]
    Env,

    /// A stream of entitlement. EXPERIMENTAL and not subject to semver.
    #[display(fmt = "Stream")]
    Stream(#[derivative(Debug = "ignore")] EntitlementStream),

    /// A raw file that may be watched for changes. EXPERIMENTAL and not subject to semver.
    #[display(fmt = "File")]
    File {
        /// The path of the entitlement file.
        path: PathBuf,

        /// `true` to watch the file for changes and hot apply them.
        watch: bool,
    },

    /// Apollo uplink.
    #[display(fmt = "Registry")]
    Registry {
        /// The Apollo key: `<YOUR_GRAPH_API_KEY>`
        apollo_key: String,

        /// The apollo graph reference: `<YOUR_GRAPH_ID>@<VARIANT>`
        apollo_graph_ref: String,

        /// The endpoint polled to fetch its latest supergraph schema.
        urls: Option<Vec<Url>>,

        /// The duration between polling
        poll_interval: Duration,

        /// The HTTP client timeout for each poll
        timeout: Duration,
    },
}

impl Default for EntitlementSource {
    fn default() -> Self {
        EntitlementSource::Static {
            entitlement: Default::default(),
        }
    }
}

impl EntitlementSource {
    /// Convert this entitlement into a stream regardless of if is static or not. Allows for unified handling later.
    pub(crate) fn into_stream(self) -> impl Stream<Item = Event> {
        match self {
            EntitlementSource::Static { entitlement } => {
                stream::once(future::ready(entitlement)).boxed()
            }
            EntitlementSource::Stream(stream) => stream.boxed(),
            EntitlementSource::File { path, watch } => {
                // Sanity check, does the schema file exists, if it doesn't then bail.
                if !path.exists() {
                    tracing::error!(
                        "Entitlement file at path '{}' does not exist.",
                        path.to_string_lossy()
                    );
                    stream::empty().boxed()
                } else {
                    // The entitlement file exists try and load it
                    match std::fs::read_to_string(&path).map(|e| e.parse()) {
                        Ok(Ok(entitlement)) => {
                            if watch {
                                crate::files::watch(&path)
                                    .filter_map(move |_| {
                                        let path = path.clone();
                                        async move {
                                            let result = tokio::fs::read_to_string(&path).await;
                                            if let Err(e) = &result {
                                                tracing::error!(
                                                    "failed to read entitlement file, {}",
                                                    e
                                                );
                                            }
                                            result.ok()
                                        }
                                    })
                                    .filter_map(|e| async move {
                                        let result = e.parse();
                                        if let Err(e) = &result {
                                            tracing::error!(
                                                "failed to parse entitlement file, {}",
                                                e
                                            );
                                        }
                                        result.ok()
                                    })
                                    .boxed()
                            } else {
                                stream::once(future::ready(entitlement)).boxed()
                            }
                        }
                        Ok(Err(err)) => {
                            tracing::error!("Failed to parse entitlement: {}", err);
                            stream::empty().boxed()
                        }
                        Err(err) => {
                            tracing::error!("Failed to read entitlement: {}", err);
                            stream::empty().boxed()
                        }
                    }
                }
            }
            EntitlementSource::Registry {
                apollo_key,
                apollo_graph_ref,
                urls,
                poll_interval,
                timeout,
            } => stream_from_uplink::<EntitlementQuery, Entitlement>(
                apollo_key,
                apollo_graph_ref,
                urls.map(Endpoints::fallback),
                poll_interval,
                timeout,
            )
            .filter_map(|res| {
                future::ready(match res {
                    Ok(entitlement) => Some(entitlement),
                    Err(e) => {
                        tracing::error!("{}", e);
                        None
                    }
                })
            })
            .boxed(),
            EntitlementSource::Env => {
                // EXPERIMENTAL and not subject to semver.
                match std::env::var("APOLLO_ROUTER_ENTITLEMENT").map(|e| Entitlement::from_str(&e))
                {
                    Ok(Ok(entitlement)) => stream::once(future::ready(entitlement)).boxed(),
                    Ok(Err(err)) => {
                        tracing::error!("Failed to parse entitlement: {}", err);
                        stream::empty().boxed()
                    }
                    Err(_) => stream::once(future::ready(Entitlement::default())).boxed(),
                }
            }
        }
        .expand_entitlements()
        .chain(stream::iter(vec![NoMoreEntitlement]))
    }
}
