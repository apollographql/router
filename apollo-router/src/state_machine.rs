use super::http_server_factory::{HttpServerFactory, HttpServerHandle};
use super::router_factory::RouterServiceFactory;
use super::state_machine::PrivateState::{Errored, Running, Startup, Stopped};
use super::Event::{UpdateConfiguration, UpdateSchema};
use super::FederatedServerError::{NoConfiguration, NoSchema};
use super::{Event, FederatedServerError, State};
use crate::configuration::Configuration;
use apollo_router_core::prelude::*;
use futures::channel::mpsc;
use futures::prelude::*;
use std::sync::Arc;
use Event::{NoMoreConfiguration, NoMoreSchema, Shutdown};

/// This state maintains private information that is not exposed to the user via state listener.
#[derive(derivative::Derivative)]
#[derivative(Debug)]
#[allow(clippy::large_enum_variant)]
enum PrivateState<RS> {
    Startup {
        configuration: Option<Configuration>,
        schema: Option<graphql::Schema>,
    },
    Running {
        configuration: Arc<Configuration>,
        schema: Arc<graphql::Schema>,
        #[derivative(Debug = "ignore")]
        router: RS,
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
pub(crate) struct StateMachine<S, FA>
where
    S: HttpServerFactory,
    FA: RouterServiceFactory,
{
    http_server_factory: S,
    state_listener: Option<mpsc::Sender<State>>,
    router_factory: FA,
}

impl<RS> From<&PrivateState<RS>> for State {
    fn from(private_state: &PrivateState<RS>) -> Self {
        match private_state {
            Startup { .. } => State::Startup,
            Running {
                server_handle,
                schema,
                ..
            } => State::Running {
                address: server_handle.listen_address().to_owned(),
                schema: schema.as_str().to_string(),
            },
            Stopped => State::Stopped,
            Errored { .. } => State::Errored,
        }
    }
}

impl<S, FA> StateMachine<S, FA>
where
    S: HttpServerFactory,
    FA: RouterServiceFactory + Send,
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
        };
        let mut state_listener = self.state_listener.take();
        let initial_state = State::from(&state);
        <StateMachine<S, FA>>::notify_state_listener(&mut state_listener, initial_state).await;
        while let Some(message) = messages.next().await {
            let last_public_state = State::from(&state);
            let new_state = match (state, message) {
                // Startup: Handle configuration updates, maybe transition to running.
                (Startup { configuration, .. }, UpdateSchema(new_schema)) => {
                    self.maybe_transition_to_running(Startup {
                        configuration,
                        schema: Some(*new_schema),
                    })
                    .await
                }
                // Startup: Handle schema updates, maybe transition to running.
                (Startup { schema, .. }, UpdateConfiguration(new_configuration)) => {
                    self.maybe_transition_to_running(Startup {
                        configuration: Some(*new_configuration),
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
                    tracing::info!("Reloading schema");

                    let schema = Arc::new(*new_schema);
                    let router = self
                        .router_factory
                        .create(&configuration, Arc::clone(&schema), Some(router))
                        .await
                        .expect("TODO deal with errors");

                    match server_handle
                        .restart(
                            &self.http_server_factory,
                            router.clone(),
                            configuration.clone(),
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
                    let new_configuration = Arc::new(*new_configuration);
                    let router = self
                        .router_factory
                        .create(&new_configuration, Arc::clone(&schema), Some(router))
                        .await
                        .expect("TODO deal with errors");

                    match server_handle
                        .restart(
                            &self.http_server_factory,
                            router.clone(),
                            new_configuration.clone(),
                        )
                        .await
                    {
                        Ok(server_handle) => Running {
                            configuration: new_configuration,
                            schema,
                            router,
                            server_handle,
                        },
                        Err(err) => Errored(err),
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
                <StateMachine<S, FA>>::notify_state_listener(&mut state_listener, new_public_state)
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
        state: PrivateState<<FA as RouterServiceFactory>::RouterService>,
    ) -> PrivateState<<FA as RouterServiceFactory>::RouterService> {
        if let Startup {
            configuration: Some(configuration),
            schema: Some(schema),
        } = state
        {
            tracing::debug!("Starting http");
            let configuration_clone = Arc::new(configuration);
            let schema = Arc::new(schema);
            let router = self
                .router_factory
                .create(&configuration_clone, Arc::clone(&schema), None)
                .await
                .unwrap();

            match self
                .http_server_factory
                .create(router.clone(), configuration_clone.clone(), None)
                .await
            {
                Ok(server_handle) => {
                    tracing::debug!("Started on {}", server_handle.listen_address());

                    Running {
                        configuration: configuration_clone.clone(),
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
        } else {
            state
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::configuration::Subgraph;
    use crate::http_server_factory::Listener;
    use crate::router_factory::RouterServiceFactory;
    use futures::channel::oneshot;
    use futures::future::BoxFuture;
    use http::{Request, Response};
    use mockall::{mock, predicate::*};
    use std::net::SocketAddr;
    use std::pin::Pin;
    use std::str::FromStr;
    use std::sync::Mutex;
    use std::task::{Context, Poll};
    use test_log::test;
    use tower::{BoxError, Service};
    use url::Url;

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
                        address: SocketAddr::from_str("127.0.0.1:4000").unwrap().into(),
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
                        address: SocketAddr::from_str("127.0.0.1:4000").unwrap().into(),
                        schema: String::new()
                    },
                    State::Running {
                        address: SocketAddr::from_str("127.0.0.1:4000").unwrap().into(),
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
                        address: SocketAddr::from_str("127.0.0.1:4000").unwrap().into(),
                        schema: String::new()
                    },
                    State::Running {
                        address: SocketAddr::from_str("127.0.0.1:4001").unwrap().into(),
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
                                            routing_url: Url::parse("http://accounts/graphql").unwrap(),
                                            layers: Vec::new(),
                                        }
                                    ),
                                    (
                                        "products".to_string(),
                                        Subgraph {
                                            routing_url: Url::parse("http://accounts/graphql").unwrap(),
                                            layers: Vec::new(),
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
                        address: SocketAddr::from_str("127.0.0.1:4000").unwrap().into(),
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
    // TODO Extraction of routing URLS test needs to be removed
    // TODO The URLS for subrequest should be correct and not mapped in the subrequest service.
    // TODO Maybe these test can be removed completely
    // #[test(tokio::test)]
    // async fn extract_routing_urls_when_updating_configuration() {
    //     let mut router_factory = MockMyRouterFactory::new();
    //     // first call, we take the URL from the configuration
    //     router_factory
    //         .expect_create()
    //         .withf(|configuration, _schema, _previous_router| {
    //             configuration
    //                 .subgraphs
    //                 .get("accounts")
    //                 .unwrap()
    //                 .routing_url
    //                 .as_str()
    //                 == "http://accounts/graphql"
    //         })
    //         .times(1)
    //         .returning(|_, _, _| {
    //             let mut router = MockMyRouter::new();
    //
    //             router.expect_clone().return_once(MockMyRouter::new);
    //             Ok(router)
    //         });
    //     // second call, configuration is empty, we should take the URL from the graph
    //     router_factory
    //         .expect_create()
    //         .withf(|configuration, _schema, _previous_router| {
    //             configuration
    //                 .subgraphs
    //                 .get("accounts")
    //                 .unwrap()
    //                 .routing_url
    //                 .as_str()
    //                 == "http://localhost:4001/graphql"
    //         })
    //         .times(1)
    //         .returning(|_, _, _| {
    //             let mut router = MockMyRouter::new();
    //
    //             router.expect_clone().return_once(MockMyRouter::new);
    //             Ok(router)
    //         });
    //     let (server_factory, shutdown_receivers) = create_mock_server_factory(2);
    //
    //     assert!(matches!(
    //         execute(
    //             server_factory,
    //             router_factory,
    //             vec![
    //                 UpdateConfiguration(
    //                     Configuration::builder()
    //                         .subgraphs(
    //                             [
    //                                 (
    //                                     "accounts".to_string(),
    //                                     Subgraph {
    //                                         routing_url: Url::parse("http://accounts/graphql").unwrap(),
    //                                         layers: Vec::new(),
    //                                     }
    //                                 ),
    //                             ]
    //                             .iter()
    //                             .cloned()
    //                             .collect()
    //                         )
    //                         .build()
    //                         .boxed()
    //                 ),
    //                 UpdateSchema(Box::new(r#"
    //                     enum join__Graph {
    //                         ACCOUNTS @join__graph(name: "accounts" url: "http://localhost:4001/graphql")
    //                     }"#.parse().unwrap())),
    //                 UpdateConfiguration(
    //                         Configuration::builder()
    //                             .build()
    //                             .boxed()
    //                     ),
    //                 Shutdown,
    //             ],
    //             vec![
    //                 State::Startup,
    //                 State::Running {
    //                     address: SocketAddr::from_str("127.0.0.1:4000").unwrap().into(),
    //                     schema: r#"
    //                     enum join__Graph {
    //                         ACCOUNTS @join__graph(name: "accounts" url: "http://localhost:4001/graphql")
    //                     }"#.to_string()
    //                 },
    //                 State::Stopped
    //             ]
    //         )
    //         .await,
    //         Ok(()),
    //     ));
    //     assert_eq!(shutdown_receivers.lock().unwrap().len(), 2);
    // }
    //
    // #[test(tokio::test)]
    // async fn extract_routing_urls_when_updating_schema() {
    //     let mut router_factory = MockMyRouterFactory::new();
    //     // first call, we take the URL from the first supergraph
    //     router_factory
    //         .expect_create()
    //         .withf(|configuration, _schema, _previous_router| {
    //             configuration
    //                 .subgraphs
    //                 .get("accounts")
    //                 .unwrap()
    //                 .routing_url
    //                 .as_str()
    //                 == "http://accounts/graphql"
    //         })
    //         .times(1)
    //         .returning(|_, _, _| {
    //             let mut router = MockMyRouter::new();
    //
    //             router.expect_clone().return_once(MockMyRouter::new);
    //             Ok(router)
    //         });
    //     // second call, configuration is still empty, we should take the URL from the new supergraph
    //     router_factory
    //         .expect_create()
    //         .withf(|configuration, _schema, _previous_router| {
    //             println!("got configuration: {:#?}", configuration);
    //             configuration
    //                 .subgraphs
    //                 .get("accounts")
    //                 .unwrap()
    //                 .routing_url
    //                 .as_str()
    //                 == "http://localhost:4001/graphql"
    //         })
    //         .times(1)
    //         .returning(|_, _, _| {
    //             let mut router = MockMyRouter::new();
    //
    //             router.expect_clone().return_once(MockMyRouter::new);
    //             Ok(router)
    //         });
    //     let (server_factory, shutdown_receivers) = create_mock_server_factory(2);
    //
    //     assert!(matches!(
    //         execute(
    //             server_factory,
    //             router_factory,
    //             vec![
    //                 UpdateConfiguration(
    //                     Configuration::builder()
    //                         .build()
    //                         .boxed()
    //                 ),
    //                 UpdateSchema(Box::new(r#"
    //                     enum join__Graph {
    //                         ACCOUNTS @join__graph(name: "accounts" url: "http://accounts/graphql")
    //                     }"#.parse().unwrap())),
    //                 UpdateSchema(Box::new(r#"
    //                     enum join__Graph {
    //                         ACCOUNTS @join__graph(name: "accounts" url: "http://localhost:4001/graphql")
    //                     }"#.parse().unwrap())),
    //                 Shutdown,
    //             ],
    //             vec![
    //                 State::Startup,
    //                 State::Running {
    //                     address: SocketAddr::from_str("127.0.0.1:4000").unwrap().into(),
    //                     schema: r#"
    //                     enum join__Graph {
    //                         ACCOUNTS @join__graph(name: "accounts" url: "http://accounts/graphql")
    //                     }"#.to_string()
    //                 },
    //                 State::Running {
    //                     address: SocketAddr::from_str("127.0.0.1:4000").unwrap().into(),
    //                     schema: r#"
    //                     enum join__Graph {
    //                         ACCOUNTS @join__graph(name: "accounts" url: "http://localhost:4001/graphql")
    //                     }"#.to_string()
    //                 },
    //                 State::Stopped
    //             ]
    //         )
    //         .await,
    //         Ok(()),
    //     ));
    //     assert_eq!(shutdown_receivers.lock().unwrap().len(), 2);
    // }

    mock! {
        #[derive(Debug)]
        MyRouterFactory {}

        #[async_trait::async_trait]
        impl RouterServiceFactory for MyRouterFactory {
            type RouterService = MockMyRouter;
            type Future = <Self::RouterService as Service<Request<graphql::Request>>>::Future;

            async fn create(
                &self,
                configuration: &Configuration,
                schema: Arc<graphql::Schema>,
                previous_router: Option<MockMyRouter>,
            ) -> Result<MockMyRouter, BoxError>;
        }
    }

    mock! {
        #[derive(Debug)]
        MyRouter {
            fn poll_ready(&mut self) -> Poll<Result<(), BoxError>>;
            fn service_call(&mut self, req: Request<graphql::Request>) -> <MockMyRouter as Service<Request<graphql::Request>>>::Future;
        }

        impl Clone for MyRouter {
            fn clone(&self) -> MockMyRouter;
        }
    }

    //mockall does not handle well the lifetime on Context
    impl Service<Request<graphql::Request>> for MockMyRouter {
        type Response = Response<graphql::Response>;
        type Error = BoxError;
        type Future = BoxFuture<'static, Result<Self::Response, Self::Error>>;

        fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), BoxError>> {
            self.poll_ready()
        }
        fn call(&mut self, req: Request<graphql::Request>) -> Self::Future {
            self.service_call(req)
        }
    }

    mock! {
        #[derive(Debug)]
        MyHttpServerFactory{
            fn create_server(&self,
                configuration: Arc<Configuration>,
                listener: Option<Listener>,) -> Result<HttpServerHandle, FederatedServerError>;
        }
    }

    impl HttpServerFactory for MockMyHttpServerFactory {
        type Future =
            Pin<Box<dyn Future<Output = Result<HttpServerHandle, FederatedServerError>> + Send>>;

        fn create<RS>(
            &self,
            _service: RS,
            configuration: Arc<Configuration>,
            listener: Option<Listener>,
        ) -> Pin<Box<dyn Future<Output = Result<HttpServerHandle, FederatedServerError>> + Send>>
        where
            RS: Service<
                    Request<graphql::Request>,
                    Response = Response<graphql::Response>,
                    Error = BoxError,
                > + Send
                + Sync
                + Clone
                + 'static,
            <RS as Service<http::Request<apollo_router_core::Request>>>::Future: std::marker::Send,
        {
            let res = self.create_server(configuration, listener);
            Box::pin(async move { res })
        }
    }

    async fn execute(
        server_factory: MockMyHttpServerFactory,
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
        MockMyHttpServerFactory,
        Arc<Mutex<Vec<oneshot::Receiver<()>>>>,
    ) {
        let mut server_factory = MockMyHttpServerFactory::new();
        let shutdown_receivers = Arc::new(Mutex::new(vec![]));
        let shutdown_receivers_clone = shutdown_receivers.to_owned();
        server_factory
            .expect_create_server()
            .times(expect_times_called)
            .returning(
                move |configuration: Arc<Configuration>, listener: Option<Listener>| {
                    let (shutdown_sender, shutdown_receiver) = oneshot::channel();
                    shutdown_receivers_clone
                        .lock()
                        .unwrap()
                        .push(shutdown_receiver);

                    let server = async move {
                        Ok(if let Some(l) = listener {
                            l
                        } else {
                            #[cfg(unix)]
                            {
                                tokio_util::either::Either::Left(
                                    tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap(),
                                )
                            }
                            #[cfg(not(unix))]
                            {
                                tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap()
                            }
                        })
                    };

                    Ok(HttpServerHandle::new(
                        shutdown_sender,
                        Box::pin(server),
                        configuration.server.listen.clone(),
                    ))
                },
            );
        (server_factory, shutdown_receivers)
    }

    fn create_mock_router_factory(expect_times_called: usize) -> MockMyRouterFactory {
        let mut router_factory = MockMyRouterFactory::new();

        router_factory
            .expect_create()
            .times(expect_times_called)
            .returning(move |_, _, _| {
                let mut router = MockMyRouter::new();

                router.expect_clone().return_once(MockMyRouter::new);
                Ok(router)
            });
        router_factory
    }
}
