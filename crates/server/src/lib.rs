//! Starts a server that will handle http graphql requests.
use std::pin::Pin;

use derivative::Derivative;
use derive_more::Display;
use derive_more::From;
use futures::future::ready;
use futures::stream::{iter, pending, select_all};
use futures::{Future, FutureExt, Stream, StreamExt, TryFutureExt};
use log::error;
use thiserror::Error;
use typed_builder::TypedBuilder;

use configuration::Configuration;
use Event::{Shutdown, UpdateConfiguration, UpdateSchema};

use crate::hyper_http_server_factory::HyperHttpServerFactory;
use crate::state_machine::StateMachine;
use crate::Event::{NoMoreConfiguration, NoMoreSchema};
use futures::channel::mpsc::channel;
use futures::channel::oneshot::Receiver;
use futures::channel::{mpsc, oneshot};
use std::net::SocketAddr;
use std::task::{Context, Poll};
use tokio::task::spawn;
mod http_server_factory;
mod hyper_http_server_factory;
mod state_machine;

type Schema = String;

type SchemaStream = Pin<Box<dyn Stream<Item = Schema> + Send>>;

/// Error types for FederatedServer
#[derive(Error, Debug, PartialEq, Clone)]
pub enum FederatedServerError {
    /// Something went wrong when trying to shutdown the http server.
    #[error("Failed to stop http")]
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
pub enum SchemaType {
    /// A static schema.
    #[display(fmt = "Instance")]
    Instance(Schema),

    /// A stream of schema.
    #[display(fmt = "Stream")]
    Stream(#[derivative(Debug = "ignore")] SchemaStream),
}

impl SchemaType {
    /// Convert this schema into a stream regardless of if is static or not. Allows for unified handling later.
    fn into_stream(self) -> impl Stream<Item = Event> {
        match self {
            SchemaType::Instance(instance) => iter(vec![UpdateSchema(instance)]).boxed(),
            SchemaType::Stream(stream) => stream.map(UpdateSchema).boxed(),
        }
        .chain(iter(vec![NoMoreSchema]))
    }
}

type ConfigurationStream = Pin<Box<dyn Stream<Item = Configuration> + Send>>;

/// The user supplied config. Either a static instance or a stream for hot reloading.
#[derive(From, Display, Derivative)]
#[derivative(Debug)]
pub enum ConfigurationType {
    /// A static configuration.
    #[display(fmt = "Instance")]
    Instance(Configuration),

