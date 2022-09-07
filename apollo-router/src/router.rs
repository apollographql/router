#![allow(missing_docs)] // FIXME

use std::fs;
use std::path::Path;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;
use std::task::Context;
use std::task::Poll;
use std::time::Duration;

use derivative::Derivative;
use derive_more::Display;
use derive_more::From;
use displaydoc::Display as DisplayDoc;
use futures::channel::oneshot;
use futures::prelude::*;
use futures::FutureExt;
use thiserror::Error;
use tokio::sync::RwLock;
use tokio::task::spawn;
use tower::BoxError;
use tracing_futures::WithSubscriber;
use url::Url;
use Event::NoMoreConfiguration;
use Event::NoMoreSchema;
use Event::Shutdown;
use Event::UpdateConfiguration;
use Event::UpdateSchema;

use crate::axum_http_server_factory::AxumHttpServerFactory;
use crate::configuration::validate_configuration;
use crate::configuration::Configuration;
use crate::configuration::ListenAddr;
use crate::router_factory::YamlSupergraphServiceFactory;
use crate::state_machine::StateMachine;

type SchemaStream = Pin<Box<dyn Stream<Item = String> + Send>>;

/// Error types for FederatedServer.
#[derive(Error, Debug, DisplayDoc)]
pub enum ApolloRouterError {
    /// failed to start server
    StartupError,

    /// failed to stop HTTP Server
    HttpServerLifecycleError,

    /// no valid configuration was supplied
    NoConfiguration,

    /// no valid schema was supplied
    NoSchema,

    /// could not create the HTTP pipeline: {0}
    ServiceCreationError(BoxError),

