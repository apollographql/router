use std::path::Path;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;

use derivative::Derivative;
use derive_more::Display;
use derive_more::From;
use futures::prelude::*;

use crate::Configuration;
use crate::router::Event;
use crate::router::Event::NoMoreConfiguration;
use crate::router::Event::RhaiReload;
use crate::router::Event::UpdateConfiguration;
use crate::router::Event::UpdateSchema;
use crate::uplink::UplinkConfig;
use crate::uplink::schema::SchemaState;
use crate::registry::OciConfig;
use crate::registry::fetch_oci;

type ConfigurationStream = Pin<Box<dyn Stream<Item = Configuration> + Send>>;

/// The user supplied config. Either a static instance or a stream for hot reloading.
#[derive(From, Display, Derivative)]
#[derivative(Debug)]
#[non_exhaustive]
pub enum ConfigurationSource {
    /// A static configuration.
    ///
    /// Can be created through `serde::Deserialize` from various formats,
    /// or inline in Rust code with `serde_json::json!` and `serde_json::from_value`.
    #[display("Static")]
    #[from(Configuration, Box<Configuration>)]
    Static(Box<Configuration>),

    /// A configuration stream where the server will react to new configuration. If possible
    /// the configuration will be applied without restarting the internal http server.
    #[display("Stream")]
    Stream(#[derivative(Debug = "ignore")] ConfigurationStream),

    /// A yaml file that may be watched for changes
    #[display("File")]
    File {
        /// The path of the configuration file.
        path: PathBuf,

        /// `true` to watch the file for changes and hot apply them.
        watch: bool,
    },
}

impl Default for ConfigurationSource {
    fn default() -> Self {
        ConfigurationSource::Static(Default::default())
    }
}

impl ConfigurationSource {
    /// Convert this config into a stream regardless of if is static or not. Allows for unified handling later.
    /// 
    /// `schema_source_provided` indicates if a schema source was already provided via CLI/env.
    /// If false and config contains graph_artifact_reference, we'll emit schema events from config.
    pub(crate) fn into_stream(
        self,
        uplink_config: Option<UplinkConfig>,
        schema_source_provided: bool,
    ) -> impl Stream<Item = Event> {
        match self {
            ConfigurationSource::Static(mut instance) => {
                instance.uplink = uplink_config;
                let config_arc = Arc::new(*instance);
                let config_event = UpdateConfiguration(config_arc.clone());
                
                // If schema source wasn't provided and config has graph_artifact_reference, fetch schema
                if !schema_source_provided {
                    if let Some(schema_stream) = Self::maybe_create_schema_from_config(config_arc.clone()) {
                        // Chain config event with schema stream
                        return stream::once(future::ready(config_event))
                            .chain(schema_stream)
                            .boxed();
                    }
                }
                
                stream::once(future::ready(config_event)).boxed()
            }
            ConfigurationSource::Stream(stream) => stream
                .map(move |mut c| {
                    c.uplink = uplink_config.clone();
                    let config_arc = Arc::new(c);
                    // Check if we should also emit schema events
                    // Note: Stream case is more complex - schema events would need to be interleaved
                    // For now, just emit config events
                    UpdateConfiguration(config_arc)
                })
                .boxed(),
            ConfigurationSource::File { path, watch } => {
                // Sanity check, does the config file exists, if it doesn't then bail.
                if !path.exists() {
                    tracing::error!(
                        "configuration file at path '{}' does not exist.",
                        path.to_string_lossy()
                    );
                    stream::empty().boxed()
                } else {
                    match ConfigurationSource::read_config(&path) {
                        Ok(mut configuration) => {
                            configuration.uplink = uplink_config.clone();
                            let config_arc = Arc::new(configuration);
                            let config_event = UpdateConfiguration(config_arc.clone());
                            
                            if watch {
                                let schema_source_provided_for_watch = schema_source_provided;
                                let config_watcher = crate::files::watch(&path)
                                    .then(move |_| {
                                        let path = path.clone();
                                        let uplink_config = uplink_config.clone();
                                        let schema_source_provided = schema_source_provided_for_watch;
                                        async move {
                                            match ConfigurationSource::read_config_async(&path)
                                                .await
                                            {
                                                Ok(mut configuration) => {
                                                    configuration.uplink = uplink_config.clone();
                                                    let config_arc = Arc::new(configuration);
                                                    let config_event = UpdateConfiguration(config_arc.clone());
                                                    
                                                    // Check if config should provide schema source
                                                    let mut events = vec![config_event];
                                                    if !schema_source_provided {
                                                        if let Some(schema_stream) = Self::maybe_create_schema_from_config(config_arc.clone()) {
                                                            // Collect schema event from stream
                                                            let schema_events: Vec<Event> = schema_stream.collect().await;
                                                            events.extend(schema_events);
                                                        }
                                                    }
                                                    events
                                                }
                                                Err(err) => {
                                                    tracing::error!("{}", err);
                                                    vec![]
                                                }
                                            }
                                        }
                                    })
                                    .flat_map(|events| stream::iter(events))
                                    .boxed();
                                
                                // Combine initial config event with watch stream
                                // Also add schema stream if needed
                                let initial_config_stream = stream::once(future::ready(config_event));
                                let combined_stream = if !schema_source_provided {
                                    if let Some(schema_stream) = Self::maybe_create_schema_from_config(config_arc.clone()) {
                                        initial_config_stream
                                            .chain(schema_stream)
                                            .chain(config_watcher)
                                            .boxed()
                                    } else {
                                        initial_config_stream
                                            .chain(config_watcher)
                                            .boxed()
                                    }
                                } else {
                                    initial_config_stream
                                        .chain(config_watcher)
                                        .boxed()
                                };
                                
                                let config_arc_clone = config_arc.clone();
                                if let Some(rhai_plugin) =
                                    config_arc_clone.apollo_plugins.plugins.get("rhai")
                                {
                                    let scripts_path = match rhai_plugin["scripts"].as_str() {
                                        Some(path) => Path::new(path),
                                        None => Path::new("rhai"),
                                    };
                                    // If our path is relative, add it to the current dir
                                    let scripts_watch = if scripts_path.is_relative() {
                                        let current_directory = std::env::current_dir();
                                        if current_directory.is_err() {
                                            tracing::error!("No current directory found",);
                                            return stream::empty().boxed();
                                        }
                                        current_directory.unwrap().join(scripts_path)
                                    } else {
                                        scripts_path.into()
                                    };
                                    let rhai_watcher = crate::files::watch_rhai(&scripts_watch)
                                        .filter_map(move |_| future::ready(Some(RhaiReload)))
                                        .boxed();
                                    // Select across both our streams
                                    futures::stream::select(combined_stream.boxed(), rhai_watcher).boxed()
                                } else {
                                    // No rhai - just return combined stream
                                    combined_stream.boxed()
                                }
                            } else {
                                // Non-watching case: emit config event, then schema if needed
                                let initial_stream = stream::once(future::ready(config_event));
                                if !schema_source_provided {
                                    if let Some(schema_stream) = Self::maybe_create_schema_from_config(config_arc.clone()) {
                                        return initial_stream
                                            .chain(schema_stream)
                                            .boxed();
                                    }
                                }
                                initial_stream.boxed()
                            }
                        }
                        Err(err) => {
                            tracing::error!("Failed to read configuration: {}", err);
                            stream::empty().boxed()
                        }
                    }
                }
            }
        }
        .chain(stream::iter(vec![NoMoreConfiguration]))
        .boxed()
    }

