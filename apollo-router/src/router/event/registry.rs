//! Schema and license events for the artifacts declared in the configuration's `registry`
//! section.
//!
//! Those sources are not fixed at startup like the CLI ones: they are read off every
//! `UpdateConfiguration` event, so an edit to the section while the router runs stops the
//! pollers it changes and starts new ones against the new references. A configuration edit that
//! leaves the section as it was keeps the running pollers and their cached state.

use std::sync::Arc;

use futures::StreamExt;
use futures::future;
use futures::stream;
use futures::stream::BoxStream;
use tokio::sync::mpsc;
use tokio::task::AbortHandle;
use tokio_stream::wrappers::ReceiverStream;
use tracing::instrument::WithSubscriber;

use super::Event;
use super::Event::NoMoreLicense;
use super::Event::NoMoreSchema;
use super::Event::UpdateConfiguration;
use super::LicenseSource;
use super::SchemaSource;
use crate::configuration::Configuration;
use crate::configuration::registry::ResolvedRegistry;

/// Merge the schema and license events for the configured registry artifacts into the
/// configuration stream. `schema` and `license` say which of the two this router takes from the
/// configuration; the other keeps whatever source the CLI chose.
pub(crate) fn drive_from_configuration(
    configuration: BoxStream<'static, Event>,
    schema: bool,
    license: bool,
) -> impl futures::Stream<Item = Event> {
    let (sender, receiver) = mpsc::channel::<Event>(8);
    let mut driver = Driver {
        schema,
        license,
        sender,
        current: None,
        schema_poller: None,
        license_poller: None,
    };
    let configuration = configuration.map(move |event| {
        if let UpdateConfiguration(configuration) = &event {
            driver.apply(configuration);
        }
        event
    });
    stream::select(configuration, ReceiverStream::new(receiver))
}

struct Driver {
    schema: bool,
    license: bool,
    sender: mpsc::Sender<Event>,
    /// What the running pollers were started from; `None` before the first configuration.
    current: Option<ResolvedRegistry>,
    schema_poller: Option<Poller>,
    license_poller: Option<Poller>,
}

impl Driver {
    fn apply(&mut self, configuration: &Arc<Configuration>) {
        let resolved = &configuration.resolved_registry;
        let first = self.current.is_none();
        let graph_changed = self.current.as_ref().map(|c| &c.graph) != Some(&resolved.graph);
        let entitlement_changed =
            self.current.as_ref().map(|c| &c.entitlement) != Some(&resolved.entitlement);

        if self.schema && graph_changed {
            match &resolved.graph {
                Some(oci_config) => {
                    if !first {
                        tracing::info!(
                            "registry.artifacts.graph changed; fetching the schema from {}",
                            oci_config.reference
                        );
                    }
                    self.schema_poller = Some(Poller::spawn(
                        SchemaSource::OCI(oci_config.clone())
                            .into_stream()
                            .filter(|event| future::ready(!matches!(event, NoMoreSchema)))
                            .boxed(),
                        self.sender.clone(),
                    ));
                }
                None if first => {
                    tracing::error!(
                        "the router has no schema source: set registry.artifacts.graph, APOLLO_GRAPH_ARTIFACT_REFERENCE, or --supergraph"
                    );
                    self.schema_poller = None;
                    self.send(NoMoreSchema);
                }
                None => {
                    tracing::error!(
                        "registry.artifacts.graph was removed; the router keeps serving the schema it has"
                    );
                }
            }
        }

        // The license follows the graph artifact unless an entitlement artifact is named.
        if self.license && (graph_changed || entitlement_changed) {
            let source = match (&resolved.entitlement, &resolved.graph) {
                (Some(entitlement), _) => Some(LicenseSource::OCIEntitlement(entitlement.clone())),
                (None, Some(graph)) => Some(LicenseSource::OCI(graph.clone())),
                (None, None) => None,
            };
            match source {
                Some(source) => {
                    self.license_poller = Some(Poller::spawn(
                        source
                            .into_stream()
                            .filter(|event| future::ready(!matches!(event, NoMoreLicense)))
                            .boxed(),
                        self.sender.clone(),
                    ));
                }
                None if first => {
                    self.license_poller = None;
                    self.send(NoMoreLicense);
                }
                None => {
                    tracing::error!(
                        "the registry section no longer names an artifact to take the license from; the router keeps its current license"
                    );
                }
            }
        }

        self.current = Some(resolved.clone());
    }

    fn send(&self, event: Event) {
        let sender = self.sender.clone();
        drop(tokio::task::spawn(
            async move {
                let _ = sender.send(event).await;
            }
            .with_current_subscriber(),
        ));
    }
}

/// A running poller task; dropping it stops the poller.
struct Poller(AbortHandle);

impl Poller {
    fn spawn(mut events: BoxStream<'static, Event>, sender: mpsc::Sender<Event>) -> Self {
        let handle = tokio::task::spawn(
            async move {
                while let Some(event) = events.next().await {
                    if sender.send(event).await.is_err() {
                        // The router is shutting down.
                        break;
                    }
                }
            }
            .with_current_subscriber(),
        );
        Poller(handle.abort_handle())
    }
}

impl Drop for Poller {
    fn drop(&mut self) {
        self.0.abort();
    }
}
