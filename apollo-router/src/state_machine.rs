use super::http_server_factory::{HttpServerFactory, HttpServerHandle};
use super::router_factory::RouterServiceFactory;
use super::state_machine::PrivateState::{Errored, Running, Startup, Stopped};
use super::Event::{UpdateConfiguration, UpdateSchema};
use super::FederatedServerError::{NoConfiguration, NoSchema};
use super::{Event, FederatedServerError, State};
use crate::configuration::Configuration;
use apollo_router_core::Schema;
use apollo_router_core::{prelude::*, Handler};
use futures::channel::mpsc;
use futures::prelude::*;
use std::collections::HashMap;
use std::fmt::{Display, Formatter};
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
        router_service: RS,
        server_handle: HttpServerHandle,
    },
    Stopped,
    Errored(FederatedServerError),
}

impl<T> Display for PrivateState<T> {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            PrivateState::Startup { .. } => write!(f, "startup"),
            PrivateState::Running { .. } => write!(f, "running"),
            PrivateState::Stopped => write!(f, "stopped"),
            PrivateState::Errored { .. } => write!(f, "errored"),
        }
    }
}

/// A state machine that responds to events to control the lifecycle of the server.
/// The server is in startup state until both configuration and schema are supplied.
/// If config and schema are not supplied then the machine ends with an error.
/// Once schema and config are obtained running state is entered.
/// Config and schema updates will try to swap in the new values into the running state. In future we may trigger an http server restart if for instance socket address is encountered.
/// At any point a shutdown event will cause the machine to try to get to stopped state.  
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
        tracing::debug!("starting");
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
                (Startup { configuration, .. }, UpdateSchema(new_schema)) => self
                    .maybe_transition_to_running(Startup {
                        configuration,
                        schema: Some(*new_schema),
                    })
                    .await
                    .into_ok_or_err2(),

                // Startup: Handle schema updates, maybe transition to running.
                (Startup { schema, .. }, UpdateConfiguration(new_configuration)) => self
                    .maybe_transition_to_running(Startup {
                        configuration: Some(*new_configuration),
                        schema,
                    })
                    .await
                    .into_ok_or_err2(),

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
                    tracing::debug!("shutting down");
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
                        router_service,
                        server_handle,
                    },
                    UpdateSchema(new_schema),
                ) => {
                    tracing::info!("reloading schema");
                    self.reload_server(
                        configuration,
                        schema,
                        router_service,
                        server_handle,
                        None,
                        Some(Arc::new(*new_schema)),
                    )
                    .await
                    .into_ok_or_err2()
                }

                // Running: Handle configuration updates
                (
                    Running {
                        configuration,
                        schema,
                        router_service,
                        server_handle,
                    },
                    UpdateConfiguration(new_configuration),
                ) => {
                    tracing::info!("reloading configuration");
                    self.reload_server(
                        configuration,
                        schema,
                        router_service,
                        server_handle,
                        Some(Arc::new(*new_configuration)),
                        None,
                    )
                    .await
                    .map(|s| {
                        tracing::info!("reloaded");
                        s
                    })
                    .into_ok_or_err2()
                }

                // Anything else we don't care about
                (state, message) => {
                    tracing::debug!("ignoring message transition {:?}", message);
                    state
                }
            };

            let new_public_state = State::from(&new_state);
            if last_public_state != new_public_state {
                <StateMachine<S, FA>>::notify_state_listener(&mut state_listener, new_public_state)
                    .await;
            }
            tracing::debug!("transitioned to state {}", &new_state);
            state = new_state;

            // If we've errored then exit even if there are potentially more messages
            if matches!(&state, Errored(_)) {
                break;
            }
        }
        tracing::debug!("stopped");

        match state {
            Stopped => Ok(()),
            Errored(err) => Err(err),
            _ => {
                panic!("must finish on stopped or errored state")
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
        &mut self,
        state: PrivateState<<FA as RouterServiceFactory>::RouterService>,
    ) -> Result<
        PrivateState<<FA as RouterServiceFactory>::RouterService>,
        PrivateState<<FA as RouterServiceFactory>::RouterService>,
    > {
        if let Startup {
            configuration: Some(configuration),
            schema: Some(schema),
        } = state
        {
            tracing::debug!("starting http");
            let configuration = Arc::new(configuration);
            let schema = Arc::new(schema);

            let router = self
                .router_factory
                .create(configuration.clone(), schema.clone(), None)
                .await
                .map_err(|err| {
                    tracing::error!("cannot create the router: {}", err);
                    Errored(FederatedServerError::ServiceCreationError(err))
                })?;

            let plugin_handlers: HashMap<String, Handler> = self
                .router_factory
                .plugins()
                .iter()
                .filter_map(|(plugin_name, plugin)| {
                    (plugin_name.starts_with("apollo.") || plugin_name.starts_with("experimental."))
                        .then(|| plugin.custom_endpoint())
                        .flatten()
                        .map(|h| (plugin_name.clone(), h))
                })
                .collect();

            let server_handle = self
                .http_server_factory
                .create(router.clone(), configuration.clone(), None, plugin_handlers)
                .await
                .map_err(|err| {
                    tracing::error!("cannot start the router: {}", err);
                    Errored(err)
                })?;

            Ok(Running {
                configuration,
                schema,
                router_service: router,
                server_handle,
            })
        } else {
            Ok(state)
        }
    }
    async fn reload_server(
        &mut self,
        configuration: Arc<Configuration>,
        schema: Arc<Schema>,
        router_service: <FA as RouterServiceFactory>::RouterService,
        server_handle: HttpServerHandle,
        new_configuration: Option<Arc<Configuration>>,
        new_schema: Option<Arc<Schema>>,
    ) -> Result<
        PrivateState<<FA as RouterServiceFactory>::RouterService>,
        PrivateState<<FA as RouterServiceFactory>::RouterService>,
    > {
        let new_schema = new_schema.unwrap_or_else(|| schema.clone());
        let new_configuration = new_configuration.unwrap_or_else(|| configuration.clone());

        match self
            .router_factory
            .create(
                new_configuration.clone(),
                new_schema.clone(),
                Some(&router_service),
            )
            .await
        {
            Ok(new_router_service) => {
                let plugin_handlers: HashMap<String, Handler> = self
                    .router_factory
                    .plugins()
                    .iter()
                    .filter_map(|(plugin_name, plugin)| {
                        (plugin_name.starts_with("apollo.")
                            || plugin_name.starts_with("experimental."))
                        .then(|| plugin.custom_endpoint())
                        .flatten()
                        .map(|handler| (plugin_name.clone(), handler))
                    })
                    .collect();

                let server_handle = server_handle
                    .restart(
                        &self.http_server_factory,
                        new_router_service.clone(),
                        new_configuration.clone(),
                        plugin_handlers,
                    )
                    .await
                    .map_err(|err| {
                        tracing::error!("cannot start the router: {}", err);
                        Errored(err)
                    })?;
                Ok(Running {
                    configuration: new_configuration,
                    schema: new_schema,
                    router_service: new_router_service,
                    server_handle,
                })
            }
            Err(err) => {
                tracing::error!(
                    "cannot create new router, keeping previous configuration: {}",
                    err
                );
                Err(Running {
                    configuration,
                    schema,
                    router_service,
                    server_handle,
                })
            }
        }
    }
}

