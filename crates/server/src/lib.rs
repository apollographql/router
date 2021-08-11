//! Starts a server that will handle http graphql requests.
use std::net::SocketAddr;
use std::pin::Pin;
use std::task::{Context, Poll};

use derivative::Derivative;
use derive_more::Display;
use derive_more::From;
use futures::channel::{mpsc, oneshot};
use futures::prelude::*;
use log::error;
use std::time::Duration;
use thiserror::Error;
use tokio::task::spawn;
use typed_builder::TypedBuilder;

use configuration::Configuration;
use Event::{Shutdown, UpdateConfiguration, UpdateSchema};

use crate::hyper_http_server_factory::HyperHttpServerFactory;
use crate::state_machine::StateMachine;
use crate::Event::{NoMoreConfiguration, NoMoreSchema};
use futures::FutureExt;
use std::fs::{read, read_to_string};
use std::path::{Path, PathBuf};

mod files;
mod http_server_factory;
mod hyper_http_server_factory;
mod state_machine;

type Schema = String;

type SchemaStream = Pin<Box<dyn Stream<Item = Schema> + Send>>;

/// Error types for FederatedServer
#[derive(Error, Debug, PartialEq, Clone)]
pub enum FederatedServerError {
    /// Something went wrong when trying to start the server.
    #[error("Failed to start server")]
    StartupError,

    /// Something went wrong when trying to shutdown the http server.
    #[error("Failed to stop HTTP Server")]
    HttpServerLifecycleError,

    /// Configuration was not supplied.
    #[error("Configuration was not supplied")]
    NoConfiguration,

    /// Schema was not supplied.
    #[error("Schema was not supplied")]
    NoSchema,
}

/// The user supplied schema. Either a static instance or a stream for hot reloading.
#[derive(From, Display, Derivative)]
#[derivative(Debug)]
pub enum SchemaKind {
    /// A static schema.
    #[display(fmt = "Instance")]
    Instance(Schema),

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

    /// A YAML file that may be watched for changes.
    #[display(fmt = "File")]
    Registry {
        /// The Apollo key: <YOUR_GRAPH_API_KEY>
        apollo_key: String,

        /// The apollo graph reference: <YOUR_GRAPH_ID>@<VARIANT>
        apollo_graph_ref: String,
    },
}

