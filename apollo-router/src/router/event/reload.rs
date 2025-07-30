use std::sync::Arc;
use std::task::Poll;
use std::time::Duration;

use futures::prelude::*;
use parking_lot::Mutex;
use tokio_util::time::DelayQueue;

use crate::configuration::Configuration;
use crate::router::Event;
use crate::uplink::schema::SchemaState;

#[derive(Clone)]
enum ChaosEvent {
    Schema,
    Configuration,
}

#[derive(Default)]
struct ReloadSourceInner {
    queue: DelayQueue<ChaosEvent>,
    force_schema_reload_period: Option<Duration>,
    force_config_reload_period: Option<Duration>,
    last_schema: Option<SchemaState>,
    last_configuration: Option<Arc<Configuration>>,
}

/// Chaos testing event source that automatically captures and replays schema/configuration events
/// to force hot reloads at configurable intervals.
///
/// This is used for memory leak detection during hot reload scenarios. The ReloadSource:
/// 1. Automatically captures the last schema and configuration events as they flow through the system
/// 2. Replays these events at configured intervals with modifications to ensure they're seen as "different"
/// 3. For schema events: injects a timestamp comment into the SDL
/// 4. For configuration events: clones and re-emits the configuration
///
/// The ReloadSource requires no manual setup - it automatically configures itself when the first
/// configuration event (containing chaos settings) flows through the event stream.
#[derive(Clone, Default)]
pub(crate) struct ReloadSource {
    inner: Arc<Mutex<ReloadSourceInner>>,
}

impl ReloadSource {
    /// Configure chaos reload periods from the router configuration.
    /// This is called automatically when configuration events flow through the system.
    pub(crate) fn set_periods(&self, chaos_config: &crate::configuration::Chaos) {
        let mut inner = self.inner.lock();
        // Clear the queue before setting the periods
        inner.queue.clear();

        inner.force_schema_reload_period = chaos_config.force_schema_reload;
        inner.force_config_reload_period = chaos_config.force_config_reload;

        if let Some(period) = chaos_config.force_schema_reload {
            inner.queue.insert(ChaosEvent::Schema, period);
        }
        if let Some(period) = chaos_config.force_config_reload {
            inner.queue.insert(ChaosEvent::Configuration, period);
        }
    }

    /// Store the most recent schema event for later replay during chaos testing.
    /// This is called automatically when schema events flow through the system.
    pub(crate) fn update_last_schema(&self, schema: &SchemaState) {
        let mut inner = self.inner.lock();
        inner.last_schema = Some(schema.clone());
    }

    /// Store the most recent configuration event for later replay during chaos testing.
    /// This is called automatically when configuration events flow through the system.
    pub(crate) fn update_last_configuration(&self, config: &Arc<Configuration>) {
        let mut inner = self.inner.lock();
        inner.last_configuration = Some(config.clone());
    }

    pub(crate) fn into_stream(self) -> impl Stream<Item = Event> {
        #[cfg(unix)]
        let signal_stream = {
            let mut signal = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::hangup())
                .expect("Failed to install SIGHUP signal handler");

            futures::stream::poll_fn(move |cx| match signal.poll_recv(cx) {
                Poll::Ready(Some(_)) => {
                    // SIGHUP received - for now we skip this since we don't have a generic reload event
                    // In the future, this could trigger a forced reload of the last known events
                    Poll::Pending
                }
                Poll::Ready(None) => Poll::Ready(None),
                Poll::Pending => Poll::Pending,
            })
            .boxed()
        };
        #[cfg(not(unix))]
        let signal_stream = futures::stream::empty().boxed();

        let periodic_reload = futures::stream::poll_fn(move |cx| {
            let mut inner = self.inner.lock();
            match inner.queue.poll_expired(cx) {
                Poll::Ready(Some(expired)) => {
                    let event_type = expired.into_inner();

                    // Re-schedule the event
                    match &event_type {
                        ChaosEvent::Schema => {
                            if let Some(period) = inner.force_schema_reload_period {
                                inner.queue.insert(ChaosEvent::Schema, period);
                            }
                        }
                        ChaosEvent::Configuration => {
                            if let Some(period) = inner.force_config_reload_period {
                                inner.queue.insert(ChaosEvent::Configuration, period);
                            }
                        }
                    }

                    // Generate the appropriate event
                    let event = match event_type {
                        ChaosEvent::Schema => {
                            if let Some(mut schema) = inner.last_schema.clone() {
                                // Inject a timestamp comment into the schema SDL to make it appear "different"
                                // This ensures the router's change detection will trigger a hot reload even
                                // though the functional schema content is identical
                                let timestamp = std::time::SystemTime::now()
                                    .duration_since(std::time::UNIX_EPOCH)
                                    .unwrap_or_default()
                                    .as_secs();
                                schema.sdl = format!(
                                    "# Chaos reload timestamp: {}\n{}",
                                    timestamp, schema.sdl
                                );
                                Some(Event::UpdateSchema(schema))
                            } else {
                                // No schema available yet - skip this chaos event
                                None
                            }
                        }
                        ChaosEvent::Configuration => {
                            if let Some(config) = inner.last_configuration.clone() {
                                // Clone and re-emit the configuration to trigger reload processing
                                // The router's change detection will process this as a new configuration event
                                // even though the content is identical, forcing configuration reload logic
                                let config_clone = (*config).clone();
                                Some(Event::UpdateConfiguration(Arc::new(config_clone)))
                            } else {
                                // No configuration available yet - skip this chaos event
                                None
                            }
                        }
                    };

                    match event {
                        Some(event) => Poll::Ready(Some(event)),
                        None => Poll::Pending, // Skip this event and wait for the next one
                    }
                }
                // We must return pending even if the queue is empty, otherwise the stream will never be polled again
                // The waker will still be used, so this won't end up in a hot loop.
                Poll::Ready(None) => Poll::Pending,
                Poll::Pending => Poll::Pending,
            }
        });

        futures::stream::select(signal_stream, periodic_reload)
    }
}

/// Extension trait to add chaos reload functionality to event streams.
///
/// This trait provides the `.with_reload()` method that automatically:
/// - Captures schema and configuration events as they flow through
/// - Configures the ReloadSource with chaos settings from configuration events
/// - Merges the upstream events with periodic chaos reload events
pub(crate) trait ReloadableEventStream: Stream<Item = Event> + Sized {
    /// Add chaos reload functionality to an event stream.
    ///
    /// This method wraps the event stream to automatically capture schema and configuration
    /// events for later replay during chaos testing. The reload source will emit modified
    /// versions of these events at configured intervals to force hot reloads.
    ///
    /// The chaos reload timers are automatically configured when configuration events
    /// flow through the stream - no manual setup is required.
    fn with_reload(self, reload_source: ReloadSource) -> impl Stream<Item = Event> {
        let reload_source_for_events = reload_source.clone();
        let watched_upstream = self.map(move |event| {
            match &event {
                Event::UpdateSchema(schema_state) => {
                    reload_source_for_events.update_last_schema(schema_state);
                }
                Event::UpdateConfiguration(config) => {
                    reload_source_for_events.set_periods(&config.experimental_chaos);
                    reload_source_for_events.update_last_configuration(config);
                }
                _ => {}
            }
            event
        });

        // Combine upstream events with reload source events
        stream::select(watched_upstream, reload_source.into_stream())
    }
}

impl<S> ReloadableEventStream for S where S: Stream<Item = Event> {}
