use super::graph_factory::GraphFactory;
use super::http_server_factory::{HttpServerFactory, HttpServerHandle};
use super::state_machine::PrivateState::{Errored, Running, Startup, Stopped};
use super::Event::{UpdateConfiguration, UpdateSchema};
use super::FederatedServerError::{NoConfiguration, NoSchema};
use super::{Event, FederatedServerError, State};
use crate::configuration::Configuration;
use apollo_router_core::prelude::*;
use futures::channel::mpsc;
use futures::prelude::*;
use std::marker::PhantomData;
use std::sync::Arc;
use Event::{NoMoreConfiguration, NoMoreSchema, Shutdown};

/// This state maintains private information that is not exposed to the user via state listener.
#[derive(Debug)]
enum PrivateState<F>
where
    F: graphql::Fetcher,
{
    Startup {
        configuration: Option<Configuration>,
        schema: Option<graphql::Schema>,
    },
    Running {
        configuration: Arc<Configuration>,
        schema: Arc<graphql::Schema>,
        graph: Arc<F>,
        server_handle: HttpServerHandle,
    },
    Stopped,
    Errored(FederatedServerError),
}

/// A state machine that responds to events to control the lifecycle of the server.
/// The server is in startup state until both configuration and schema are supplied.
/// If config and schema are not supplied then the machine ends with an error.
/// Once schema and config are obtained running state is entered.
/// Config and schema updates will try to swap in the new values into the running state. In future we may trigger an http server restart if for instance socket address is encountered.
/// At any point a shutdown event will case the machine to try to get to stopped state.  
pub(crate) struct StateMachine<S, F, FA>
where
    S: HttpServerFactory,
    F: graphql::Fetcher + 'static,
    FA: GraphFactory<F>,
{
    http_server_factory: S,
    state_listener: Option<mpsc::Sender<State>>,
    graph_factory: FA,
    phantom: PhantomData<F>,
}

impl<F> From<&PrivateState<F>> for State
where
    F: graphql::Fetcher,
{
    fn from(private_state: &PrivateState<F>) -> Self {
        match private_state {
            Startup { .. } => State::Startup,
            Running { server_handle, .. } => State::Running(server_handle.listen_address),
            Stopped => State::Stopped,
            Errored { .. } => State::Errored,
        }
    }
}