    /// could not create the HTTP server: {0}
    ServerCreationError(std::io::Error),
}

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
        delay: Option<Duration>,
    },

    /// Apollo managed federation.
    #[display(fmt = "Registry")]
    Registry {
        /// The Apollo key: <YOUR_GRAPH_API_KEY>
        apollo_key: String,

        /// The apollo graph reference: <YOUR_GRAPH_ID>@<VARIANT>
        apollo_graph_ref: String,

        /// The endpoint polled to fetch its latest supergraph schema.
        urls: Option<Vec<Url>>,

        /// The duration between polling
        poll_interval: Duration,
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
    fn into_stream(self) -> impl Stream<Item = Event> {
        match self {
            SchemaSource::Static { schema_sdl: schema } => {
                stream::once(future::ready(UpdateSchema(schema))).boxed()
            }
            SchemaSource::Stream(stream) => stream.map(UpdateSchema).boxed(),
            SchemaSource::File { path, watch, delay } => {
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
                                crate::files::watch(path.to_owned(), delay)
                                    .filter_map(move |_| {
                                        future::ready(std::fs::read_to_string(&path).ok())
                                    })
                                    .map(UpdateSchema)
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
            SchemaSource::Registry {
                apollo_key,
                apollo_graph_ref,
                urls,
                poll_interval,
            } => {
                // With regards to ELv2 licensing, the code inside this block
                // is license key functionality
                apollo_uplink::stream_supergraph(apollo_key, apollo_graph_ref, urls, poll_interval)
                    .filter_map(|res| {
                        future::ready(match res {
                            Ok(schema_result) => Some(UpdateSchema(schema_result.schema)),
                            Err(e) => {
                                tracing::error!(
                                    "error downloading the schema from Uplink: {:?}",
                                    e
                                );
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
    #[display(fmt = "Static")]
    #[from(types(Configuration))]
    Static(Box<Configuration>),

    /// A configuration stream where the server will react to new configuration. If possible
    /// the configuration will be applied without restarting the internal http server.
    #[display(fmt = "Stream")]
    Stream(#[derivative(Debug = "ignore")] ConfigurationStream),

    /// A yaml file that may be watched for changes
    #[display(fmt = "File")]
    File {
        /// The path of the configuration file.
        path: PathBuf,

        /// `true` to watch the file for changes and hot apply them.
        watch: bool,

        /// When watching, the delay to wait before applying the new configuration.
        delay: Option<Duration>,
    },
}

impl Default for ConfigurationSource {
    fn default() -> Self {
        ConfigurationSource::Static(Default::default())
    }
}

impl ConfigurationSource {
    /// Convert this config into a stream regardless of if is static or not. Allows for unified handling later.
    fn into_stream(self) -> impl Stream<Item = Event> {
        match self {
            ConfigurationSource::Static(instance) => {
                stream::iter(vec![UpdateConfiguration(instance)]).boxed()
            }
            ConfigurationSource::Stream(stream) => {
                stream.map(|x| UpdateConfiguration(Box::new(x))).boxed()
            }
            ConfigurationSource::File { path, watch, delay } => {
                // Sanity check, does the config file exists, if it doesn't then bail.
                if !path.exists() {
                    tracing::error!(
                        "configuration file at path '{}' does not exist.",
                        path.to_string_lossy()
                    );
                    stream::empty().boxed()
                } else {
                    match ConfigurationSource::read_config(&path) {
                        Ok(configuration) => {
                            if watch {
                                crate::files::watch(path.to_owned(), delay)
                                    .filter_map(move |_| {
                                        future::ready(
                                            match ConfigurationSource::read_config(&path) {
                                                Ok(config) => Some(config),
                                                Err(err) => {
                                                    tracing::error!("{}", err);
                                                    None
                                                }
                                            },
                                        )
                                    })
                                    .map(|x| UpdateConfiguration(Box::new(x)))
                                    .boxed()
                            } else {
                                stream::once(future::ready(UpdateConfiguration(Box::new(
                                    configuration,
                                ))))
                                .boxed()
                            }
                        }
                        Err(err) => {
                            tracing::error!("{}", err);
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
        let config = fs::read_to_string(path)?;
        let config = validate_configuration(&config)?;

        Ok(config)
    }
}

#[derive(From, Display)]
enum ReadConfigError {
    /// could not read configuration: {0}
    Io(std::io::Error),
    /// {0}
    Validation(crate::configuration::ConfigurationError),
}

type ShutdownFuture = Pin<Box<dyn Future<Output = ()> + Send>>;

/// Specifies when the Router’s HTTP server should gracefully shutdown
#[derive(Display, Derivative)]
#[derivative(Debug)]
#[non_exhaustive]
pub enum ShutdownSource {
    /// No graceful shutdown
    #[display(fmt = "None")]
    None,

    /// A custom shutdown future.
    #[display(fmt = "Custom")]
    Custom(#[derivative(Debug = "ignore")] ShutdownFuture),

    /// Watch for Ctl-C signal.
    #[display(fmt = "CtrlC")]
    CtrlC,
}

impl ShutdownSource {
    /// Convert this shutdown hook into a future. Allows for unified handling later.
    fn into_stream(self) -> impl Stream<Item = Event> {
        match self {
            ShutdownSource::None => stream::pending::<Event>().boxed(),
            ShutdownSource::Custom(future) => future.map(|_| Shutdown).into_stream().boxed(),
            ShutdownSource::CtrlC => {
                #[cfg(not(unix))]
                {
                    async {
                        tokio::signal::ctrl_c()
                            .await
                            .expect("Failed to install CTRL+C signal handler");
                    }
                    .map(|_| Shutdown)
                    .into_stream()
                    .boxed()
                }

                #[cfg(unix)]
                future::select(
                    tokio::signal::ctrl_c().map(|s| s.ok()).boxed(),
                    async {
                        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                            .expect("Failed to install SIGTERM signal handler")
                            .recv()
                            .await
                    }
                    .boxed(),
                )
                .map(|_| Shutdown)
                .into_stream()
                .boxed()
            }
        }
    }
}

/// The entry point for running the Router’s HTTP server.
///
/// # Examples
///
/// ```
/// use apollo_router::RouterHttpServer;
/// use apollo_router::Configuration;
///
/// async {
///     let configuration = serde_yaml::from_str::<Configuration>("Config").unwrap();
///     let schema = "schema";
///     RouterHttpServer::builder()
///             .configuration(configuration)
///             .schema(schema)
///             .start()
///             .await;
/// };
/// ```
///
/// Shutdown via handle.
/// ```
/// use apollo_router::RouterHttpServer;
/// use apollo_router::Configuration;
///
/// async {
///     let configuration = serde_yaml::from_str::<Configuration>("Config").unwrap();
///     let schema = "schema";
///     let mut server = RouterHttpServer::builder()
///             .configuration(configuration)
///             .schema(schema)
///             .start();
///     // …
///     server.shutdown().await
/// };
/// ```
///
pub struct RouterHttpServer {
    result: Pin<Box<dyn Future<Output = Result<(), ApolloRouterError>> + Send>>,
    graphql_listen_address: Arc<RwLock<Option<ListenAddr>>>,
    extra_listen_adresses: Arc<RwLock<Vec<ListenAddr>>>,
    shutdown_sender: Option<oneshot::Sender<()>>,
}

#[buildstructor::buildstructor]
impl RouterHttpServer {
    /// Returns a builder to start an HTTP server in a separate Tokio task.
    ///
    /// Builder methods:
    ///
    /// * `.schema(impl Into<`[`SchemaSource`]`>)`
    ///   Required.
    ///   Specifies where to find the supergraph schema definition.
    ///   Some sources support hot-reloading.
    ///
    /// * `.configuration(impl Into<`[`ConfigurationSource`]`>)`
    ///   Optional.
    ///   Specifies where to find the router configuration.
    ///   If not provided, the default configuration as with an empty YAML file.
    ///
    /// * `.shutdown(impl Into<`[`ShutdownSource`]`>)`
    ///   Optional.
    ///   Specifies when the server should gracefully shut down.
    ///   If not provided, the default is [`ShutdownSource::CtrlC`].
    ///
    /// * `.start()`
    ///   Finishes the builder,
    ///   starts an HTTP server in a separate Tokio task,
    ///   and returns a `RouterHttpServer` handle.
    ///
    /// The server handle can be used in multiple ways.
    /// As a [`Future`], it resolves to `Result<(), `[`ApolloRouterError`]`>`
    /// either when the server has finished gracefully shutting down
    /// or when it encounters a fatal error that prevents it from starting.
    ///
    /// If the handle is dropped before being awaited as a future,
    /// a graceful shutdown is triggered.
    /// In order to wait until shutdown finishes,
    /// use the [`shutdown`][Self::shutdown] method instead.
    #[builder(visibility = "pub", entry = "builder", exit = "start")]
    fn start(
        schema: SchemaSource,
        configuration: Option<ConfigurationSource>,
        shutdown: Option<ShutdownSource>,
    ) -> RouterHttpServer {
        let (shutdown_sender, shutdown_receiver) = oneshot::channel::<()>();
        let event_stream = generate_event_stream(
            shutdown.unwrap_or(ShutdownSource::CtrlC),
            configuration.unwrap_or_default(),
            schema,
            shutdown_receiver,
        );
        let server_factory = AxumHttpServerFactory::new();
        let router_factory = YamlSupergraphServiceFactory::default();
        let state_machine = StateMachine::new(server_factory, router_factory);
        let extra_listen_adresses = state_machine.extra_listen_adresses.clone();
        let graphql_listen_address = state_machine.graphql_listen_address.clone();
        let result = spawn(
            async move { state_machine.process_events(event_stream).await }
                .with_current_subscriber(),
        )
        .map(|r| match r {
            Ok(Ok(ok)) => Ok(ok),
            Ok(Err(err)) => Err(err),
            Err(err) => {
                tracing::error!("{}", err);
                Err(ApolloRouterError::StartupError)
            }
        })
        .with_current_subscriber()
        .boxed();

        RouterHttpServer {
            result,
            shutdown_sender: Some(shutdown_sender),
            graphql_listen_address,
            extra_listen_adresses,
        }
    }

    /// Returns the listen address when the router is ready to receive GraphQL requests.
    ///
    /// This can be useful when the `server.listen` configuration specifies TCP port 0,
    /// which instructs the operating system to pick an available port number.
    ///
    /// Note: if configuration is dynamic, the listen address can change over time.
    pub async fn listen_address(&self) -> Option<ListenAddr> {
        self.graphql_listen_address.read().await.clone()
    }

    /// Returns the extra listen addresses the router can receive requests to.
    ///
    /// Combine it with `listen_address` to have an exhaustive list
    /// of all addresses used by the router.
    /// Note: if configuration is dynamic, the listen address can change over time.
    pub async fn extra_listen_adresses(&self) -> Vec<ListenAddr> {
        self.extra_listen_adresses.read().await.clone()
    }

    /// Trigger and wait for graceful shutdown
    pub async fn shutdown(&mut self) -> Result<(), ApolloRouterError> {
        if let Some(sender) = self.shutdown_sender.take() {
            let _ = sender.send(());
        }
        (&mut self.result).await
    }
}

/// Messages that are broadcast across the app.
#[derive(Debug)]
pub(crate) enum Event {
    /// The configuration was updated.
    UpdateConfiguration(Box<Configuration>),

    /// There are no more updates to the configuration
    NoMoreConfiguration,

    /// The schema was updated.
    UpdateSchema(String),

    /// There are no more updates to the schema
    NoMoreSchema,

    /// The server should gracefully shutdown.
    Shutdown,
}

impl Drop for RouterHttpServer {
    fn drop(&mut self) {
        if let Some(sender) = self.shutdown_sender.take() {
            let _ = sender.send(());
        }
    }
}

impl Future for RouterHttpServer {
    type Output = Result<(), ApolloRouterError>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        self.result.poll_unpin(cx)
    }
}

/// Create the unified event stream.
/// This merges all contributing streams and sets up shutdown handling.
/// When a shutdown message is received no more events are emitted.
fn generate_event_stream(
    shutdown: ShutdownSource,
    configuration: ConfigurationSource,
    schema: SchemaSource,
    shutdown_receiver: oneshot::Receiver<()>,
) -> impl Stream<Item = Event> {
    // Chain is required so that the final shutdown message is sent.
    let messages = stream::select_all(vec![
        shutdown.into_stream().boxed(),
        configuration.into_stream().boxed(),
        schema.into_stream().boxed(),
        shutdown_receiver.into_stream().map(|_| Shutdown).boxed(),
    ])
    .take_while(|msg| future::ready(!matches!(msg, Shutdown)))
    .chain(stream::iter(vec![Shutdown]))
    .boxed();
    messages
}

#[cfg(test)]
mod tests {
    use std::env::temp_dir;

    use serde_json::to_string_pretty;
    use test_log::test;

    use super::*;
    use crate::files::tests::create_temp_file;
    use crate::files::tests::write_and_flush;
    use crate::graphql;
    use crate::graphql::Request;

    fn init_with_server() -> RouterHttpServer {
        let configuration =
            serde_yaml::from_str::<Configuration>(include_str!("testdata/supergraph_config.yaml"))
                .unwrap();
        let schema = include_str!("testdata/supergraph.graphql");
        RouterHttpServer::builder()
            .configuration(configuration)
            .schema(schema)
            .start()
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn basic_request() {
        let mut router_handle = init_with_server();
        let listen_address = router_handle
            .listen_address()
            .await
            .expect("router failed to start");

        assert_federated_response(&listen_address, r#"{ topProducts { name } }"#).await;
        router_handle.shutdown().await.unwrap();
    }

    async fn assert_federated_response(listen_addr: &ListenAddr, request: &str) {
        let request = Request::builder().query(request).build();
        let expected = query(listen_addr, &request).await.unwrap();

        let response = to_string_pretty(&expected).unwrap();
        assert!(!response.is_empty());
    }

    async fn query(
        listen_addr: &ListenAddr,
        request: &graphql::Request,
    ) -> Result<graphql::Response, crate::error::FetchError> {
        Ok(reqwest::Client::new()
            .post(format!("{}/", listen_addr))
            .json(request)
            .send()
            .await
            .expect("couldn't send request")
            .json()
            .await
            .expect("couldn't deserialize into json"))
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn config_by_file_watching() {
        let (path, mut file) = create_temp_file();
        let contents = include_str!("testdata/supergraph_config.yaml");
        write_and_flush(&mut file, contents).await;
        let mut stream = ConfigurationSource::File {
            path,
            watch: true,
            delay: Some(Duration::from_millis(10)),
        }
        .into_stream()
        .boxed();

        // First update is guaranteed
        assert!(matches!(
            stream.next().await.unwrap(),
            UpdateConfiguration(_)
        ));

        // Modify the file and try again
        write_and_flush(&mut file, contents).await;
        assert!(matches!(
            stream.next().await.unwrap(),
            UpdateConfiguration(_)
        ));

        // This time write garbage, there should not be an update.
        write_and_flush(&mut file, ":garbage").await;
        assert!(stream.into_future().now_or_never().is_none());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn config_by_file_invalid() {
        let (path, mut file) = create_temp_file();
        write_and_flush(&mut file, "Garbage").await;
        let mut stream = ConfigurationSource::File {
            path,
            watch: true,
            delay: None,
        }
        .into_stream();

        // First update fails because the file is invalid.
        assert!(matches!(stream.next().await.unwrap(), NoMoreConfiguration));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn config_by_file_missing() {
        let mut stream = ConfigurationSource::File {
            path: temp_dir().join("does_not_exit"),
            watch: true,
            delay: None,
        }
        .into_stream();

        // First update fails because the file is invalid.
        assert!(matches!(stream.next().await.unwrap(), NoMoreConfiguration));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn config_by_file_no_watch() {
        let (path, mut file) = create_temp_file();
        let contents = include_str!("testdata/supergraph_config.yaml");
        write_and_flush(&mut file, contents).await;

        let mut stream = ConfigurationSource::File {
            path,
            watch: false,
            delay: None,
        }
        .into_stream();
        assert!(matches!(
            stream.next().await.unwrap(),
            UpdateConfiguration(_)
        ));
        assert!(matches!(stream.next().await.unwrap(), NoMoreConfiguration));
    }

    #[test(tokio::test)]
    async fn schema_by_file_watching() {
        let (path, mut file) = create_temp_file();
        let schema = include_str!("testdata/supergraph.graphql");
        write_and_flush(&mut file, schema).await;
        let mut stream = SchemaSource::File {
            path,
            watch: true,
            delay: Some(Duration::from_millis(10)),
        }
        .into_stream()
        .boxed();

        // First update is guaranteed
        assert!(matches!(stream.next().await.unwrap(), UpdateSchema(_)));

        // Modify the file and try again
        write_and_flush(&mut file, schema).await;
        assert!(matches!(stream.next().await.unwrap(), UpdateSchema(_)));
    }

    #[test(tokio::test)]
    async fn schema_by_file_missing() {
        let mut stream = SchemaSource::File {
            path: temp_dir().join("does_not_exit"),
            watch: true,
            delay: None,
        }
        .into_stream();

        // First update fails because the file is invalid.
        assert!(matches!(stream.next().await.unwrap(), NoMoreSchema));
    }

    #[test(tokio::test)]
    async fn schema_by_file_no_watch() {
        let (path, mut file) = create_temp_file();
        let schema = include_str!("testdata/supergraph.graphql");
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
}
