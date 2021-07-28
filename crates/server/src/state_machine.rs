use std::sync::{Arc, RwLock};

use futures::channel::mpsc;
use futures::prelude::*;
use log::{debug, error};

use configuration::Configuration;
use execution::federated::FederatedGraph;
use execution::http_service_registry::HttpServiceRegistry;
use query_planner::caching::WithCaching;
use query_planner::harmonizer::HarmonizerQueryPlanner;
use Event::{NoMoreConfiguration, NoMoreSchema, Shutdown};

use crate::http_server_factory::{HttpServerFactory, HttpServerHandle};
use crate::state_machine::PrivateState::{Errored, Running, Startup, Stopped};
use crate::Event::{UpdateConfiguration, UpdateSchema};
use crate::FederatedServerError::{NoConfiguration, NoSchema};
use crate::{Event, FederatedServerError, Schema, State};

/// This state maintains private information that is not exposed to the user via state listener.
enum PrivateState {
    Startup {
        configuration: Option<Configuration>,
        schema: Option<Schema>,
    },
    Running {
        configuration: Arc<RwLock<Configuration>>,
        schema: Arc<RwLock<Schema>>,
        graph: Arc<RwLock<FederatedGraph>>,
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
pub(crate) struct StateMachine<S>
where
    S: HttpServerFactory,
{
    http_server_factory: S,
    state_listener: Option<mpsc::Sender<State>>,
}

impl From<&PrivateState> for State {
    fn from(private_state: &PrivateState) -> Self {
        match private_state {
            Startup { .. } => State::Startup,
            Running { server_handle, .. } => State::Running(server_handle.listen_address),
            Stopped => State::Stopped,
            Errored { .. } => State::Errored,
        }
    }
}

impl<S> StateMachine<S>
where
    S: HttpServerFactory,
{
    pub(crate) fn new(http_server_factory: S, state_listener: Option<mpsc::Sender<State>>) -> Self {
        Self {
            http_server_factory,
            state_listener,
        }
    }

    pub(crate) async fn process_events(
        mut self,
        mut messages: impl Stream<Item = Event> + Unpin,
    ) -> Result<(), FederatedServerError> {
        debug!("Starting");
        let mut state = Startup {
            configuration: None,
            schema: None,
        };
        let mut state_listener = self.state_listener.take();
        let initial_state = State::from(&state);
        <StateMachine<S>>::notify_state_listener(&mut state_listener, initial_state).await;
        while let Some(message) = messages.next().await {
            let last_public_state = State::from(&state);
            let new_state = match (state, message) {
                // Startup: Handle configuration updates, maybe transition to running.
                (Startup { configuration, .. }, UpdateSchema(new_schema)) => self
                    .maybe_transition_to_running(Startup {
                        configuration,
                        schema: Some(new_schema),
                    }),
                // Startup: Handle schema updates, maybe transition to running.
                (Startup { schema, .. }, UpdateConfiguration(new_configuration)) => self
                    .maybe_transition_to_running(Startup {
                        configuration: Some(new_configuration),
                        schema,
                    }),

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
                    debug!("Shutting down");
                    match server_handle.shutdown().await {
                        Ok(_) => Stopped,
                        Err(err) => Errored(err),
                    }
                }

                // Running: Handle schema updates
                (
                    Running {
                        configuration,
                        schema,
                        graph,
                        server_handle,
                    },
                    UpdateSchema(new_schema),
                ) => {
                    debug!("Reloading schema");
                    let new_graph = <StateMachine<S>>::create_graph(
                        &configuration.read().unwrap(), //unwrap-lock
                        &schema.read().unwrap(),        //unwrap-lock
                    );
                    *schema.write().unwrap() = new_schema; //unwrap-lock
                    *graph.write().unwrap() = new_graph; //unwrap-lock
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
                        configuration,
                        schema,
                        graph,
                        server_handle,
                    },
                    UpdateConfiguration(new_configuration),
                ) => {
                    debug!("Reloading configuration");

                    *configuration.write().unwrap() = new_configuration; //unwrap-lock
                    let server_handle = if server_handle.listen_address
                        != configuration.read().unwrap().listen
                    //unwrap-lock
                    {
                        debug!("Restarting http");
                        if let Err(_err) = server_handle.shutdown().await {
                            error!("Failed to notify shutdown")
                        }
                        let new_handle = self
                            .http_server_factory
                            .create(graph.to_owned(), configuration.to_owned());
                        debug!("Restarted on {}", new_handle.listen_address);
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
                (state, _message) => state,
            };

            let new_public_state = State::from(&new_state);
            if last_public_state != new_public_state {
                <StateMachine<S>>::notify_state_listener(&mut state_listener, new_public_state)
                    .await
            }
            state = new_state;
        }
        debug!("Stopped");

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

    fn create_graph(configuration: &Configuration, schema: &str) -> FederatedGraph {
        let service_registry = HttpServiceRegistry::new(configuration);
        FederatedGraph::new(
            HarmonizerQueryPlanner::new(schema.to_owned()).with_caching(),
            Arc::new(service_registry),
        )
    }

    fn maybe_transition_to_running(&self, state: PrivateState) -> PrivateState {
        if let Startup {
            configuration: Some(configuration),
            schema: Some(schema),
        } = state
        {
            debug!("Starting http");

            let graph = Arc::new(RwLock::new(<StateMachine<S>>::create_graph(
                &configuration,
                &schema,
            )));
            let configuration = Arc::new(RwLock::new(configuration));
            let schema = Arc::new(RwLock::new(schema));

            let server_handle = self
                .http_server_factory
                .create(graph.to_owned(), configuration.to_owned());
            debug!("Started on {}", server_handle.listen_address);
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
    use std::collections::HashMap;
    use std::net::SocketAddr;
    use std::str::FromStr;
    use std::sync::Mutex;

    use futures::channel::oneshot;
    use futures::prelude::*;

    use crate::http_server_factory::MockHttpServerFactory;

    use super::*;

    fn init() {
        let _ = env_logger::builder().is_test(true).try_init();
    }

    #[tokio::test]
    async fn no_configuration() {
        init();
        let server_factory = MockHttpServerFactory::new();
        assert_eq!(
            Err(NoConfiguration),
            execute(
                server_factory,
                vec![NoMoreConfiguration],
                vec![State::Startup, State::Errored]
            )
            .await
        );
    }

    #[tokio::test]
    async fn no_schema() {
        init();
        let server_factory = MockHttpServerFactory::new();
        assert_eq!(
            Err(NoSchema),
            execute(
                server_factory,
                vec![NoMoreSchema],
                vec![State::Startup, State::Errored]
            )
            .await
        );
    }

    #[tokio::test]
    async fn shutdown_during_startup() {
        init();
        let server_factory = MockHttpServerFactory::new();
        assert_eq!(
            Ok(()),
            execute(
                server_factory,
                vec![Shutdown],
                vec![State::Startup, State::Stopped]
            )
            .await
        );
    }

    #[tokio::test]
    async fn startup_shutdown() {
        init();

        let mut server_factory = MockHttpServerFactory::new();
        let shutdown_receivers = expect_graceful_shutdown(&mut server_factory);
        assert_eq!(
            Ok(()),
            execute(
                server_factory,
                vec![
                    UpdateConfiguration(Configuration::builder().subgraphs(HashMap::new()).build()),
                    UpdateSchema("".to_string()),
                    Shutdown
                ],
                vec![
                    State::Startup,
                    State::Running(SocketAddr::from_str("127.0.0.1:4000").unwrap()),
                    State::Stopped
                ]
            )
            .await
        );
        assert_eq!(shutdown_receivers.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn startup_reload_schema() {
        init();

        let mut server_factory = MockHttpServerFactory::new();
        let shutdown_receivers = expect_graceful_shutdown(&mut server_factory);
        assert_eq!(
            Ok(()),
            execute(
                server_factory,
                vec![
                    UpdateConfiguration(Configuration::builder().subgraphs(HashMap::new()).build()),
                    UpdateSchema("".to_string()),
                    UpdateSchema("".to_string()),
                    Shutdown
                ],
                vec![
                    State::Startup,
                    State::Running(SocketAddr::from_str("127.0.0.1:4000").unwrap()),
                    State::Stopped
                ]
            )
            .await
        );
        assert_eq!(shutdown_receivers.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn startup_reload_configuration() {
        init();

        let mut server_factory = MockHttpServerFactory::new();
        let shutdown_receivers = expect_graceful_shutdown(&mut server_factory);
        assert_eq!(
            Ok(()),
            execute(
                server_factory,
                vec![
                    UpdateConfiguration(Configuration::builder().subgraphs(HashMap::new()).build()),
                    UpdateSchema("".to_string()),
                    UpdateConfiguration(
                        Configuration::builder()
                            .listen(SocketAddr::from_str("127.0.0.1:4001").unwrap())
                            .subgraphs(HashMap::new())
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
            .await
        );
        assert_eq!(shutdown_receivers.lock().unwrap().len(), 2);
    }

    async fn execute(
        server_factory: MockHttpServerFactory,
        events: Vec<Event>,
        expected_states: Vec<State>,
    ) -> Result<(), FederatedServerError> {
        let (state_listener, state_reciever) = mpsc::channel(100);
        let state_machine = StateMachine::new(server_factory, Some(state_listener));
        let result = state_machine
            .process_events(stream::iter(events).boxed())
            .await;
        let states = state_reciever.collect::<Vec<State>>().await;
        assert_eq!(states, expected_states);
        result
    }

    fn expect_graceful_shutdown(
        server_factory: &mut MockHttpServerFactory,
    ) -> Arc<Mutex<Vec<oneshot::Receiver<()>>>> {
        let shutdown_receivers = Arc::new(Mutex::new(vec![]));
        let shutdown_receivers_clone = shutdown_receivers.to_owned();
        server_factory.expect_create().returning(
            move |_: Arc<RwLock<FederatedGraph>>, configuration: Arc<RwLock<Configuration>>| {
                let (shutdown_sender, shutdown_receiver) = oneshot::channel::<()>();
                shutdown_receivers_clone
                    .lock()
                    .unwrap()
                    .push(shutdown_receiver);
                HttpServerHandle {
                    shutdown_sender,
                    server_future: future::ready(Ok(())).boxed(),
                    listen_address: configuration.read().unwrap().listen, //unwrap-lock
                }
            },
        );
        shutdown_receivers
    }
}