impl SchemaKind {
    /// Convert this schema into a stream regardless of if is static or not. Allows for unified handling later.
    fn into_stream(self) -> impl Stream<Item = Event> {
        match self {
            SchemaKind::Instance(instance) => stream::iter(vec![UpdateSchema(instance)]).boxed(),
            SchemaKind::Stream(stream) => stream.map(UpdateSchema).boxed(),
            SchemaKind::File { path, watch, delay } => {
                // Sanity check, does the schema file exists, if it doesn't then bail.
                if !path.exists() {
                    log::error!(
                        "Schema file at path '{}' does not exist.",
                        path.to_string_lossy()
                    );
                    stream::empty().boxed()
                } else {
                    //The schema file exists try and load it
                    let schema = ConfigurationKind::read_schema(&path);
                    match schema {
                        Some(schema) => {
                            if watch {
                                files::watch(path.to_owned(), delay)
                                    .filter_map(move |_| {
                                        future::ready(ConfigurationKind::read_schema(&path))
                                    })
                                    .map(UpdateSchema)
                                    .boxed()
                            } else {
                                stream::once(future::ready(UpdateSchema(schema))).boxed()
                            }
                        }
                        None => stream::empty().boxed(),
                    }
                }
            }
            SchemaKind::Registry { .. } => {
                todo!("Registry is not supported yet")
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
    Instance(Configuration),

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
            ConfigurationKind::Stream(stream) => stream.map(UpdateConfiguration).boxed(),
            ConfigurationKind::File { path, watch, delay } => {
                // Sanity check, does the config file exists, if it doesn't then bail.
                if !path.exists() {
                    log::error!(
                        "Configuration file at path '{}' does not exist.",
                        path.to_string_lossy()
                    );
                    stream::empty().boxed()
                } else {
                    // The config file exists try and load it
                    let configuration = ConfigurationKind::read_config(&path);
                    match configuration {
                        Some(configuration) => {
                            if watch {
                                files::watch(path.to_owned(), delay)
                                    .filter_map(move |_| {
                                        future::ready(ConfigurationKind::read_config(&path))
                                    })
                                    .map(UpdateConfiguration)
                                    .boxed()
                            } else {
                                stream::once(future::ready(UpdateConfiguration(configuration)))
                                    .boxed()
                            }
                        }
                        None => stream::empty().boxed(),
                    }
                }
            }
        }
        .chain(stream::iter(vec![NoMoreConfiguration]))
        .boxed()
    }

    fn read_config(path: &Path) -> Option<Configuration> {
        match read(&path) {
            Ok(bytes) => match serde_yaml::from_slice::<Configuration>(&bytes) {
                Ok(configuration) => Some(configuration),
                Err(err) => {
                    log::error!("Invalid configuration: {}", err);
                    None
                }
            },
            Err(err) => {
                log::error!("Failed to read configuration: {}", err);
                None
            }
        }
    }

    fn read_schema(path: &Path) -> Option<Schema> {
        match read_to_string(&path) {
            Ok(string) => Some(string),
            Err(err) => {
                log::error!("Failed to read schema: {}", err);
                None
            }
        }
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
/// use server::FederatedServer;
/// use server::ShutdownKind;
/// use configuration::Configuration;
///
/// async {
///     let configuration = serde_yaml::from_str::<Configuration>("Config").unwrap();
///     let schema = "schema".to_string();
///     let server = FederatedServer::builder()
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
/// use server::FederatedServer;
/// use server::ShutdownKind;
/// use configuration::Configuration;
///
/// async {
///     let configuration = serde_yaml::from_str::<Configuration>("Config").unwrap();
///     let schema = "schema".to_string();
///     let server = FederatedServer::builder()
///             .configuration(configuration)
///             .schema(schema)
///             .shutdown(ShutdownKind::CtrlC)
///             .build();
///     let handle = server.serve();
///     handle.shutdown().await;
/// };
/// ```
///
#[derive(TypedBuilder, Debug)]
#[builder(field_defaults(setter(into)))]
pub struct FederatedServer {
    /// The Configuration that the server will use. This can be static or a stream for hot reloading.
    configuration: ConfigurationKind,

    /// The Schema that the server will use. This can be static or a stream for hot reloading.
    schema: SchemaKind,

    /// A future that when resolved will shut down the server.
    #[builder(default = ShutdownKind::None)]
    shutdown: ShutdownKind,
}

/// Messages that are broadcast across the app.
#[derive(PartialEq, Debug, Clone)]
enum Event {
    /// The configuration was updated.
    UpdateConfiguration(Configuration),

    /// There are no more updates to the configuration
    NoMoreConfiguration,

    /// The schema was updated.
    UpdateSchema(Schema),

    /// There are no more updates to the schema
    NoMoreSchema,

    /// The server should gracefully shutdown.
    Shutdown,
}

/// Public state that the client can be notified with via state listener
/// This is useful for waiting until the server is actually serving requests.
#[derive(Debug, Eq, PartialEq, Clone)]
pub enum State {
    /// The server is starting up.
    Startup,

    /// The server is running on a particular address.
    Running(SocketAddr),

    /// The server has stopped.
    Stopped,

    /// The server has errored.
    Errored,
}

/// A handle that allows the client to await for various server events.
#[derive(Derivative)]
#[derivative(Debug)]
pub struct FederatedServerHandle {
    #[derivative(Debug = "ignore")]
    result: Pin<Box<dyn Future<Output = Result<(), FederatedServerError>> + Send>>,
    #[derivative(Debug = "ignore")]
    shutdown_sender: oneshot::Sender<()>,
    #[derivative(Debug = "ignore")]
    state_receiver: Option<mpsc::Receiver<State>>,
}

impl FederatedServerHandle {
    /// Wait until the server is ready and return the socket address that it is listening on.
    /// If the socket address has been configured to port zero the OS will choose the port.
    /// The socket address returned is the actual port that was bound.
    ///
    /// This method can only be called once, and is not designed for use in dynamic configuration
    /// scenarios.
    ///
    /// returns: Option<SocketAddr>
    pub async fn ready(&mut self) -> Option<SocketAddr> {
        self.state_receiver()
            .map(|state| {
                if let State::Running(socket) = state {
                    Some(socket)
                } else {
                    None
                }
            })
            .filter(|socket| future::ready(socket != &None))
            .map(|s| s.unwrap())
            .next()
            .boxed()
            .await
    }

    /// Return a receiver of lifecycle events for the server. This method may only be called once.
    ///
    /// returns: mspc::Receiver<State>
    pub fn state_receiver(&mut self) -> mpsc::Receiver<State> {
        self.state_receiver.take().expect(
            "State listener has already been taken. 'ready' or 'state' may be called once only.",
        )
    }

    /// Trigger and wait until the server has shut down.
    ///
    /// returns: Result<(), FederatedServerError>
    pub async fn shutdown(mut self) -> Result<(), FederatedServerError> {
        self.maybe_close_state_receiver();
        if self.shutdown_sender.send(()).is_err() {
            log::error!("Failed to send shutdown event")
        }
        self.result.await
    }

    /// If the state receiver has not been set it must be closed otherwise it'll block the
    /// state machine from progressing.
    fn maybe_close_state_receiver(&mut self) {
        if let Some(mut state_receiver) = self.state_receiver.take() {
            state_receiver.close();
        }
    }
}

impl Future for FederatedServerHandle {
    type Output = Result<(), FederatedServerError>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        self.maybe_close_state_receiver();
        self.result.poll_unpin(cx)
    }
}

impl FederatedServer {
    /// Start the federated server on a separate thread.
    ///
    /// The returned handle allows the user to await until the server is ready and shutdown.
    /// Alternatively the user can await on the server handle itself to wait for shutdown via the
    /// configured shutdown mechanism.
    ///
    /// returns: FederatedServerHandle
    ///
    pub fn serve(self) -> FederatedServerHandle {
        let (state_listener, state_receiver) = mpsc::channel::<State>(1);
        let server_factory = HyperHttpServerFactory::new();
        let state_machine = StateMachine::new(server_factory, Some(state_listener));
        let (shutdown_sender, shutdown_receiver) = oneshot::channel::<()>();
        let result = spawn(async {
            state_machine
                .process_events(self.generate_event_stream(shutdown_receiver))
                .await
        })
        .map(|r| match r {
            Ok(Ok(ok)) => Ok(ok),
            Ok(Err(err)) => Err(err),
            Err(_err) => Err(FederatedServerError::StartupError),
        })
        .boxed();

        FederatedServerHandle {
            result,
            shutdown_sender,
            state_receiver: Some(state_receiver),
        }
    }

    /// Create the unified event stream.
    /// This merges all contributing streams and sets up shutdown handling.
    /// When a shutdown message is received no more events are emitted.
    fn generate_event_stream(
        self,
        shutdown_receiver: oneshot::Receiver<()>,
    ) -> impl Stream<Item = Event> {
        // Chain is required so that the final shutdown message is sent.
        let messages = stream::select_all(vec![
            self.shutdown.into_stream().boxed(),
            self.configuration.into_stream().boxed(),
            self.schema.into_stream().boxed(),
            shutdown_receiver.into_stream().map(|_| Shutdown).boxed(),
        ])
        .take_while(|msg| future::ready(msg != &Shutdown))
        .chain(stream::iter(vec![Shutdown]))
        .boxed();
        messages
    }
}

#[cfg(test)]
mod tests {
    use futures::prelude::*;
    use serde_json::to_string_pretty;

    use execution::http_subgraph::HttpSubgraphFetcher;
    use execution::{GraphQLFetcher, GraphQLRequest, GraphQLResponseStream};

    use super::*;
    use crate::files::tests::{create_temp_file, write_and_flush};
    use std::env::temp_dir;

    fn init_with_server() -> FederatedServerHandle {
        let _ = env_logger::builder().is_test(true).try_init();

        let configuration =
            serde_yaml::from_str::<Configuration>(include_str!("testdata/supergraph_config.yaml"))
                .unwrap();
        let schema = include_str!("testdata/supergraph.graphql").to_string();
        FederatedServer::builder()
            .configuration(configuration)
            .schema(schema)
            .build()
            .serve()
    }

    fn init() {
        let _ = env_logger::builder().is_test(true).try_init();
    }

    #[tokio::test]
    async fn basic_request() {
        let mut server_handle = init_with_server();
        let socket = server_handle.ready().await.expect("Server never ready");
        assert_federated_response(&socket, r#"{ topProducts { name } }"#).await;
        server_handle.shutdown().await.expect("Could not shutdown");
    }

    async fn assert_federated_response(socket: &SocketAddr, request: &str) {
        let request = GraphQLRequest::builder().query(request).build();
        let mut expected = query(socket, request.clone());

        let expected = expected.next().await.unwrap().primary();
        let response = to_string_pretty(&expected).unwrap();
        assert!(!response.is_empty());
    }

    fn query(socket: &SocketAddr, request: GraphQLRequest) -> GraphQLResponseStream {
        HttpSubgraphFetcher::new(
            "federated".into(),
            format!("http://{}/graphql", socket).into(),
        )
        .stream(request)
    }

    #[tokio::test]
    async fn config_by_file_watching() {
        init();
        let (path, mut file) = create_temp_file();
        let contents = include_str!("testdata/supergraph_config.yaml");
        let configuration = serde_yaml::from_slice::<Configuration>(contents.as_bytes()).unwrap();
        write_and_flush(&mut file, contents).await;
        let mut stream = ConfigurationKind::File {
            path,
            watch: true,
            delay: Some(Duration::from_millis(10)),
        }
        .into_stream()
        .boxed();

        // First update is guaranteed
        assert_eq!(
            stream.next().await.unwrap(),
            UpdateConfiguration(configuration.to_owned())
        );

        // Modify the file and try again
        write_and_flush(&mut file, contents).await;
        assert_eq!(
            stream.next().await.unwrap(),
            UpdateConfiguration(configuration)
        );

        // This time write garbage, there should not be an update.
        write_and_flush(&mut file, ":").await;
        assert!(stream.into_future().now_or_never().is_none());
    }

    #[tokio::test]
    async fn config_by_file_invalid() {
        init();
        let (path, mut file) = create_temp_file();
        write_and_flush(&mut file, "Garbage").await;
        let mut stream = ConfigurationKind::File {
            path,
            watch: true,
            delay: None,
        }
        .into_stream();

        // First update fails because the file is invalid.
        assert_eq!(stream.next().await.unwrap(), NoMoreConfiguration);
    }

    #[tokio::test]
    async fn config_by_file_missing() {
        init();
        let mut stream = ConfigurationKind::File {
            path: PathBuf::from(temp_dir().join("does_not_exit")),
            watch: true,
            delay: None,
        }
        .into_stream();

        // First update fails because the file is invalid.
        assert_eq!(stream.next().await.unwrap(), NoMoreConfiguration);
    }

    #[tokio::test]
    async fn config_by_file_no_watch() {
        init();
        let (path, mut file) = create_temp_file();
        let contents = include_str!("testdata/supergraph_config.yaml");
        let configuration = serde_yaml::from_slice::<Configuration>(contents.as_bytes()).unwrap();
        write_and_flush(&mut file, contents).await;

        let mut stream = ConfigurationKind::File {
            path,
            watch: false,
            delay: None,
        }
        .into_stream();
        assert_eq!(
            stream.next().await.unwrap(),
            UpdateConfiguration(configuration)
        );
        assert_eq!(stream.next().await.unwrap(), NoMoreConfiguration);
    }

    #[tokio::test]
    async fn schema_by_file_watching() {
        init();
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
        assert_eq!(stream.next().await.unwrap(), UpdateSchema(schema.into()));

        // Modify the file and try again
        write_and_flush(&mut file, schema).await;
        assert_eq!(stream.next().await.unwrap(), UpdateSchema(schema.into()));
    }

    #[tokio::test]
    async fn schema_by_file_missing() {
        init();
        let mut stream = SchemaKind::File {
            path: PathBuf::from(temp_dir().join("does_not_exit")),
            watch: true,
            delay: None,
        }
        .into_stream();

        // First update fails because the file is invalid.
        assert_eq!(stream.next().await.unwrap(), NoMoreSchema);
    }

    #[tokio::test]
    async fn schema_by_file_no_watch() {
        init();
        let (path, mut file) = create_temp_file();
        let schema = include_str!("testdata/supergraph.graphql");
        write_and_flush(&mut file, schema).await;

        let mut stream = SchemaKind::File {
            path,
            watch: false,
            delay: None,
        }
        .into_stream();
        assert_eq!(stream.next().await.unwrap(), UpdateSchema(schema.into()));
        assert_eq!(stream.next().await.unwrap(), NoMoreSchema);
    }
}
