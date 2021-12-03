//! Starts a server that will handle http graphql requests.

mod apollo_router;
pub mod configuration;
mod files;
mod http_server_factory;
pub mod http_service_registry;
pub mod http_subgraph;
mod router_factory;
mod state_machine;
mod warp_http_server_factory;

pub use self::apollo_router::*;
use crate::router_factory::ApolloRouterFactory;
use crate::state_machine::StateMachine;
use crate::warp_http_server_factory::WarpHttpServerFactory;
use crate::Event::{NoMoreConfiguration, NoMoreSchema};
use apollo_router_core::prelude::*;
use configuration::{Configuration, OpenTelemetry};
use derivative::Derivative;
use derive_more::Display;
use derive_more::From;
use displaydoc::Display as DisplayDoc;
use futures::channel::{mpsc, oneshot};
use futures::prelude::*;
use futures::FutureExt;
use once_cell::sync::OnceCell;
use opentelemetry::sdk::trace::BatchSpanProcessor;
use opentelemetry::trace::TracerProvider;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::str::FromStr;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;
use thiserror::Error;
use tokio::task::spawn;
use tracing_subscriber::prelude::*;
use tracing_subscriber::EnvFilter;
use typed_builder::TypedBuilder;
use Event::{Shutdown, UpdateConfiguration, UpdateSchema};

type SchemaStream = Pin<Box<dyn Stream<Item = graphql::Schema> + Send>>;

pub static GLOBAL_ENV_FILTER: OnceCell<String> = OnceCell::new();

/// Error types for FederatedServer.
#[derive(Error, Debug, DisplayDoc)]
pub enum FederatedServerError {
    /// Failed to start server.
    StartupError,

    /// Failed to stop HTTP Server.
    HttpServerLifecycleError,

    /// Configuration was not supplied.
    NoConfiguration,

    /// Schema was not supplied.
    NoSchema,

    /// Could not deserialize configuration: {0}
    DeserializeConfigError(serde_yaml::Error),

    /// Could not read configuration: {0}
    ReadConfigError(std::io::Error),

    /// Could not read schema: {0}
    ReadSchemaError(graphql::SchemaError),

    /// Could not create the HTTP server: {0}
    ServerCreationError(std::io::Error),
}

/// The user supplied schema. Either a static instance or a stream for hot reloading.
#[derive(From, Display, Derivative)]
#[derivative(Debug)]
pub enum SchemaKind {
    /// A static schema.
    #[display(fmt = "Instance")]
    Instance(graphql::Schema),

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
                                files::watch(path.to_owned(), delay)
                                    .filter_map(move |_| {
                                        future::ready(ConfigurationKind::read_schema(&path).ok())
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
                        "Configuration file at path '{}' does not exist.",
                        path.to_string_lossy()
                    );
                    stream::empty().boxed()
                } else {
                    match ConfigurationKind::read_config(&path) {
                        Ok(configuration) => {
                            if watch {
                                files::watch(path.to_owned(), delay)
                                    .filter_map(move |_| {
                                        future::ready(ConfigurationKind::read_config(&path).ok())
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
                            tracing::error!("Failed to read configuration: {}", err);
                            stream::empty().boxed()
                        }
                    }
                }
            }
        }
        .map(|event| match event {
            UpdateConfiguration(mut config) => {
                match try_initialize_subscriber(&config) {
                    Ok(subscriber) => {
                        config.subscriber = subscriber;
                    }
                    Err(err) => {
                        tracing::error!("Could not initialize tracing subscriber: {}", err,)
                    }
                };
                UpdateConfiguration(config)
            }
            _ => event,
        })
        .chain(stream::iter(vec![NoMoreConfiguration]))
        .boxed()
    }