    fn read_config(path: &Path) -> Result<Configuration, ReadConfigError> {
        let config = std::fs::read_to_string(path)?;
        config.parse().map_err(ReadConfigError::Validation)
    }
    async fn read_config_async(path: &Path) -> Result<Configuration, ReadConfigError> {
        let config = tokio::fs::read_to_string(path).await?;
        config.parse().map_err(ReadConfigError::Validation)
    }
    
    /// Create a schema event from configuration if graph_artifact_reference is set.
    /// Returns None if no schema should be created from config.
    /// 
    /// This creates an async stream that fetches the schema from OCI.
    fn maybe_create_schema_from_config(config: Arc<Configuration>) -> Option<impl Stream<Item = Event>> {
        // Check if graph_artifact_reference is set in config
        let graph_artifact_ref = config.graph_artifact_reference.clone()?;
        
        // Validate and determine reference type
        let (reference, oci_reference_type) = match crate::executable::Opt::validate_oci_reference(&graph_artifact_ref) {
            Ok(result) => result,
            Err(err) => {
                tracing::error!("Invalid graph_artifact_reference in config: {}", err);
                return None;
            }
        };
        
        // Get Apollo key - required for OCI
        let apollo_key = match std::env::var("APOLLO_KEY") {
            Ok(key) => key,
            Err(_) => {
                tracing::error!("APOLLO_KEY environment variable is required when using graph_artifact_reference from config");
                return None;
            }
        };
        
        // Create OciConfig
        let oci_config = OciConfig {
            apollo_key,
            reference,
            hot_reload: config.hot_reload,
            oci_reference_type,
        };
        
        // Fetch schema from OCI asynchronously
        Some(futures::stream::once(async move {
            match fetch_oci(oci_config).await {
                Ok(oci_result) => {
                    tracing::debug!("fetched schema from oci registry via config");
                    Some(UpdateSchema(SchemaState {
                        sdl: oci_result.schema,
                        launch_id: None,
                    }))
                }
                Err(err) => {
                    tracing::error!("error fetching schema from oci registry via config: {}", err);
                    None
                }
            }
        })
        .filter_map(|s| async move { s }))
    }
}

#[derive(From, Display)]
enum ReadConfigError {
    /// could not read configuration: {0}
    Io(std::io::Error),
    /// {0}
    Validation(crate::configuration::ConfigurationError),
}

#[cfg(test)]
mod tests {
    use std::env::temp_dir;