trait ResultExt<T> {
    // Unstable method can be deleted in future
    fn into_ok_or_err2(self) -> T;
}

impl<T> ResultExt<T> for Result<T, T> {
    fn into_ok_or_err2(self) -> T {
        match self {
            Ok(v) => v,
            Err(v) => v,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http_server_factory::Listener;
    use crate::router_factory::RouterServiceFactory;
    use apollo_router_core::http_compat::{Request, Response};
    use apollo_router_core::{DynPlugin, ResponseBody};
    use futures::channel::oneshot;
    use futures::future::BoxFuture;
    use mockall::{mock, Sequence};
    use std::net::SocketAddr;
    use std::pin::Pin;
    use std::str::FromStr;
    use std::sync::Mutex;
    use std::task::{Context, Poll};
    use test_log::test;
    use tower::{BoxError, Service};

    fn example_schema() -> Schema {
        include_str!("testdata/supergraph.graphql").parse().unwrap()
    }

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
                    UpdateConfiguration(Configuration::builder().build().boxed()),
                    UpdateSchema(Box::new(example_schema())),
                    Shutdown
                ],
                vec![
                    State::Startup,
                    State::Running {
                        address: SocketAddr::from_str("127.0.0.1:4000").unwrap().into(),
                        schema: example_schema().as_str().to_string()
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
        let minimal_schema = r#"       
        type Query {
          me: String
        }"#;
        assert!(matches!(
            execute(
                server_factory,
                router_factory,
                vec![
                    UpdateConfiguration(Configuration::builder().build().boxed()),
                    UpdateSchema(Box::new(minimal_schema.parse().unwrap())),
                    UpdateSchema(Box::new(example_schema())),
                    Shutdown
                ],
                vec![
                    State::Startup,
                    State::Running {
                        address: SocketAddr::from_str("127.0.0.1:4000").unwrap().into(),
                        schema: minimal_schema.to_string()
                    },
                    State::Running {
                        address: SocketAddr::from_str("127.0.0.1:4000").unwrap().into(),
                        schema: example_schema().as_str().to_string(),
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
                    UpdateConfiguration(Configuration::builder().build().boxed()),
                    UpdateSchema(Box::new(example_schema())),
                    UpdateConfiguration(
                        Configuration::builder()
                            .server(
                                crate::configuration::Server::builder()
                                    .listen(SocketAddr::from_str("127.0.0.1:4001").unwrap())
                                    .build()
                            )
                            .build()
                            .boxed()
                    ),
                    Shutdown
                ],
                vec![
                    State::Startup,
                    State::Running {
                        address: SocketAddr::from_str("127.0.0.1:4000").unwrap().into(),
                        schema: example_schema().as_str().to_string()
                    },
                    State::Running {
                        address: SocketAddr::from_str("127.0.0.1:4001").unwrap().into(),
                        schema: example_schema().as_str().to_string()
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
                    UpdateConfiguration(Configuration::builder().build().boxed()),
                    UpdateSchema(Box::new(example_schema())),
                    Shutdown
                ],
                vec![
                    State::Startup,
                    State::Running {
                        address: SocketAddr::from_str("127.0.0.1:4000").unwrap().into(),
                        schema: example_schema().as_str().to_string()
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
    async fn router_factory_error_startup() {
        let mut router_factory = MockMyRouterFactory::new();
        router_factory
            .expect_create()
            .times(1)
            .returning(|_, _, _| Err(BoxError::from("Error")));

        router_factory
            .expect_plugins()
            .times(0)
            .return_const(Vec::new());
        let (server_factory, shutdown_receivers) = create_mock_server_factory(0);

        assert!(matches!(
            execute(
                server_factory,
                router_factory,
                vec![
                    UpdateConfiguration(Configuration::builder().build().boxed()),
                    UpdateSchema(Box::new(example_schema())),
                ],
                vec![State::Startup, State::Errored,]
            )
            .await,
            Err(FederatedServerError::ServiceCreationError(_)),
        ));
        assert_eq!(shutdown_receivers.lock().unwrap().len(), 0);
    }

    #[test(tokio::test)]
    async fn router_factory_error_restart() {
        let mut seq = Sequence::new();
        let mut router_factory = MockMyRouterFactory::new();
        router_factory
            .expect_create()
            .times(1)
            .in_sequence(&mut seq)
            .returning(|_, _, _| {
                let mut router = MockMyRouter::new();
                router.expect_clone().return_once(MockMyRouter::new);
                Ok(router)
            });
        router_factory
            .expect_create()
            .times(1)
            .in_sequence(&mut seq)
            .returning(|_, _, _| Err(BoxError::from("error")));

        router_factory
            .expect_plugins()
            .times(1)
            .return_const(Vec::new());
        let (server_factory, shutdown_receivers) = create_mock_server_factory(1);

        assert!(matches!(
            execute(
                server_factory,
                router_factory,
                vec![
                    UpdateConfiguration(Configuration::builder().build().boxed()),
                    UpdateSchema(Box::new(example_schema())),
                    UpdateSchema(Box::new(example_schema())),
                    Shutdown
                ],
                vec![
                    State::Startup,
                    State::Running {
                        address: SocketAddr::from_str("127.0.0.1:4000").unwrap().into(),
                        schema: example_schema().as_str().to_string()
                    },
                    State::Stopped
                ]
            )
            .await,
            Ok(()),
        ));
        assert_eq!(shutdown_receivers.lock().unwrap().len(), 1);
    }

    mock! {
        #[derive(Debug)]
        MyRouterFactory {}

        #[async_trait::async_trait]
        impl RouterServiceFactory for MyRouterFactory {
            type RouterService = MockMyRouter;
            type Future = <Self::RouterService as Service<Request<graphql::Request>>>::Future;

            async fn create<'a>(
                &'a mut self,
                configuration: Arc<Configuration>,
                schema: Arc<graphql::Schema>,
                previous_router: Option<&'a MockMyRouter>,
            ) -> Result<MockMyRouter, BoxError>;

            fn plugins(&self) -> &[(String, Box<dyn DynPlugin>)];
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
        type Response = Response<ResponseBody>;
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
            _plugin_handlers: HashMap<String, Handler>,
        ) -> Pin<Box<dyn Future<Output = Result<HttpServerHandle, FederatedServerError>> + Send>>
        where
            RS: Service<
                    Request<graphql::Request>,
                    Response = Response<ResponseBody>,
                    Error = BoxError,
                > + Send
                + Sync
                + Clone
                + 'static,
            <RS as Service<Request<apollo_router_core::Request>>>::Future: std::marker::Send,
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
                            Listener::Tcp(
                                tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap(),
                            )
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
            .expect_plugins()
            .times(expect_times_called)
            .return_const(Vec::new());
        router_factory
    }
}
