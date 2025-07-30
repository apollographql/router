//! Chaos reload source for schema and configuration events.
//!
//! This module provides the ReloadSource for chaos testing, which automatically captures
//! and replays schema/configuration events to force hot reloads at configurable intervals.

use std::sync::Arc;
use std::task::Poll;
use std::time::Duration;

use futures::prelude::*;
use parking_lot::Mutex;
use tokio_util::time::DelayQueue;

use super::Config;
use crate::configuration::Configuration;
use crate::router::Event;
use crate::uplink::schema::SchemaState;

#[derive(Clone)]
enum ChaosEvent {
    Schema,
    Configuration,
}

#[derive(Default)]
struct ReloadStateInner {
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
pub(crate) struct ReloadState {
    inner: Arc<Mutex<ReloadStateInner>>,
}

impl ReloadState {
    /// Configure chaos reload periods from the router configuration.
    /// This is called automatically when configuration events flow through the system.
    pub(crate) fn set_periods(&self, config: &Config) {
        let mut inner = self.inner.lock();
        // Clear the queue before setting the periods
        inner.queue.clear();

        inner.force_schema_reload_period = config.force_schema_reload;
        inner.force_config_reload_period = config.force_config_reload;

        if let Some(period) = config.force_schema_reload {
            inner.queue.insert(ChaosEvent::Schema, period);
        }
        if let Some(period) = config.force_config_reload {
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
        futures::stream::poll_fn(move |cx| {
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
        })
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use futures::StreamExt;
    use futures::pin_mut;

    use super::*;
    use crate::configuration::Configuration;
    use crate::plugins::chaos::ChaosEventStream;

    fn create_test_schema() -> SchemaState {
        SchemaState {
            sdl: "type Query { hello: String }".to_string(),
            launch_id: Some("test-launch".to_string()),
        }
    }

    fn create_test_config() -> Arc<Configuration> {
        Arc::new(Configuration::default())
    }

    #[test]
    fn test_reload_source_default() {
        let source = ReloadState::default();
        let inner = source.inner.lock();
        assert!(inner.force_schema_reload_period.is_none());
        assert!(inner.force_config_reload_period.is_none());
        assert!(inner.last_schema.is_none());
        assert!(inner.last_configuration.is_none());
    }

    #[tokio::test]
    async fn test_set_periods_configures_reload_intervals() {
        let source = ReloadState::default();
        let schema_period = Duration::from_secs(5);
        let config_period = Duration::from_secs(10);

        let chaos_config = Config::new(Some(schema_period), Some(config_period));
        source.set_periods(&chaos_config);

        let inner = source.inner.lock();
        assert_eq!(inner.force_schema_reload_period, Some(schema_period));
        assert_eq!(inner.force_config_reload_period, Some(config_period));
    }

    #[tokio::test]
    async fn test_set_periods_clears_existing_queue() {
        let source = ReloadState::default();

        // Set initial periods
        let chaos_config1 = Config::new(Some(Duration::from_secs(5)), None);
        source.set_periods(&chaos_config1);

        // Verify queue has an entry
        assert_eq!(source.inner.lock().queue.len(), 1);
        // Set different periods - should clear queue
        let chaos_config2 = Config::new(None, Some(Duration::from_secs(10)));
        source.set_periods(&chaos_config2);

        let inner = source.inner.lock();
        assert_eq!(inner.force_schema_reload_period, None);
        assert_eq!(
            inner.force_config_reload_period,
            Some(Duration::from_secs(10))
        );
        assert_eq!(inner.queue.len(), 1);
    }

    #[test]
    fn test_set_periods_with_none_values() {
        let source = ReloadState::default();
        let chaos_config = Config::new(None, None);

        source.set_periods(&chaos_config);

        let inner = source.inner.lock();
        assert!(inner.force_schema_reload_period.is_none());
        assert!(inner.force_config_reload_period.is_none());
        assert!(inner.queue.is_empty());
    }

    #[test]
    fn test_update_last_schema() {
        let source = ReloadState::default();
        let schema = create_test_schema();

        // Initially no schema stored
        {
            let inner = source.inner.lock();
            assert!(inner.last_schema.is_none());
        }

        // Update with schema
        source.update_last_schema(&schema);

        // Verify schema is stored
        let inner = source.inner.lock();
        let stored_schema = inner.last_schema.as_ref().unwrap();
        assert_eq!(stored_schema.sdl, schema.sdl);
        assert_eq!(stored_schema.launch_id, schema.launch_id);
    }

    #[test]
    fn test_update_last_configuration() {
        let source = ReloadState::default();
        let config = create_test_config();

        // Initially no configuration stored
        {
            let inner = source.inner.lock();
            assert!(inner.last_configuration.is_none());
        }

        // Update with configuration
        source.update_last_configuration(&config);

        // Verify configuration is stored
        let inner = source.inner.lock();
        let stored_config = inner.last_configuration.as_ref().unwrap();
        assert!(Arc::ptr_eq(stored_config, &config));
    }

    #[test]
    fn test_update_methods_replace_previous_values() {
        let source = ReloadState::default();

        // Set initial schema and config
        let schema1 = create_test_schema();
        let config1 = create_test_config();
        source.update_last_schema(&schema1);
        source.update_last_configuration(&config1);

        // Create new schema and config
        let mut schema2 = create_test_schema();
        schema2.sdl = "type Query { goodbye: String }".to_string();
        schema2.launch_id = Some("new-launch".to_string());
        let config2 = Arc::new(Configuration::default());

        // Update with new values
        source.update_last_schema(&schema2);
        source.update_last_configuration(&config2);

        // Verify new values are stored
        let inner = source.inner.lock();
        let stored_schema = inner.last_schema.as_ref().unwrap();
        assert_eq!(stored_schema.sdl, schema2.sdl);
        assert_eq!(stored_schema.launch_id, schema2.launch_id);

        let stored_config = inner.last_configuration.as_ref().unwrap();
        assert!(Arc::ptr_eq(stored_config, &config2));
        assert!(!Arc::ptr_eq(stored_config, &config1));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_stream_with_no_chaos_periods_is_empty() {
        let source = ReloadState::default();
        let mut stream = source.into_stream();

        // Stream should not produce events without periods configured
        let result = tokio::time::timeout(Duration::from_millis(50), stream.next()).await;
        assert!(result.is_err()); // Timeout expected
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_stream_skips_events_when_no_data_available() {
        let source = ReloadState::default();
        let chaos_config = Config::new(Some(Duration::from_millis(10)), None);
        source.set_periods(&chaos_config);

        let mut stream = source.into_stream();

        // Stream should not produce events since no schema is stored
        let result = tokio::time::timeout(Duration::from_millis(50), stream.next()).await;
        assert!(result.is_err()); // Timeout expected
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_schema_reload_stream_generates_events() {
        let source = ReloadState::default();
        let schema = create_test_schema();
        source.update_last_schema(&schema);

        let chaos_config = Config::new(Some(Duration::from_millis(10)), None);
        source.set_periods(&chaos_config);

        let mut stream = source.into_stream();

        // Should get a schema reload event
        let event = tokio::time::timeout(Duration::from_millis(100), stream.next())
            .await
            .expect("Should receive event within timeout")
            .expect("Stream should produce an event");

        match event {
            Event::UpdateSchema(reloaded_schema) => {
                // Should contain timestamp comment
                assert!(reloaded_schema.sdl.contains("# Chaos reload timestamp:"));
                assert!(reloaded_schema.sdl.contains(&schema.sdl));
                assert_eq!(reloaded_schema.launch_id, schema.launch_id);
            }
            _ => panic!("Expected UpdateSchema event, got {:?}", event),
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_config_reload_stream_generates_events() {
        let source = ReloadState::default();
        let config = create_test_config();
        source.update_last_configuration(&config);

        let chaos_config = Config::new(None, Some(Duration::from_millis(10)));
        source.set_periods(&chaos_config);

        let mut stream = source.into_stream();

        // Should get a config reload event
        let event = tokio::time::timeout(Duration::from_millis(100), stream.next())
            .await
            .expect("Should receive event within timeout")
            .expect("Stream should produce an event");

        match event {
            Event::UpdateConfiguration(reloaded_config) => {
                // Should be a clone of the original config (different Arc but same contents)
                assert!(!Arc::ptr_eq(&reloaded_config, &config));
            }
            _ => panic!("Expected UpdateConfiguration event, got {:?}", event),
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_reloadable_event_stream_captures_schema_events() {
        let reload_source = ReloadState::default();
        let schema = create_test_schema();
        let schema_event = Event::UpdateSchema(schema.clone());

        let upstream = futures::stream::once(async move { schema_event });
        let stream = upstream.with_chaos_reload_state(reload_source.clone());
        pin_mut!(stream);

        // Get the upstream event
        let event = stream.next().await.unwrap();
        match event {
            Event::UpdateSchema(received_schema) => {
                assert_eq!(received_schema.sdl, schema.sdl);
                assert_eq!(received_schema.launch_id, schema.launch_id);
            }
            _ => panic!("Expected UpdateSchema event"),
        }

        // Verify the schema was captured by reload source
        let inner = reload_source.inner.lock();
        let stored_schema = inner.last_schema.as_ref().unwrap();
        assert_eq!(stored_schema.sdl, schema.sdl);
        assert_eq!(stored_schema.launch_id, schema.launch_id);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_reloadable_event_stream_configures_and_captures_config_events() {
        let reload_source = ReloadState::default();
        let config = Configuration {
            experimental_chaos: Config::new(Some(Duration::from_secs(30)), None),
            ..Default::default()
        };
        let config_arc = Arc::new(config);
        let config_event = Event::UpdateConfiguration(config_arc.clone());

        let upstream = futures::stream::once(async move { config_event });
        let stream = upstream.with_chaos_reload_state(reload_source.clone());
        pin_mut!(stream);

        // Get the upstream event
        let event = stream.next().await.unwrap();
        match event {
            Event::UpdateConfiguration(received_config) => {
                assert!(Arc::ptr_eq(&received_config, &config_arc));
            }
            _ => panic!("Expected UpdateConfiguration event"),
        }

        // Verify the configuration was captured and periods were set
        let inner = reload_source.inner.lock();
        let stored_config = inner.last_configuration.as_ref().unwrap();
        assert!(Arc::ptr_eq(stored_config, &config_arc));
        assert_eq!(
            inner.force_schema_reload_period,
            Some(Duration::from_secs(30))
        );
        assert!(inner.force_config_reload_period.is_none());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_reloadable_event_stream_ignores_other_events() {
        let reload_source = ReloadState::default();
        let other_event = Event::NoMoreSchema;

        let upstream = futures::stream::once(async move { other_event });
        let stream = upstream.with_chaos_reload_state(reload_source.clone());
        pin_mut!(stream);

        // Get the upstream event
        let event = stream.next().await.unwrap();
        match event {
            Event::NoMoreSchema => {
                // Expected - this should pass through unchanged
            }
            _ => panic!("Expected NoMoreSchema event"),
        }

        // Verify no data was captured
        let inner = reload_source.inner.lock();
        assert!(inner.last_schema.is_none());
        assert!(inner.last_configuration.is_none());
        assert!(inner.force_schema_reload_period.is_none());
        assert!(inner.force_config_reload_period.is_none());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_reloadable_event_stream_merges_upstream_and_chaos_events() {
        let reload_source = ReloadState::default();
        let schema = create_test_schema();
        reload_source.update_last_schema(&schema);

        let chaos_config = Config::new(Some(Duration::from_millis(10)), None);
        reload_source.set_periods(&chaos_config);

        // Create upstream with a single event, then end
        let schema_event = Event::UpdateSchema(schema.clone());
        let upstream = futures::stream::once(async move { schema_event });

        let stream = upstream.with_chaos_reload_state(reload_source);
        pin_mut!(stream);

        // Should get the original upstream event first
        let first_event = stream.next().await.unwrap();
        match first_event {
            Event::UpdateSchema(received_schema) => {
                // This should be the original schema without timestamp
                assert_eq!(received_schema.sdl, schema.sdl);
                assert!(!received_schema.sdl.contains("# Chaos reload timestamp:"));
            }
            _ => panic!("Expected UpdateSchema event"),
        }

        // Should then get chaos reload events
        let second_event = tokio::time::timeout(Duration::from_millis(100), stream.next())
            .await
            .expect("Should receive chaos event within timeout")
            .expect("Stream should produce an event");

        match second_event {
            Event::UpdateSchema(chaos_schema) => {
                // This should contain the timestamp comment
                assert!(chaos_schema.sdl.contains("# Chaos reload timestamp:"));
                assert!(chaos_schema.sdl.contains(&schema.sdl));
            }
            _ => panic!("Expected UpdateSchema chaos event"),
        }
    }
}