    fn read_config(path: &Path) -> Result<Configuration, FederatedServerError> {
        let file = std::fs::File::open(path).map_err(FederatedServerError::ReadConfigError)?;
        let config = serde_yaml::from_reader::<_, Configuration>(&file)
            .map_err(FederatedServerError::DeserializeConfigError)?;

        Ok(config)
    }

    fn read_schema(path: &Path) -> Result<graphql::Schema, FederatedServerError> {
        graphql::Schema::read(path).map_err(FederatedServerError::ReadSchemaError)
    }
}

fn try_initialize_subscriber(
    config: &Configuration,
) -> Result<Option<Arc<dyn tracing::Subscriber + Send + Sync + 'static>>, Box<dyn std::error::Error>>
{
    let subscriber = tracing_subscriber::fmt::fmt()
        .with_env_filter(EnvFilter::new(
            GLOBAL_ENV_FILTER
                .get()
                .map(|x| x.as_str())
                .unwrap_or("info"),
        ))
        .finish();

    match config.opentelemetry.as_ref() {
        Some(OpenTelemetry::Jaeger(config)) => {
            let default_config = Default::default();
            let config = config.as_ref().unwrap_or(&default_config);
            let mut pipeline =
                opentelemetry_jaeger::new_pipeline().with_service_name(&config.service_name);
            if let Some(url) = config.collector_endpoint.as_ref() {
                pipeline = pipeline.with_collector_endpoint(url.as_str());
            }
            if let Some(username) = config.username.as_ref() {
                pipeline = pipeline.with_collector_username(username);
            }
            if let Some(password) = config.password.as_ref() {
                pipeline = pipeline.with_collector_password(password);
            }

            let batch_size = std::env::var("OTEL_BSP_MAX_EXPORT_BATCH_SIZE")
                .ok()
                .and_then(|batch_size| usize::from_str(&batch_size).ok());

            let exporter = pipeline.init_async_exporter(opentelemetry::runtime::Tokio)?;

            let batch = BatchSpanProcessor::builder(exporter, opentelemetry::runtime::Tokio)
                .with_scheduled_delay(std::time::Duration::from_secs(1));
            let batch = if let Some(size) = batch_size {
                batch.with_max_export_batch_size(size)
            } else {
                batch
            }
            .build();

            let provider = opentelemetry::sdk::trace::TracerProvider::builder()
                .with_span_processor(batch)
                .build();

            let tracer = provider.tracer("opentelemetry-jaeger", Some(env!("CARGO_PKG_VERSION")));
            let _ = opentelemetry::global::set_tracer_provider(provider);

            let telemetry = tracing_opentelemetry::layer().with_tracer(tracer);

            opentelemetry::global::set_error_handler(handle_error)?;
            return Ok(Some(Arc::new(subscriber.with(telemetry))));
        }
        #[cfg(any(feature = "otlp-grpc", feature = "otlp-http"))]
        Some(OpenTelemetry::Otlp(configuration::otlp::Otlp::Tracing(tracing))) => {
            let tracer = if let Some(tracing) = tracing.as_ref() {
                tracing.tracer()?
            } else {
                configuration::otlp::Tracing::tracer_from_env()?
            };
            let telemetry = tracing_opentelemetry::layer().with_tracer(tracer);
            opentelemetry::global::set_error_handler(handle_error)?;
            return Ok(Some(Arc::new(subscriber.with(telemetry))));
        }
        None => {}
    }

    Ok(None)
}