impl<S, F, FA> StateMachine<S, F, FA>
where
    S: HttpServerFactory,
    F: graphql::Fetcher,
    FA: GraphFactory<F>,
{
    pub(crate) fn new(
        http_server_factory: S,
        state_listener: Option<mpsc::Sender<State>>,
        graph_factory: FA,
    ) -> Self {
        Self {
            http_server_factory,
            state_listener,
            graph_factory,
            phantom: Default::default(),
        }
    }

    pub(crate) async fn process_events(
        mut self,
        mut messages: impl Stream<Item = Event> + Unpin,
    ) -> Result<(), FederatedServerError> {
        log::debug!("Starting");
        let mut state = Startup {
            configuration: None,
            schema: None,
        };
        let mut state_listener = self.state_listener.take();
        let initial_state = State::from(&state);
        <StateMachine<S, F, FA>>::notify_state_listener(&mut state_listener, initial_state).await;
        while let Some(message) = messages.next().await {
            let last_public_state = State::from(&state);
            let new_state = match (state, message) {
                // Startup: Handle configuration updates, maybe transition to running.
                (Startup { configuration, .. }, UpdateSchema(new_schema)) => {
                    self.maybe_transition_to_running(Startup {
                        configuration,
                        schema: Some(new_schema),
                    })
                    .await
                }
                // Startup: Handle schema updates, maybe transition to running.
                (Startup { schema, .. }, UpdateConfiguration(new_configuration)) => {
                    self.maybe_transition_to_running(Startup {
                        configuration: Some(new_configuration),
                        schema,
                    })
                    .await
                }

                // Startup: Missing configuration.
                (
                    Startup {
                        configuration: None,
                        ..
                    },
                    NoMoreConfiguration,
                ) => Errored(NoConfiguration),

                // Startup: Missing schema.
                (Startup { schema: None, .. }, NoMoreSchema) => Errored(NoSchema),

                // Startup: Go straight for shutdown.
                (Startup { .. }, Shutdown) => Stopped,

                // Running: Handle shutdown.
                (Running { server_handle, .. }, Shutdown) => {
                    log::debug!("Shutting down");
                    match server_handle.shutdown().await {
                        Ok(_) => Stopped,
                        Err(err) => Errored(err),
                    }
                }

                // Running: Handle schema updates
                (
                    Running {
                        configuration,
                        schema: _,
                        graph: _,
                        server_handle,
                    },
                    UpdateSchema(new_schema),
                ) => {
                    log::debug!("Reloading schema");
                    let schema = Arc::new(new_schema);
                    let graph = Arc::new(
                        self.graph_factory
                            .create(&configuration, Arc::clone(&schema)),
                    );

                    Running {
                        configuration,
                        schema,
                        graph,
                        server_handle,
                    }
                }

                // Running: Handle configuration updates
                (
                    Running {
                        configuration: _,
                        schema,
                        graph,
                        server_handle,
                    },
                    UpdateConfiguration(new_configuration),
                ) => {
                    log::debug!("Reloading configuration");

                    let configuration = Arc::new(new_configuration);
                    let server_handle =
                        if server_handle.listen_address != configuration.server.listen {
                            log::debug!("Restarting http");
                            if let Err(_err) = server_handle.shutdown().await {
                                log::error!("Failed to notify shutdown")
                            }
                            let new_handle = self
                                .http_server_factory
                                .create(Arc::clone(&graph), Arc::clone(&configuration))
                                .await;
                            log::debug!("Restarted on {}", new_handle.listen_address);
                            new_handle
                        } else {
                            server_handle
                        };

                    Running {
                        configuration,
                        schema,
                        graph,
                        server_handle,
                    }
                }

                // Anything else we don't care about
                (state, message) => {
                    log::debug!("Ignoring message transition {:?}", message);
                    state
                }
            };

            let new_public_state = State::from(&new_state);
            if last_public_state != new_public_state {
                <StateMachine<S, F, FA>>::notify_state_listener(
                    &mut state_listener,
                    new_public_state,
                )
                .await;
            }
            log::debug!("Transitioned to state {:?}", &new_state);
            state = new_state;

            // If we've errored then exit even if there are potentially more messages
            if matches!(&state, Errored(_)) {
                break;
            }
        }
        log::debug!("Stopped");

        match state {
            Stopped => Ok(()),
            Errored(err) => Err(err),
            _ => {
                panic!("Must finish on stopped or errored state.")
            }
        }
    }

    async fn notify_state_listener(
        state_listener: &mut Option<mpsc::Sender<State>>,
        new_public_state: State,
    ) {
        if let Some(state_listener) = state_listener {
            let _ = state_listener.send(new_public_state).await;
        }
    }

    async fn maybe_transition_to_running(&self, state: PrivateState<F>) -> PrivateState<F> {
        if let Startup {
            configuration: Some(configuration),
            schema: Some(schema),
        } = state
        {
            log::debug!("Starting http");

            let schema = Arc::new(schema);
            let graph = Arc::new(
                self.graph_factory
                    .create(&configuration, Arc::clone(&schema)),
            );
            let configuration = Arc::new(configuration);

            let server_handle = self
                .http_server_factory
                .create(Arc::clone(&graph), Arc::clone(&configuration))
                .await;
            log::debug!("Started on {}", server_handle.listen_address);
            Running {
                configuration,
                schema,
                graph,
                server_handle,
            }
        } else {
            state
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph_factory::MockGraphFactory;
    use crate::http_server_factory::MockHttpServerFactory;
    use async_trait::async_trait;
    use futures::channel::oneshot;
    use mockall::{mock, predicate::*};
    use parking_lot::Mutex;
    use std::net::SocketAddr;
    use std::str::FromStr;

    #[ctor::ctor]
    fn init() {
        let _ = env_logger::builder().is_test(true).try_init();
    }

    #[tokio::test]
    async fn no_configuration() {
        let graph_factory = create_mock_graph_factory(0);
        let (server_factory, _) = create_mock_server_factory(0);
        assert!(matches!(
            execute(
                server_factory,
                graph_factory,
                vec![NoMoreConfiguration],
                vec![State::Startup, State::Errored]
            )
            .await,
            Err(NoConfiguration),
        ));
    }

    #[tokio::test]
    async fn no_schema() {
        let graph_factory = create_mock_graph_factory(0);
        let (server_factory, _) = create_mock_server_factory(0);
        assert!(matches!(
            execute(
                server_factory,
                graph_factory,
                vec![NoMoreSchema],
                vec![State::Startup, State::Errored]
            )
            .await,
            Err(NoSchema),
        ));
    }

    #[tokio::test]
    async fn shutdown_during_startup() {
        let graph_factory = create_mock_graph_factory(0);
        let (server_factory, _) = create_mock_server_factory(0);
        assert!(matches!(
            execute(
                server_factory,
                graph_factory,
                vec![Shutdown],
                vec![State::Startup, State::Stopped]
            )
            .await,
            Ok(()),
        ));
    }

    #[tokio::test]
    async fn startup_shutdown() {
        let graph_factory = create_mock_graph_factory(1);
        let (server_factory, shutdown_receivers) = create_mock_server_factory(1);

        assert!(matches!(
            execute(
                server_factory,
                graph_factory,
                vec![
                    UpdateConfiguration(
                        Configuration::builder()
                            .subgraphs(Default::default())
                            .build()
                    ),
                    UpdateSchema("".parse().unwrap()),
                    Shutdown
                ],
                vec![
                    State::Startup,
                    State::Running(SocketAddr::from_str("127.0.0.1:4000").unwrap()),
                    State::Stopped
                ]
            )
            .await,
            Ok(()),
        ));
        assert_eq!(shutdown_receivers.lock().len(), 1);
    }

    #[tokio::test]
    async fn startup_reload_schema() {
        let graph_factory = create_mock_graph_factory(2);
        let (server_factory, shutdown_receivers) = create_mock_server_factory(1);

        assert!(matches!(
            execute(
                server_factory,
                graph_factory,
                vec![
                    UpdateConfiguration(
                        Configuration::builder()
                            .subgraphs(Default::default())
                            .build()
                    ),
                    UpdateSchema("".parse().unwrap()),
                    UpdateSchema("".parse().unwrap()),
                    Shutdown
                ],
                vec![
                    State::Startup,
                    State::Running(SocketAddr::from_str("127.0.0.1:4000").unwrap()),
                    State::Stopped
                ]
            )
            .await,
            Ok(()),
        ));
        assert_eq!(shutdown_receivers.lock().len(), 1);
    }

    #[tokio::test]
    async fn startup_reload_configuration() {
        let graph_factory = create_mock_graph_factory(1);
        let (server_factory, shutdown_receivers) = create_mock_server_factory(2);

        assert!(matches!(
            execute(
                server_factory,
                graph_factory,
                vec![
                    UpdateConfiguration(
                        Configuration::builder()
                            .subgraphs(Default::default())
                            .build()
                    ),
                    UpdateSchema("".parse().unwrap()),
                    UpdateConfiguration(
                        Configuration::builder()
                            .server(
                                crate::configuration::Server::builder()
                                    .listen(SocketAddr::from_str("127.0.0.1:4001").unwrap())
                                    .build()
                            )
                            .subgraphs(Default::default())
                            .build()
                    ),
                    Shutdown
                ],
                vec![
                    State::Startup,
                    State::Running(SocketAddr::from_str("127.0.0.1:4000").unwrap()),
                    State::Running(SocketAddr::from_str("127.0.0.1:4001").unwrap()),
                    State::Stopped
                ]
            )
            .await,
            Ok(()),
        ));
        assert_eq!(shutdown_receivers.lock().len(), 2);
    }

    mock! {
        #[derive(Debug)]
        MyFetcher {}

        #[async_trait]
        impl graphql::Fetcher for MyFetcher {
            async fn stream(&self, request: graphql::Request) -> graphql::ResponseStream;
        }
    }

    async fn execute(
        server_factory: MockHttpServerFactory,
        graph_factory: MockGraphFactory<MockMyFetcher>,
        events: Vec<Event>,
        expected_states: Vec<State>,
    ) -> Result<(), FederatedServerError> {
        let (state_listener, state_reciever) = mpsc::channel(100);
        let state_machine = StateMachine::new(server_factory, Some(state_listener), graph_factory);
        let result = state_machine
            .process_events(stream::iter(events).boxed())
            .await;
        let states = state_reciever.collect::<Vec<State>>().await;
        assert_eq!(states, expected_states);
        result
    }

    fn create_mock_server_factory(
        expect_times_called: usize,
    ) -> (
        MockHttpServerFactory,
        Arc<Mutex<Vec<oneshot::Receiver<()>>>>,
    ) {
        let mut server_factory = MockHttpServerFactory::new();
        let shutdown_receivers = Arc::new(Mutex::new(vec![]));
        let shutdown_receivers_clone = shutdown_receivers.to_owned();
        server_factory
            .expect_create()
            .times(expect_times_called)
            .returning(
                move |_: Arc<MockMyFetcher>, configuration: Arc<Configuration>| {
                    let (shutdown_sender, shutdown_receiver) = oneshot::channel::<()>();
                    shutdown_receivers_clone.lock().push(shutdown_receiver);

                    Box::pin(async move {
                        HttpServerHandle {
                            shutdown_sender,
                            server_future: future::ready(Ok(())).boxed(),
                            listen_address: configuration.server.listen,
                        }
                    })
                },
            );
        (server_factory, shutdown_receivers)
    }

    fn create_mock_graph_factory(expect_times_called: usize) -> MockGraphFactory<MockMyFetcher> {
        let mut graph_factory = MockGraphFactory::new();
        graph_factory
            .expect_create()
            .times(expect_times_called)
            .returning(|_, _| MockMyFetcher::new());
        graph_factory
    }
}
