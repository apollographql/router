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
use tracing::subscriber::SetGlobalDefaultError;
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
use crate::reload::Error as ReloadError;
use crate::router_factory::YamlRouterServiceFactory;
use crate::state_machine::StateMachine;

type SchemaStream = Pin<Box<dyn Stream<Item = crate::Schema> + Send>>;

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

    /// could not deserialize configuration: {0}
    DeserializeConfigError(serde_yaml::Error),

    /// could not read configuration: {0}
    ReadConfigError(std::io::Error),

    /// {0}
    ConfigError(crate::configuration::ConfigurationError),

    /// could not read schema: {0}
    ReadSchemaError(crate::error::SchemaError),

    /// could not create the HTTP pipeline: {0}
    ServiceCreationError(tower::BoxError),

    /// could not create the HTTP server: {0}
    ServerCreationError(std::io::Error),

    /// could not configure spaceport
    ServerSpaceportError,

    /// no reload handle available
    NoReloadTracingHandleError,

    /// could not set global subscriber: {0}
    SetGlobalSubscriberError(SetGlobalDefaultError),

    /// could not reload tracing layer: {0}
    ReloadTracingLayerError(ReloadError),
}

/// The user supplied schema. Either a static instance or a stream for hot reloading.
#[derive(From, Display, Derivative)]
#[derivative(Debug)]
pub enum SchemaKind {
    /// A static schema.
    #[display(fmt = "Instance")]
    Instance(Box<crate::Schema>),

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

impl From<crate::Schema> for SchemaKind {
    fn from(schema: crate::Schema) -> Self {
        Self::Instance(Box::new(schema))
    }
}

impl SchemaKind {
    /// Convert this schema into a stream regardless of if is static or not. Allows for unified handling later.
    fn into_stream(self) -> impl Stream<Item = Event> {
        match self {
            SchemaKind::Instance(instance) => stream::iter(vec![UpdateSchema(instance)]).boxed(),
            SchemaKind::Stream(stream) => {
                stream.map(|schema| UpdateSchema(Box::new(schema))).boxed()
            }
            SchemaKind::File { path, watch, delay } => {
                // Sanity check, does the schema file exists, if it doesn't then bail.
                if !path.exists() {
                    tracing::error!(
                        "Schema file at path '{}' does not exist.",
                        path.to_string_lossy()
                    );
                    stream::empty().boxed()
                } else {
                    //The schema file exists try and load it
                    match ConfigurationKind::read_schema(&path) {
                        Ok(schema) => {
                            if watch {
                                crate::files::watch(path.to_owned(), delay)
                                    .filter_map(move |_| {
                                        future::ready(ConfigurationKind::read_schema(&path).ok())
                                    })
                                    .map(|schema| UpdateSchema(Box::new(schema)))
                                    .boxed()
                            } else {
                                stream::once(future::ready(UpdateSchema(Box::new(schema)))).boxed()
                            }
                        }
                        Err(err) => {
                            tracing::error!("Failed to read schema: {}", err);
                            stream::empty().boxed()
                        }
                    }
                }
            }
            SchemaKind::Registry {
                apollo_key,
                apollo_graph_ref,
                urls,
                poll_interval,
            } => {
                apollo_uplink::stream_supergraph(apollo_key, apollo_graph_ref, urls, poll_interval)
                    .filter_map(|res| {
                        future::ready(match res {
                            Ok(schema_result) => schema_result
                                .schema
                                .parse()
                                .map_err(|e| {
                                    tracing::error!("could not parse schema: {:?}", e);
                                })
                                .ok(),

                            Err(e) => {
                                tracing::error!(
                                    "error downloading the schema from Uplink: {:?}",
                                    e
                                );
                                None
                            }
                        })
                    })
                    .map(|schema| UpdateSchema(Box::new(schema)))
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
pub enum ConfigurationKind {
    /// A static configuration.
    #[display(fmt = "Instance")]
    #[from(types(Configuration))]
    Instance(Box<Configuration>),

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

impl ConfigurationKind {
    /// Convert this config into a stream regardless of if is static or not. Allows for unified handling later.
    fn into_stream(self) -> impl Stream<Item = Event> {
        match self {
            ConfigurationKind::Instance(instance) => {
                stream::iter(vec![UpdateConfiguration(instance)]).boxed()
            }
            ConfigurationKind::Stream(stream) => {
                stream.map(|x| UpdateConfiguration(Box::new(x))).boxed()
            }
            ConfigurationKind::File { path, watch, delay } => {
                // Sanity check, does the config file exists, if it doesn't then bail.
                if !path.exists() {
                    tracing::error!(
                        "configuration file at path '{}' does not exist.",
                        path.to_string_lossy()
                    );
                    stream::empty().boxed()
                } else {
                    match ConfigurationKind::read_config(&path) {
                        Ok(configuration) => {
                            if watch {
                                crate::files::watch(path.to_owned(), delay)
                                    .filter_map(move |_| {
                                        future::ready(match ConfigurationKind::read_config(&path) {
                                            Ok(config) => Some(config),
                                            Err(err) => {
                                                tracing::error!("{}", err);
                                                None
                                            }
                                        })
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

    fn read_config(path: &Path) -> Result<Configuration, ApolloRouterError> {
        let config = fs::read_to_string(path).map_err(ApolloRouterError::ReadConfigError)?;
        let config = validate_configuration(&config).map_err(ApolloRouterError::ConfigError)?;

        Ok(config)
    }

    fn read_schema(path: &Path) -> Result<crate::Schema, ApolloRouterError> {
        crate::Schema::read(path).map_err(ApolloRouterError::ReadSchemaError)
    }
}

type ShutdownFuture = Pin<Box<dyn Future<Output = ()> + Send>>;

/// The user supplied shutdown hook.
#[derive(Display, Derivative)]
#[derivative(Debug)]
pub enum ShutdownKind {
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

impl ShutdownKind {
    /// Convert this shutdown hook into a future. Allows for unified handling later.
    fn into_stream(self) -> impl Stream<Item = Event> {
        match self {
            ShutdownKind::None => stream::pending::<Event>().boxed(),
            ShutdownKind::Custom(future) => future.map(|_| Shutdown).into_stream().boxed(),
            ShutdownKind::CtrlC => async {
                tokio::signal::ctrl_c()
                    .await
                    .expect("Failed to install CTRL+C signal handler");
            }
            .map(|_| Shutdown)
            .into_stream()
            .boxed(),
        }
    }
}

/// Federated server takes requests and federates a response based on calls to subgraphs.
///
/// # Examples
///
/// ```
/// use apollo_router::ApolloRouter;
/// use apollo_router::Configuration;
/// use apollo_router::ShutdownKind;
///
/// async {
///     let configuration = serde_yaml::from_str::<Configuration>("Config").unwrap();
///     let schema: apollo_router::Schema = "schema".parse().unwrap();
///     let server = ApolloRouter::builder()
///             .configuration(configuration)
///             .schema(schema)
///             .shutdown(ShutdownKind::CtrlC)
///             .build();
///     server.serve().await;
/// };
/// ```
///
/// Shutdown via handle.
/// ```
/// use apollo_router::ApolloRouter;
/// use apollo_router::Configuration;
/// use apollo_router::ShutdownKind;
///
/// async {
///     let configuration = serde_yaml::from_str::<Configuration>("Config").unwrap();
///     let schema: apollo_router::Schema = "schema".parse().unwrap();
///     let server = ApolloRouter::builder()
///             .configuration(configuration)
///             .schema(schema)
///             .shutdown(ShutdownKind::CtrlC)
///             .build();
///     let handle = server.serve();
///     drop(handle);
/// };
/// ```
///
pub struct ApolloRouter {
    /// The Configuration that the server will use. This can be static or a stream for hot reloading.
    pub(crate) configuration: ConfigurationKind,

    /// The Schema that the server will use. This can be static or a stream for hot reloading.
    pub(crate) schema: SchemaKind,

    /// A future that when resolved will shut down the server.
    pub(crate) shutdown: ShutdownKind,

    pub(crate) router_factory: YamlRouterServiceFactory,
}

#[buildstructor::buildstructor]
impl ApolloRouter {
    /// Build a new Apollo router.
    ///
    /// This must only be called in the context of Executable::builder() because it relies on custom logging setup to support hot reload.
    #[builder]
    pub fn new(
        configuration: ConfigurationKind,
        schema: SchemaKind,
        shutdown: Option<ShutdownKind>,
    ) -> ApolloRouter {
        ApolloRouter {
            configuration,
            schema,
            shutdown: shutdown.unwrap_or(ShutdownKind::CtrlC),
            router_factory: YamlRouterServiceFactory::default(),
        }
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
    UpdateSchema(Box<crate::Schema>),

    /// There are no more updates to the schema
    NoMoreSchema,

    /// The server should gracefully shutdown.
    Shutdown,
}

/// A handle that allows the client to await for various server events.
pub struct RouterHandle {
    result: Pin<Box<dyn Future<Output = Result<(), ApolloRouterError>> + Send>>,
    listen_address: Arc<RwLock<Option<ListenAddr>>>,
    shutdown_sender: Option<oneshot::Sender<()>>,
}

impl RouterHandle {
    /// Returns the listen address when the router is ready to receive requests.
    pub async fn listen_address(&self) -> Result<ListenAddr, ApolloRouterError> {
        self.listen_address
            .read()
            .await
            .clone()
            .ok_or(ApolloRouterError::StartupError)
    }
}

impl Drop for RouterHandle {
    fn drop(&mut self) {
        let _ = self
            .shutdown_sender
            .take()
            .expect("shutdown sender must be present")
            .send(());
    }
}

impl Future for RouterHandle {
    type Output = Result<(), ApolloRouterError>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        self.result.poll_unpin(cx)
    }
}

impl ApolloRouter {
    /// Start the federated server on a separate thread.
    ///
    /// Dropping the server handle will shutdown the server.
    ///
    /// returns: RouterHandle
    ///
    pub fn serve(self) -> RouterHandle {
        let server_factory = AxumHttpServerFactory::new();
        let (shutdown_sender, shutdown_receiver) = oneshot::channel::<()>();
        let event_stream = Self::generate_event_stream(
            self.shutdown,
            self.configuration,
            self.schema,
            shutdown_receiver,
        );

        let state_machine = StateMachine::new(server_factory, self.router_factory);
        let listen_address = state_machine.listen_address.clone();
        let result = spawn(async move { state_machine.process_events(event_stream).await })
            .map(|r| match r {
                Ok(Ok(ok)) => Ok(ok),
                Ok(Err(err)) => Err(err),
                Err(err) => {
                    tracing::error!("{}", err);
                    Err(ApolloRouterError::StartupError)
                }
            })
            .boxed();

        RouterHandle {
            result,
            shutdown_sender: Some(shutdown_sender),
            listen_address,
        }
    }

    /// Create the unified event stream.
    /// This merges all contributing streams and sets up shutdown handling.
    /// When a shutdown message is received no more events are emitted.
    fn generate_event_stream(
        shutdown: ShutdownKind,
        configuration: ConfigurationKind,
        schema: SchemaKind,
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

    fn init_with_server() -> RouterHandle {
        let configuration =
            serde_yaml::from_str::<Configuration>(include_str!("testdata/supergraph_config.yaml"))
                .unwrap();
        let schema: crate::Schema = include_str!("testdata/supergraph.graphql").parse().unwrap();
        ApolloRouter::builder()
            .configuration(configuration)
            .schema(schema)
            .build()
            .serve()
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn basic_request() {
        let router_handle = init_with_server();
        let listen_address = router_handle
            .listen_address()
            .await
            .expect("router failed to start");
        assert_federated_response(&listen_address, r#"{ topProducts { name } }"#).await;
        drop(router_handle);
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
        let mut stream = ConfigurationKind::File {
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
        let mut stream = ConfigurationKind::File {
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
        let mut stream = ConfigurationKind::File {
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

        let mut stream = ConfigurationKind::File {
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
        let mut stream = SchemaKind::File {
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
        let mut stream = SchemaKind::File {
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

        let mut stream = SchemaKind::File {
            path,
            watch: false,
            delay: None,
        }
        .into_stream();
        assert!(matches!(stream.next().await.unwrap(), UpdateSchema(_)));
        assert!(matches!(stream.next().await.unwrap(), NoMoreSchema));
    }
}
