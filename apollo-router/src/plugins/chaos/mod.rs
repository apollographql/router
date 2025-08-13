//! Chaos testing plugin for the Apollo Router.
//!
//! This plugin provides chaos testing capabilities to help reproduce bugs that require uncommon conditions.
//! You probably don't want this in production!

use std::time::Duration;

use futures::Stream;
use futures::StreamExt;
use futures::stream;
use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;

pub(crate) mod reload;

// Re-export reload functionality
pub(crate) use reload::ReloadState;

use crate::router::Event;

/// Configuration for chaos testing, trying to reproduce bugs that require uncommon conditions.
/// You probably don't want this in production!
///
/// ## How Chaos Reloading Works
///
/// The chaos system automatically captures and replays the last known schema and configuration
/// events to force hot reloads even when the underlying content hasn't actually changed. This
/// is particularly useful for memory leak detection during hot reload scenarios.
/// If configured, it will activate upon the first config event that is encountered.
///
/// ### Schema Reloading (`force_schema_reload`)
/// When enabled, the router will periodically replay the last schema event with a timestamp
/// comment injected into the SDL (e.g., `# Chaos reload timestamp: 1234567890`). This ensures
/// the schema is seen as "different" and triggers a full hot reload, even though the functional
/// schema content is identical.
///
/// ### Configuration Reloading (`force_config_reload`)
/// When enabled, the router will periodically replay the last configuration event. The
/// configuration is cloned and re-emitted, which triggers the router's configuration change
/// detection and reload logic.
///
/// ### Example Usage
/// ```yaml
/// experimental_chaos:
///   force_schema_reload: "30s"    # Trigger schema reload every 30 seconds
///   force_config_reload: "2m"     # Trigger config reload every 2 minutes
/// ```
#[derive(Debug, Clone, Default, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[serde(default)]
pub(crate) struct Config {
    /// Force a hot reload of the schema at regular intervals by injecting a timestamp comment
    /// into the SDL. This ensures schema reloads occur even when the functional schema content
    /// hasn't changed, which is useful for testing memory leaks during schema hot reloads.
    ///
    /// The system automatically captures the last schema event and replays it with a timestamp
    /// comment added to make it appear "different" to the reload detection logic.
    #[serde(with = "humantime_serde")]
    #[schemars(with = "Option<String>")]
    pub(crate) force_schema_reload: Option<Duration>,

    /// Force a hot reload of the configuration at regular intervals by replaying the last
    /// configuration event. This triggers the router's configuration change detection even
    /// when the configuration content hasn't actually changed.
    ///
    /// The system automatically captures the last configuration event and replays it to
    /// force configuration reload processing.
    #[serde(with = "humantime_serde")]
    #[schemars(with = "Option<String>")]
    pub(crate) force_config_reload: Option<Duration>,
}

impl Config {
    #[cfg(test)]
    fn new(force_schema_reload: Option<Duration>, force_config_reload: Option<Duration>) -> Self {
        Self {
            force_schema_reload,
            force_config_reload,
        }
    }
}

/// Extension trait to add chaos reload functionality to event streams.
pub(crate) trait ChaosEventStream: Stream<Item = Event> + Sized {
    /// Add chaos reload functionality to an event stream.
    ///
    /// This method wraps the event stream to automatically capture schema and configuration
    /// events for later replay during chaos testing. The reload source will emit modified
    /// versions of these events at configured intervals to force hot reloads.
    ///
    /// The chaos reload timers are automatically configured when configuration events
    /// flow through the stream.
    fn with_chaos_reload_state(self, reload_source: ReloadState) -> impl Stream<Item = Event> {
        let reload_source_for_events = reload_source.clone();
        let watched_upstream = self.map(move |event| {
            match &event {
                Event::UpdateSchema(schema_state) => {
                    reload_source_for_events.update_last_schema(schema_state);
                }
                Event::UpdateConfiguration(config) => {
                    // Update the reload source with the latest configuration
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
    /// Add chaos reload functionality to an event stream.
    ///
    /// This method wraps the event stream to automatically capture schema and configuration
    /// events for later replay during chaos testing. The reload source will emit modified
    /// versions of these events at configured intervals to force hot reloads.
    ///
    /// The chaos reload timers are automatically configured when configuration events
    /// flow through the stream - no manual setup is required.
    fn with_chaos_reload(self) -> impl Stream<Item = Event> {
        self.with_chaos_reload_state(ReloadState::default())
    }
}

impl<S> ChaosEventStream for S where S: Stream<Item = Event> {}