    /// A configuration stream where the server will react to new configuration. If possible
    /// the configuration will be applied without restarting the internal http server.
    #[display(fmt = "Stream")]
    Stream(#[derivative(Debug = "ignore")] ConfigurationStream),
}

impl ConfigurationType {
    /// Convert this config into a stream regardless of if is static or not. Allows for unified handling later.
    fn into_stream(self) -> impl Stream<Item = Event> {
        match self {
            ConfigurationType::Instance(instance) => {
                iter(vec![UpdateConfiguration(instance)]).boxed()
            }
            ConfigurationType::Stream(stream) => stream.map(UpdateConfiguration).boxed(),
        }
        .chain(iter(vec![NoMoreConfiguration]))
    }
}

type ShutdownFuture = Pin<Box<dyn Future<Output = ()> + Send>>;

/// The user supplied shutdown hook.
#[derive(Display, Derivative)]
#[derivative(Debug)]
pub enum ShutdownType {
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

impl ShutdownType {
    /// Convert this shutdown hook into a future. Allows for unified handling later.
    fn into_stream(self) -> impl Stream<Item = Event> {
        match self {
            ShutdownType::None => pending::<Event>().boxed(),
            ShutdownType::Custom(future) => future.map(|_| Shutdown).into_stream().boxed(),
            ShutdownType::CtrlC => async {
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
/// use server::ShutdownType;
/// use configuration::Configuration;
///
/// async {
///     let configuration = serde_yaml::from_str::<Configuration>("Config").unwrap();
///     let schema = "schema".to_string();
///     let server = FederatedServer::builder()
///             .configuration(configuration)
///             .schema(schema)
///             .shutdown(ShutdownType::CtrlC)
///             .build();
///     server.serve().await;
/// };
/// ```
///
/// Shutdown via handle.
/// ```
/// use server::FederatedServer;
/// use server::ShutdownType;
/// use configuration::Configuration;
///
/// async {
///     let configuration = serde_yaml::from_str::<Configuration>("Config").unwrap();
///     let schema = "schema".to_string();
///     let server = FederatedServer::builder()
///             .configuration(configuration)
///             .schema(schema)
///             .shutdown(ShutdownType::CtrlC)
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
    configuration: ConfigurationType,

    /// The Schema that the server will use. This can be static or a stream for hot reloading.
    schema: SchemaType,

    /// A future that when resolved will shut down the server.
    #[builder(default = ShutdownType::None)]
    shutdown: ShutdownType,
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
#[derive(Debug, Eq, PartialEq)]
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
        self.state()
            .map(|state| {
                if let State::Running(socket) = state {
                    Some(socket)
                } else {
                    None
                }
            })
            .filter(|socket| ready(socket != &None))
            .map(|s| s.unwrap())
            .next()
            .boxed()
            .await
    }

    /// Return a receiver of lifecycle events for the server. This method may only be called once.
    ///
    /// returns: mspc::Receiver<State>
    fn state(&mut self) -> mpsc::Receiver<State> {
        self.state_receiver.take().expect(
            "State listener has already been taken. 'ready' or 'state' may be called once only.",
        )
    }

    /// Trigger and wait until the server has shut down.
    ///
    /// returns: Result<(), FederatedServerError>
    pub async fn shutdown(mut self) -> Result<(), FederatedServerError> {
        self.maybe_close_state_receiver();
        match self.shutdown_sender.send(()) {
            Ok(_) => {}
            Err(_) => {
                error!("Failed to send shutdown event")
            }
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
        let (state_listener, state_receiver) = channel::<State>(1);
        let server_factory = HyperHttpServerFactory::new();
        let state_machine = StateMachine::new(server_factory, Some(state_listener));
        let (shutdown_sender, shutdown_receiver) = oneshot::channel::<()>();
        let result = spawn(async {
            state_machine
                .process_events(self.generate_event_stream(shutdown_receiver))
                .await
        })
        .map_err(|_| FederatedServerError::HttpServerLifecycleError)
        .map(|r| match r {
            Ok(Ok(ok)) => Ok(ok),
            Ok(Err(err)) => Err(err),
            Err(err) => Err(err),
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
    fn generate_event_stream(self, shutdown_receiver: Receiver<()>) -> impl Stream<Item = Event> {
        // Chain is required so that the final shutdown message is sent.
        let messages = select_all(vec![
            self.shutdown.into_stream().boxed(),
            self.configuration.into_stream().boxed(),
            self.schema.into_stream().boxed(),
            shutdown_receiver.into_stream().map(|_| Shutdown).boxed(),
        ])
        .take_while(|msg| ready(msg != &Shutdown))
        .chain(iter(vec![Shutdown]))
        .boxed();
        messages
    }
}

#[cfg(test)]
mod tests {
    use futures::StreamExt;
    use serde_json::to_string_pretty;

    use execution::http_subgraph::HttpSubgraphFetcher;
    use execution::{GraphQLFetcher, GraphQLRequest, GraphQLResponseStream};

    use super::*;
    use log::LevelFilter;

    fn init() -> FederatedServerHandle {
        let _ = env_logger::builder()
            .filter_level(LevelFilter::Debug)
            //.filter("execution".into(), LevelFilter::Debug)
            //.is_test(true)
            .try_init();

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

    #[tokio::test]
    async fn basic_request() {
        let mut server_handle = init();
        let socket = server_handle.ready().await.expect("Server never ready");
        assert_federated_response(&socket, r#"{ topProducts { name } }"#).await;
        server_handle.shutdown().await.expect("Could not shutdown");
    }

    async fn assert_federated_response(socket: &SocketAddr, request: &str) {
        let request = GraphQLRequest {
            query: request.into(),
            operation_name: None,
            variables: None,
            extensions: None,
        };
        let mut expected = query(socket, request.clone());

        let expected = expected.next().await.unwrap().unwrap().primary();
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
}