    use futures::StreamExt;

    use super::*;
    use crate::files::tests::create_temp_file;
    use crate::files::tests::write_and_flush;
    use crate::uplink::UplinkConfig;

    #[tokio::test(flavor = "multi_thread")]
    async fn config_by_file_watching() {
        let (path, mut file) = create_temp_file();
        let contents = include_str!("../../testdata/supergraph_config.router.yaml");
        write_and_flush(&mut file, contents).await;
        let mut stream = ConfigurationSource::File { path, watch: true }
            .into_stream(Some(UplinkConfig::default()))
            .boxed();

        // First update is guaranteed
        assert!(matches!(
            stream.next().await.unwrap(),
            UpdateConfiguration(_)
        ));

        // Need different contents, since we won't get an event if content is the same
        let contents_datadog = include_str!("../../testdata/datadog.router.yaml");
        // Modify the file and try again
        write_and_flush(&mut file, contents_datadog).await;
        assert!(matches!(
            stream.next().await.unwrap(),
            UpdateConfiguration(_)
        ));

        // This time write garbage, there should not be an update.
        write_and_flush(&mut file, ":garbage").await;
        let event = StreamExt::into_future(stream).now_or_never();
        assert!(event.is_none() || matches!(event, Some((Some(NoMoreConfiguration), _))));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn config_by_file_missing() {
        let mut stream = ConfigurationSource::File {
            path: temp_dir().join("does_not_exit"),
            watch: true,
        }
        .into_stream(Some(UplinkConfig::default()));

        // First update fails because the file is invalid.
        assert!(matches!(stream.next().await.unwrap(), NoMoreConfiguration));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn config_by_file_invalid() {
        let (path, mut file) = create_temp_file();
        write_and_flush(&mut file, "Garbage").await;
        let mut stream = ConfigurationSource::File { path, watch: true }
            .into_stream(Some(UplinkConfig::default()));

        // First update fails because the file is invalid.
        assert!(matches!(stream.next().await.unwrap(), NoMoreConfiguration));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn config_by_file_no_watch() {
        let (path, mut file) = create_temp_file();
        let contents = include_str!("../../testdata/supergraph_config.router.yaml");
        write_and_flush(&mut file, contents).await;

        let mut stream = ConfigurationSource::File { path, watch: false }
            .into_stream(Some(UplinkConfig::default()));
        assert!(matches!(
            stream.next().await.unwrap(),
            UpdateConfiguration(_)
        ));
        assert!(matches!(stream.next().await.unwrap(), NoMoreConfiguration));
    }
}