pub fn handle_error<T: Into<opentelemetry::global::Error>>(err: T) {
    match err.into() {
        opentelemetry::global::Error::Trace(err) => {
            tracing::error!("OpenTelemetry trace error occurred: {}", err)
        }
        opentelemetry::global::Error::Other(err_msg) => {
            tracing::error!("OpenTelemetry error occurred: {}", err_msg)
        }
        other => {
            tracing::error!("OpenTelemetry error occurred: {:?}", other)
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
/// use apollo_router_core::prelude::*;
/// use apollo_router::FederatedServer;
/// use apollo_router::ShutdownKind;
/// use apollo_router::configuration::Configuration;
///
/// async {
///     let configuration = serde_yaml::from_str::<Configuration>("Config").unwrap();
///     let schema: graphql::Schema = "schema".parse().unwrap();
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
/// use apollo_router_core::prelude::*;
/// use apollo_router::FederatedServer;
/// use apollo_router::ShutdownKind;
/// use apollo_router::configuration::Configuration;
///
/// async {
///     let configuration = serde_yaml::from_str::<Configuration>("Config").unwrap();
///     let schema: graphql::Schema = "schema".parse().unwrap();
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
#[derive(Debug)]
enum Event {
    /// The configuration was updated.
    UpdateConfiguration(Box<Configuration>),

    /// There are no more updates to the configuration
    NoMoreConfiguration,

    /// The schema was updated.
    UpdateSchema(graphql::Schema),

    /// There are no more updates to the schema
    NoMoreSchema,

    /// The server should gracefully shutdown.
    Shutdown,
}

/// Public state that the client can be notified with via state listener
/// This is useful for waiting until the server is actually serving requests.
#[derive(Debug, PartialEq)]
pub enum State {
    /// The server is starting up.
    Startup,

    /// The server is running on a particular address.
    Running { address: SocketAddr, schema: String },

    /// The server has stopped.
    Stopped,

    /// The server has errored.
    Errored,
}

/// A handle that allows the client to await for various server events.
pub struct FederatedServerHandle {
    result: Pin<Box<dyn Future<Output = Result<(), FederatedServerError>> + Send>>,
    shutdown_sender: oneshot::Sender<()>,
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
                if let State::Running { address, .. } = state {
                    Some(address)
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
            tracing::error!("Failed to send shutdown event")
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
        let server_factory = WarpHttpServerFactory::new();
        let state_machine = StateMachine::new(
            server_factory,
            Some(state_listener),
            ApolloRouterFactory::default(),
        );
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
        .take_while(|msg| future::ready(!matches!(msg, Shutdown)))
        .chain(stream::iter(vec![Shutdown]))
        .boxed();
        messages
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::files::tests::{create_temp_file, write_and_flush};
    use crate::http_subgraph::HttpSubgraphFetcher;
    use serde_json::to_string_pretty;
    use std::env::temp_dir;
    use test_log::test;

    fn init_with_server() -> FederatedServerHandle {
        let configuration =
            serde_yaml::from_str::<Configuration>(include_str!("testdata/supergraph_config.yaml"))
                .unwrap();
        let schema: graphql::Schema = include_str!("testdata/supergraph.graphql").parse().unwrap();
        FederatedServer::builder()
            .configuration(configuration)
            .schema(schema)
            .build()
            .serve()
    }

    #[test(tokio::test)]
    async fn basic_request() {
        let mut server_handle = init_with_server();
        let socket = server_handle.ready().await.expect("Server never ready");
        assert_federated_response(&socket, r#"{ topProducts { name } }"#).await;
        server_handle.shutdown().await.expect("Could not shutdown");
    }

    async fn assert_federated_response(socket: &SocketAddr, request: &str) {
        let request = graphql::Request::builder().query(request).build();
        let mut expected = query(socket, request.clone()).await;

        let expected = expected.next().await.unwrap();
        let response = to_string_pretty(&expected).unwrap();
        assert!(!response.is_empty());
    }

    async fn query(socket: &SocketAddr, request: graphql::Request) -> graphql::ResponseStream {
        HttpSubgraphFetcher::new("federated".into(), format!("http://{}/graphql", socket))
            .stream(request)
            .await
    }

    #[test(tokio::test)]
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
        write_and_flush(&mut file, ":").await;
        assert!(stream.into_future().now_or_never().is_none());
    }

    #[test(tokio::test)]
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

    #[test(tokio::test)]
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

    #[test(tokio::test)]
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
