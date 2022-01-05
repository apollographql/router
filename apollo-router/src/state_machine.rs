use super::http_server_factory::{HttpServerFactory, HttpServerHandle};
use super::router_factory::RouterFactory;
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
#[allow(clippy::large_enum_variant)]
enum PrivateState<Router, PreparedQuery>
where
    Router: graphql::Router<PreparedQuery>,
    PreparedQuery: graphql::PreparedQuery,
{
    Startup {
        configuration: Option<Configuration>,
        schema: Option<graphql::Schema>,
        phantom: PhantomData<(Router, PreparedQuery)>,
    },
    Running {
        configuration: Arc<Configuration>,
        schema: Arc<graphql::Schema>,
        router: Arc<Router>,
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
pub(crate) struct StateMachine<S, Router, PreparedQuery, FA>
where
    S: HttpServerFactory,
    Router: graphql::Router<PreparedQuery>,
    PreparedQuery: graphql::PreparedQuery,
    FA: RouterFactory<Router, PreparedQuery>,
{
    http_server_factory: S,
    state_listener: Option<mpsc::Sender<State>>,
    router_factory: FA,
    phantom: PhantomData<(Router, PreparedQuery)>,
}

impl<Router, PreparedQuery> From<&PrivateState<Router, PreparedQuery>> for State
where
    Router: graphql::Router<PreparedQuery>,
    PreparedQuery: graphql::PreparedQuery,
{
    fn from(private_state: &PrivateState<Router, PreparedQuery>) -> Self {
        match private_state {
            Startup { .. } => State::Startup,
            Running {
                server_handle,
                schema,
                ..
            } => State::Running {
                address: server_handle.listen_address(),
                schema: schema.as_str().to_string(),
            },
            Stopped => State::Stopped,
            Errored { .. } => State::Errored,
        }
    }
}

impl<S, Router, PreparedQuery, FA> StateMachine<S, Router, PreparedQuery, FA>
where
    S: HttpServerFactory,
    Router: graphql::Router<PreparedQuery> + 'static,
    PreparedQuery: graphql::PreparedQuery + 'static,
    FA: RouterFactory<Router, PreparedQuery>,
{
    pub(crate) fn new(
        http_server_factory: S,
        state_listener: Option<mpsc::Sender<State>>,
        router_factory: FA,
    ) -> Self {
        Self {
            http_server_factory,
            state_listener,
            router_factory,
            phantom: Default::default(),
        }
    }

    pub(crate) async fn process_events(
        mut self,
        mut messages: impl Stream<Item = Event> + Unpin,
    ) -> Result<(), FederatedServerError> {
        tracing::debug!("Starting");
        let mut state = Startup {
            configuration: None,
            schema: None,
            phantom: PhantomData,
        };
        let mut state_listener = self.state_listener.take();
        let initial_state = State::from(&state);
        <StateMachine<S, Router, PreparedQuery, FA>>::notify_state_listener(
            &mut state_listener,
            initial_state,
        )
        .await;
        while let Some(message) = messages.next().await {
            let last_public_state = State::from(&state);
            let new_state = match (state, message) {
                // Startup: Handle configuration updates, maybe transition to running.
                (Startup { configuration, .. }, UpdateSchema(new_schema)) => {
                    self.maybe_transition_to_running(Startup {
                        configuration,
                        schema: Some(*new_schema),
                        phantom: PhantomData,
                    })
                    .await
                }
                // Startup: Handle schema updates, maybe transition to running.
                (Startup { schema, .. }, UpdateConfiguration(new_configuration)) => {
                    self.maybe_transition_to_running(Startup {
                        configuration: Some(*new_configuration),
                        schema,
                        phantom: PhantomData,
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
                    tracing::debug!("Shutting down");
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
                        router,
                        server_handle,
                    },
                    UpdateSchema(new_schema),
                ) => {
                    tracing::debug!("Reloading schema");
                    let mut derived_configuration: Configuration =
                        configuration.as_ref().to_owned();
                    match derived_configuration.load_subgraphs(&new_schema) {
                        Err(e) => {
                            let strings = e.iter().map(ToString::to_string).collect::<Vec<_>>();
                            tracing::error!(
                                "The new configuration is invalid, keeping the previous one: {}",
                                strings.join(", ")
                            );
                            Running {
                                configuration,
                                schema,
                                router,
                                server_handle,
                            }
                        }
                        Ok(()) => {
                            tracing::info!("Reloading schema");
                            let derived_configuration = Arc::new(derived_configuration);

                            let schema = Arc::new(*new_schema);
                            let router = Arc::new(
                                self.router_factory
                                    .create(
                                        &derived_configuration,
                                        Arc::clone(&schema),
                                        Some(router),
                                    )
                                    .await,
                            );

                            match server_handle
                                .restart(
                                    &self.http_server_factory,
                                    Arc::clone(&router),
                                    derived_configuration,
                                )
                                .await
                            {
                                Ok(server_handle) => Running {
                                    configuration,
                                    schema,
                                    router,
                                    server_handle,
                                },
                                Err(err) => Errored(err),
                            }
                        }
                    }
                }

                // Running: Handle configuration updates
                (
                    Running {
                        configuration,
                        schema,
                        router,
                        server_handle,
                    },
                    UpdateConfiguration(new_configuration),
                ) => {
                    tracing::info!("Reloading configuration");

                    let mut derived_configuration = (*new_configuration).clone();
                    match derived_configuration.load_subgraphs(&schema) {
                        Err(e) => {
                            let strings = e.iter().map(ToString::to_string).collect::<Vec<_>>();
                            tracing::error!(
                                "The new configuration is invalid, keeping the previous one: {}",
                                strings.join(", ")
                            );
                            Running {
                                configuration,
                                schema,
                                router,
                                server_handle,
                            }
                        }
                        Ok(()) => {
                            let derived_configuration = Arc::new(derived_configuration);
                            let router = Arc::new(
                                self.router_factory
                                    .create(
                                        &derived_configuration,
                                        Arc::clone(&schema),
                                        Some(router),
                                    )
                                    .await,
                            );

                            match server_handle
                                .restart(
                                    &self.http_server_factory,
                                    Arc::clone(&router),
                                    Arc::clone(&derived_configuration),
                                )
                                .await
                            {
                                Ok(server_handle) => Running {
                                    configuration: Arc::new(*new_configuration),
                                    schema,
                                    router,
                                    server_handle,
                                },
                                Err(err) => Errored(err),
                            }
                        }
                    }
                }

                // Anything else we don't care about
                (state, message) => {
                    tracing::debug!("Ignoring message transition {:?}", message);
                    state
                }
            };

            let new_public_state = State::from(&new_state);
            if last_public_state != new_public_state {
                <StateMachine<S, Router, PreparedQuery, FA>>::notify_state_listener(
                    &mut state_listener,
                    new_public_state,
                )
                .await;
            }
            tracing::debug!("Transitioned to state {:?}", &new_state);
            state = new_state;

            // If we've errored then exit even if there are potentially more messages
            if matches!(&state, Errored(_)) {
                break;
            }
        }
        tracing::debug!("Stopped");

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

    async fn maybe_transition_to_running(
        &self,
        state: PrivateState<Router, PreparedQuery>,
    ) -> PrivateState<Router, PreparedQuery> {
        if let Startup {
            configuration: Some(configuration),
            schema: Some(schema),
            phantom: _,
        } = state
        {
            tracing::debug!("Starting http");

            let mut derived_configuration = configuration.clone();
            match derived_configuration.load_subgraphs(&schema) {
                Err(e) => {
                    let strings = e.iter().map(ToString::to_string).collect::<Vec<_>>();
                    tracing::error!(
                        "The new configuration is invalid, keeping the previous one: {}",
                        strings.join(", ")
                    );
                    Startup {
                        configuration: Some(configuration),
                        schema: Some(schema),
                        phantom: PhantomData,
                    }
                }
                Ok(()) => {
                    let schema = Arc::new(schema);
                    let router = Arc::new(
                        self.router_factory
                            .create(&derived_configuration, Arc::clone(&schema), None)
                            .await,
                    );

                    match self
                        .http_server_factory
                        .create(Arc::clone(&router), Arc::new(derived_configuration), None)
                        .await
                    {
                        Ok(server_handle) => {
                            tracing::debug!("Started on {}", server_handle.listen_address());

                            Running {
                                configuration: Arc::new(configuration),
                                schema,
                                router,
                                server_handle,
                            }
                        }

                        Err(err) => {
                            tracing::error!("Cannot start the router: {}", err);
                            Errored(err)
                        }
                    }
                }
            }
        } else {
            state
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::configuration::Subgraph;
    use crate::http_server_factory::MockHttpServerFactory;
    use crate::router_factory::RouterFactory;
    use futures::channel::oneshot;
    use mockall::{mock, predicate::*};
    use std::net::SocketAddr;
    use std::str::FromStr;
    use std::sync::Mutex;
    use test_log::test;
    use tokio::net::TcpListener;

    #[test(tokio::test)]
    async fn no_configuration() {
        let router_factory = create_mock_router_factory(0);
        let (server_factory, _) = create_mock_server_factory(0);
        assert!(matches!(
            execute(
                server_factory,
                router_factory,
                vec![NoMoreConfiguration],
                vec![State::Startup, State::Errored]
            )
            .await,
            Err(NoConfiguration),
        ));
    }

    #[test(tokio::test)]
    async fn no_schema() {
        let router_factory = create_mock_router_factory(0);
        let (server_factory, _) = create_mock_server_factory(0);
        assert!(matches!(
            execute(
                server_factory,
                router_factory,
                vec![NoMoreSchema],
                vec![State::Startup, State::Errored]
            )
            .await,
            Err(NoSchema),
        ));
    }

    #[test(tokio::test)]
    async fn shutdown_during_startup() {
        let router_factory = create_mock_router_factory(0);
        let (server_factory, _) = create_mock_server_factory(0);
        assert!(matches!(
            execute(
                server_factory,
                router_factory,
                vec![Shutdown],
                vec![State::Startup, State::Stopped]
            )
            .await,
            Ok(()),
        ));
    }

    #[test(tokio::test)]
    async fn startup_shutdown() {
        let router_factory = create_mock_router_factory(1);
        let (server_factory, shutdown_receivers) = create_mock_server_factory(1);

        assert!(matches!(
            execute(
                server_factory,
                router_factory,
                vec![
                    UpdateConfiguration(
                        Configuration::builder()
                            .subgraphs(Default::default())
                            .build()
                            .boxed()
                    ),
                    UpdateSchema(Box::new("".parse().unwrap())),
                    Shutdown
                ],
                vec![
                    State::Startup,
                    State::Running {
                        address: SocketAddr::from_str("127.0.0.1:4000").unwrap(),
                        schema: String::new()
                    },
                    State::Stopped
                ]
            )
            .await,
            Ok(()),
        ));
        assert_eq!(shutdown_receivers.lock().unwrap().len(), 1);
    }

    #[test(tokio::test)]
    async fn startup_reload_schema() {
        let router_factory = create_mock_router_factory(2);
        let (server_factory, shutdown_receivers) = create_mock_server_factory(2);
        let schema = include_str!("testdata/supergraph.graphql");

        assert!(matches!(
            execute(
                server_factory,
                router_factory,
                vec![
                    UpdateConfiguration(
                        Configuration::builder()
                            .subgraphs(Default::default())
                            .build()
                            .boxed()
                    ),
                    UpdateSchema(Box::new("".parse().unwrap())),
                    UpdateSchema(Box::new(schema.parse().unwrap())),
                    Shutdown
                ],
                vec![
                    State::Startup,
                    State::Running {
                        address: SocketAddr::from_str("127.0.0.1:4000").unwrap(),
                        schema: String::new()
                    },
                    State::Running {
                        address: SocketAddr::from_str("127.0.0.1:4000").unwrap(),
                        schema: schema.to_string(),
                    },
                    State::Stopped
                ]
            )
            .await,
            Ok(()),
        ));
        assert_eq!(shutdown_receivers.lock().unwrap().len(), 2);
    }

    #[test(tokio::test)]
    async fn startup_reload_configuration() {
        let router_factory = create_mock_router_factory(2);
        let (server_factory, shutdown_receivers) = create_mock_server_factory(2);

        assert!(matches!(
            execute(
                server_factory,
                router_factory,
                vec![
                    UpdateConfiguration(
                        Configuration::builder()
                            .subgraphs(Default::default())
                            .build()
                            .boxed()
                    ),
                    UpdateSchema(Box::new("".parse().unwrap())),
                    UpdateConfiguration(
                        Configuration::builder()
                            .server(
                                crate::configuration::Server::builder()
                                    .listen(SocketAddr::from_str("127.0.0.1:4001").unwrap())
                                    .build()
                            )
                            .subgraphs(Default::default())
                            .build()
                            .boxed()
                    ),
                    Shutdown
                ],
                vec![
                    State::Startup,
                    State::Running {
                        address: SocketAddr::from_str("127.0.0.1:4000").unwrap(),
                        schema: String::new()
                    },
                    State::Running {
                        address: SocketAddr::from_str("127.0.0.1:4001").unwrap(),
                        schema: String::new()
                    },
                    State::Stopped
                ]
            )
            .await,
            Ok(()),
        ));
        assert_eq!(shutdown_receivers.lock().unwrap().len(), 2);
    }

    #[test(tokio::test)]
    async fn extract_routing_urls() {
        let router_factory = create_mock_router_factory(1);
        let (server_factory, shutdown_receivers) = create_mock_server_factory(1);

        assert!(matches!(
            execute(
                server_factory,
                router_factory,
                vec![
                    UpdateConfiguration(
                        Configuration::builder()
                            .subgraphs(
                                [
                                    (
                                        "accounts".to_string(),
                                        Subgraph {
                                            routing_url: "http://accounts/graphql".to_string()
                                        }
                                    ),
                                    (
                                        "products".to_string(),
                                        Subgraph {
                                            routing_url: "http://accounts/graphql".to_string()
                                        }
                                    )
                                ]
                                .iter()
                                .cloned()
                                .collect()
                            )
                            .build()
                            .boxed()
                    ),
                    UpdateSchema(Box::new(r#"
                        enum join__Graph {
                            ACCOUNTS @join__graph(name: "accounts" url: "http://localhost:4001/graphql")
                            PRODUCTS @join__graph(name: "products" url: "")
                            INVENTORY @join__graph(name: "inventory" url: "http://localhost:4002/graphql")
                        }"#.parse().unwrap())),
                    Shutdown
                ],
                vec![
                    State::Startup,
                    State::Running {
                        address: SocketAddr::from_str("127.0.0.1:4000").unwrap(),
                        schema: r#"
                        enum join__Graph {
                            ACCOUNTS @join__graph(name: "accounts" url: "http://localhost:4001/graphql")
                            PRODUCTS @join__graph(name: "products" url: "")
                            INVENTORY @join__graph(name: "inventory" url: "http://localhost:4002/graphql")
                        }"#.to_string()
                    },
                    State::Stopped
                ]
            )
            .await,
            Ok(()),
        ));
        assert_eq!(shutdown_receivers.lock().unwrap().len(), 1);
    }

    #[test(tokio::test)]
    async fn extract_routing_urls_when_updating_configuration() {
        let mut router_factory = MockMyRouterFactory::new();
        // first call, we take the URL from the configuration
        router_factory
            .expect_create()
            .withf(|configuration, _schema, _previous_router| {
                configuration.subgraphs.get("accounts").unwrap().routing_url
                    == "http://accounts/graphql"
            })
            .times(1)
            .returning(|_, _, _| MockMyRouter::new());
        // second call, configuration is empty, we should take the URL from the graph
        router_factory
            .expect_create()
            .withf(|configuration, _schema, _previous_router| {
                configuration.subgraphs.get("accounts").unwrap().routing_url
                    == "http://localhost:4001/graphql"
            })
            .times(1)
            .returning(|_, _, _| MockMyRouter::new());
        let (server_factory, shutdown_receivers) = create_mock_server_factory(2);

        assert!(matches!(
            execute(
                server_factory,
                router_factory,
                vec![
                    UpdateConfiguration(
                        Configuration::builder()
                            .subgraphs(
                                [
                                    (
                                        "accounts".to_string(),
                                        Subgraph {
                                            routing_url: "http://accounts/graphql".to_string()
                                        }
                                    ),
                                ]
                                .iter()
                                .cloned()
                                .collect()
                            )
                            .build()
                            .boxed()
                    ),
                    UpdateSchema(Box::new(r#"
                        enum join__Graph {
                            ACCOUNTS @join__graph(name: "accounts" url: "http://localhost:4001/graphql")
                        }"#.parse().unwrap())),
                    UpdateConfiguration(
                            Configuration::builder()
                                .build()
                                .boxed()
                        ),
                    Shutdown,
                ],
                vec![
                    State::Startup,
                    State::Running {
                        address: SocketAddr::from_str("127.0.0.1:4000").unwrap(),
                        schema: r#"
                        enum join__Graph {
                            ACCOUNTS @join__graph(name: "accounts" url: "http://localhost:4001/graphql")
                        }"#.to_string()
                    },
                    State::Stopped
                ]
            )
            .await,
            Ok(()),
        ));
        assert_eq!(shutdown_receivers.lock().unwrap().len(), 2);
    }

    #[test(tokio::test)]
    async fn extract_routing_urls_when_updating_schema() {
        let mut router_factory = MockMyRouterFactory::new();
        // first call, we take the URL from the first supergraph
        router_factory
            .expect_create()
            .withf(|configuration, _schema, _previous_router| {
                configuration.subgraphs.get("accounts").unwrap().routing_url
                    == "http://accounts/graphql"
            })
            .times(1)
            .returning(|_, _, _| MockMyRouter::new());
        // second call, configuration is still empty, we should take the URL from the new supergraph
        router_factory
            .expect_create()
            .withf(|configuration, _schema, _previous_router| {
                println!("got configuration: {:#?}", configuration);
                configuration.subgraphs.get("accounts").unwrap().routing_url
                    == "http://localhost:4001/graphql"
            })
            .times(1)
            .returning(|_, _, _| MockMyRouter::new());
        let (server_factory, shutdown_receivers) = create_mock_server_factory(2);

        assert!(matches!(
            execute(
                server_factory,
                router_factory,
                vec![
                    UpdateConfiguration(
                        Configuration::builder()
                            .build()
                            .boxed()
                    ),
                    UpdateSchema(Box::new(r#"
                        enum join__Graph {
                            ACCOUNTS @join__graph(name: "accounts" url: "http://accounts/graphql")
                        }"#.parse().unwrap())),
                    UpdateSchema(Box::new(r#"
                        enum join__Graph {
                            ACCOUNTS @join__graph(name: "accounts" url: "http://localhost:4001/graphql")
                        }"#.parse().unwrap())),
                    Shutdown,
                ],
                vec![
                    State::Startup,
                    State::Running {
                        address: SocketAddr::from_str("127.0.0.1:4000").unwrap(),
                        schema: r#"
                        enum join__Graph {
                            ACCOUNTS @join__graph(name: "accounts" url: "http://accounts/graphql")
                        }"#.to_string()
                    },
                    State::Running {
                        address: SocketAddr::from_str("127.0.0.1:4000").unwrap(),
                        schema: r#"
                        enum join__Graph {
                            ACCOUNTS @join__graph(name: "accounts" url: "http://localhost:4001/graphql")
                        }"#.to_string()
                    },
                    State::Stopped
                ]
            )
            .await,
            Ok(()),
        ));
        assert_eq!(shutdown_receivers.lock().unwrap().len(), 2);
    }

    mock! {
        #[derive(Debug)]
        MyRouterFactory {}

        #[async_trait::async_trait]
        impl RouterFactory<MockMyRouter, MockMyRoute> for MyRouterFactory {
            async fn create(
                &self,
                configuration: &Configuration,
                schema: Arc<graphql::Schema>,
                previous_router: Option<Arc<MockMyRouter>>,
            ) -> MockMyRouter;
        }
    }

    mock! {
        #[derive(Debug)]
        MyFetcher {}

        #[async_trait::async_trait]
        impl graphql::Fetcher for MyFetcher {
            async fn stream(&self, request: graphql::Request) -> graphql::ResponseStream;
        }
    }

    mock! {
        #[derive(Debug)]
        MyRouter {}

        #[async_trait::async_trait]
        impl graphql::Router<MockMyRoute> for MyRouter {
            async fn prepare_query(
                &self,
                request: &graphql::Request,
            ) -> Result<MockMyRoute, graphql::ResponseStream>;
        }
    }

    mock! {
        #[derive(Debug)]
        MyRoute {}

        #[async_trait::async_trait]
        impl graphql::PreparedQuery for MyRoute {
            async fn execute(self, request: Arc<graphql::Request>) -> graphql::ResponseStream;
        }
    }

    async fn execute(
        server_factory: MockHttpServerFactory,
        router_factory: MockMyRouterFactory,
        events: Vec<Event>,
        expected_states: Vec<State>,
    ) -> Result<(), FederatedServerError> {
        let (state_listener, state_receiver) = mpsc::channel(100);
        let state_machine = StateMachine::new(server_factory, Some(state_listener), router_factory);
        let result = state_machine
            .process_events(stream::iter(events).boxed())
            .await;
        let states = state_receiver.collect::<Vec<State>>().await;
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
                move |_: Arc<MockMyRouter>,
                      configuration: Arc<Configuration>,
                      listener: Option<TcpListener>| {
                    let (shutdown_sender, shutdown_receiver) = oneshot::channel();
                    shutdown_receivers_clone
                        .lock()
                        .unwrap()
                        .push(shutdown_receiver);

                    let server = async move {
                        Ok(if let Some(l) = listener {
                            l
                        } else {
                            tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap()
                        })
                    };

                    Box::pin(async move {
                        Ok(HttpServerHandle::new(
                            shutdown_sender,
                            Box::pin(server),
                            configuration.server.listen,
                        ))
                    })
                },
            );
        (server_factory, shutdown_receivers)
    }

    fn create_mock_router_factory(expect_times_called: usize) -> MockMyRouterFactory {
        let mut router_factory = MockMyRouterFactory::new();
        router_factory
            .expect_create()
            .times(expect_times_called)
            .returning(|_, _, _| MockMyRouter::new());
        router_factory
    }
}
