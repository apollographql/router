use std::fmt::Display;
use std::fmt::Formatter;
use std::sync::Arc;

use futures::prelude::*;
use tokio::sync::OwnedRwLockWriteGuard;
use tokio::sync::RwLock;
use Event::NoMoreConfiguration;
use Event::NoMoreSchema;
use Event::Shutdown;

use super::http_server_factory::HttpServerFactory;
use super::http_server_factory::HttpServerHandle;
use super::router::ApolloRouterError::NoConfiguration;
use super::router::ApolloRouterError::NoSchema;
use super::router::ApolloRouterError::{self};
use super::router::Event::UpdateConfiguration;
use super::router::Event::UpdateSchema;
use super::router::Event::{self};
use super::state_machine::State::Errored;
use super::state_machine::State::Running;
use super::state_machine::State::Startup;
use super::state_machine::State::Stopped;
use crate::configuration::Configuration;
use crate::configuration::ListenAddr;
use crate::router_factory::RouterServiceConfigurator;
use crate::router_factory::RouterServiceFactory;
use crate::Schema;

/// This state maintains private information that is not exposed to the user via state listener.
#[derive(derivative::Derivative)]
#[derivative(Debug)]
#[allow(clippy::large_enum_variant)]
enum State<RS> {
    Startup {
        configuration: Option<Configuration>,
        schema: Option<Schema>,
    },
    Running {
        configuration: Arc<Configuration>,
        schema: Arc<Schema>,
        #[derivative(Debug = "ignore")]
        router_service_factory: RS,
        server_handle: HttpServerHandle,
    },
    Stopped,
    Errored(ApolloRouterError),
}

impl<T> Display for State<T> {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Startup { .. } => write!(f, "startup"),
            Running { .. } => write!(f, "running"),
            Stopped => write!(f, "stopped"),
            Errored { .. } => write!(f, "errored"),
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
    FA: RouterServiceConfigurator,
{
    http_server_factory: S,
    router_configurator: FA,

    // The reason we have listen_address and listen_address_guard is that on startup we want ensure that we update the listen address before users can read the value.
    pub(crate) listen_address: Arc<RwLock<Option<ListenAddr>>>,
    listen_address_guard: Option<OwnedRwLockWriteGuard<Option<ListenAddr>>>,
}

impl<S, FA> StateMachine<S, FA>
where
    S: HttpServerFactory,
    FA: RouterServiceConfigurator + Send,
    FA::RouterServiceFactory: RouterServiceFactory,
{
    pub(crate) fn new(http_server_factory: S, router_factory: FA) -> Self {
        let ready = Arc::new(RwLock::new(None));
        let ready_guard = ready.clone().try_write_owned().expect("owned lock");
        Self {
            http_server_factory,
            router_configurator: router_factory,
            listen_address: ready,
            listen_address_guard: Some(ready_guard),
        }
    }

    pub(crate) async fn process_events(
        mut self,
        mut messages: impl Stream<Item = Event> + Unpin,
    ) -> Result<(), ApolloRouterError> {
        tracing::debug!("starting");
        let mut state = Startup {
            configuration: None,
            schema: None,
        };
        while let Some(message) = messages.next().await {
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
                        router_service_factory: router_service,
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
                        router_service_factory: router_service,
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

            tracing::info!("transitioned to {}", &new_state);
            state = new_state;

            // If we're running then let those waiting proceed.
            self.maybe_update_listen_address(&mut state).await;

            // If we've errored then exit even if there are potentially more messages
            if matches!(&state, Errored(_)) {
                break;
            }
        }
        tracing::debug!("stopped");

        // If the listen_address_guard has not been taken, take it so that anything waiting on listen_address will proceed.
        self.listen_address_guard.take();

        match state {
            Stopped => Ok(()),
            Errored(err) => Err(err),
            _ => {
                panic!("must finish on stopped or errored state")
            }
        }
    }

    async fn maybe_update_listen_address(
        &mut self,
        state: &mut State<<FA as RouterServiceConfigurator>::RouterServiceFactory>,
    ) {
        let listen_address = if let Running { server_handle, .. } = &state {
            let listen_address = server_handle.listen_address().clone();
            Some(listen_address)
        } else {
            None
        };

        if let Some(listen_address) = listen_address {
            if let Some(mut listen_address_guard) = self.listen_address_guard.take() {
                *listen_address_guard = Some(listen_address);
            } else {
                *self.listen_address.write().await = Some(listen_address);
            }
        }
    }

    async fn maybe_transition_to_running(
        &mut self,
        state: State<<FA as RouterServiceConfigurator>::RouterServiceFactory>,
    ) -> Result<
        State<<FA as RouterServiceConfigurator>::RouterServiceFactory>,
        State<<FA as RouterServiceConfigurator>::RouterServiceFactory>,
    > {
        if let Startup {
            configuration: Some(configuration),
            schema: Some(schema),
        } = state
        {
            tracing::debug!("starting http");
            let configuration = Arc::new(configuration);
            let schema = Arc::new(schema);

            let router_factory = self
                .router_configurator
                .create(configuration.clone(), schema.clone(), None)
                .await
                .map_err(|err| {
                    tracing::error!("cannot create the router: {}", err);
                    Errored(ApolloRouterError::ServiceCreationError(err))
                })?;
            let plugin_handlers = router_factory.custom_endpoints();

            let server_handle = self
                .http_server_factory
                .create(
                    router_factory.clone(),
                    configuration.clone(),
                    None,
                    plugin_handlers,
                )
                .await
                .map_err(|err| {
                    tracing::error!("cannot start the router: {}", err);
                    Errored(err)
                })?;

            Ok(Running {
                configuration,
                schema,
                router_service_factory: router_factory,
                server_handle,
            })
        } else {
            Ok(state)
        }
    }

    #[allow(clippy::too_many_arguments)]
    async fn reload_server(
        &mut self,
        configuration: Arc<Configuration>,
        schema: Arc<Schema>,
        router_service: <FA as RouterServiceConfigurator>::RouterServiceFactory,
        server_handle: HttpServerHandle,
        new_configuration: Option<Arc<Configuration>>,
        new_schema: Option<Arc<Schema>>,
    ) -> Result<
        State<<FA as RouterServiceConfigurator>::RouterServiceFactory>,
        State<<FA as RouterServiceConfigurator>::RouterServiceFactory>,
    > {
        let new_schema = new_schema.unwrap_or_else(|| schema.clone());
        let new_configuration = new_configuration.unwrap_or_else(|| configuration.clone());

        match self
            .router_configurator
            .create(
                new_configuration.clone(),
                new_schema.clone(),
                Some(&router_service),
            )
            .await
        {
            Ok(new_router_service) => {
                let plugin_handlers = new_router_service.custom_endpoints();

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
                    router_service_factory: new_router_service,
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
                    router_service_factory: router_service,
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
    use std::collections::HashMap;
    use std::net::SocketAddr;
    use std::pin::Pin;
    use std::str::FromStr;
    use std::sync::Mutex;
    use std::task::Context;
    use std::task::Poll;

    use futures::channel::oneshot;
    use futures::future::BoxFuture;
    use futures::stream::BoxStream;
    use mockall::mock;
    use mockall::Sequence;
    use test_log::test;
    use tower::BoxError;
    use tower::Service;

    use super::*;
    use crate::graphql;
    use crate::http_ext::Request;
    use crate::http_ext::Response;
    use crate::http_server_factory::Listener;
    use crate::plugin::Handler;
    use crate::router_factory::RouterServiceConfigurator;
    use crate::router_factory::RouterServiceFactory;
    use crate::services::new_service::NewService;

    fn example_schema() -> Schema {
        include_str!("testdata/supergraph.graphql").parse().unwrap()
    }

    #[test(tokio::test)]
    async fn no_configuration() {
        let router_factory = create_mock_router_configurator(0);
        let (server_factory, _) = create_mock_server_factory(0);
        assert!(matches!(
            execute(server_factory, router_factory, vec![NoMoreConfiguration],).await,
            Err(NoConfiguration),
        ));
    }

    #[test(tokio::test)]
    async fn no_schema() {
        let router_factory = create_mock_router_configurator(0);
        let (server_factory, _) = create_mock_server_factory(0);
        assert!(matches!(
            execute(server_factory, router_factory, vec![NoMoreSchema],).await,
            Err(NoSchema),
        ));
    }

    #[test(tokio::test)]
    async fn shutdown_during_startup() {
        let router_factory = create_mock_router_configurator(0);
        let (server_factory, _) = create_mock_server_factory(0);
        assert!(matches!(
            execute(server_factory, router_factory, vec![Shutdown],).await,
            Ok(()),
        ));
    }

    #[test(tokio::test)]
    async fn startup_shutdown() {
        let router_factory = create_mock_router_configurator(1);
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
            )
            .await,
            Ok(()),
        ));
        assert_eq!(shutdown_receivers.lock().unwrap().len(), 1);
    }

    #[test(tokio::test)]
    async fn startup_reload_schema() {
        let router_factory = create_mock_router_configurator(2);
        let (server_factory, shutdown_receivers) = create_mock_server_factory(2);
        let minimal_schema = include_str!("testdata/minimal_supergraph.graphql");
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
            )
            .await,
            Ok(()),
        ));
        assert_eq!(shutdown_receivers.lock().unwrap().len(), 2);
    }

    #[test(tokio::test)]
    async fn startup_reload_configuration() {
        let router_factory = create_mock_router_configurator(2);
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
            )
            .await,
            Ok(()),
        ));
        assert_eq!(shutdown_receivers.lock().unwrap().len(), 2);
    }

    #[test(tokio::test)]
    async fn extract_routing_urls() {
        let router_factory = create_mock_router_configurator(1);
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
            )
            .await,
            Ok(()),
        ));
        assert_eq!(shutdown_receivers.lock().unwrap().len(), 1);
    }

    #[test(tokio::test)]
    async fn router_factory_error_startup() {
        let mut router_factory = MockMyRouterConfigurator::new();
        router_factory
            .expect_create()
            .times(1)
            .returning(|_, _, _| Err(BoxError::from("Error")));

        let (server_factory, shutdown_receivers) = create_mock_server_factory(0);

        assert!(matches!(
            execute(
                server_factory,
                router_factory,
                vec![
                    UpdateConfiguration(Configuration::builder().build().boxed()),
                    UpdateSchema(Box::new(example_schema())),
                ],
            )
            .await,
            Err(ApolloRouterError::ServiceCreationError(_)),
        ));
        assert_eq!(shutdown_receivers.lock().unwrap().len(), 0);
    }

    #[test(tokio::test)]
    async fn router_factory_error_restart() {
        let mut seq = Sequence::new();
        let mut router_factory = MockMyRouterConfigurator::new();
        router_factory
            .expect_create()
            .times(1)
            .in_sequence(&mut seq)
            .returning(|_, _, _| {
                let mut router = MockMyRouterFactory::new();
                router.expect_clone().return_once(MockMyRouterFactory::new);
                router.expect_custom_endpoints().returning(HashMap::new);
                Ok(router)
            });
        router_factory
            .expect_create()
            .times(1)
            .in_sequence(&mut seq)
            .returning(|_, _, _| Err(BoxError::from("error")));

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
            )
            .await,
            Ok(()),
        ));
        assert_eq!(shutdown_receivers.lock().unwrap().len(), 1);
    }

    mock! {
        #[derive(Debug)]
        MyRouterConfigurator {}

        #[async_trait::async_trait]
        impl RouterServiceConfigurator for MyRouterConfigurator {
            type RouterServiceFactory = MockMyRouterFactory;

            async fn create<'a>(
                &'a mut self,
                configuration: Arc<Configuration>,
                schema: Arc<crate::Schema>,
                previous_router: Option<&'a MockMyRouterFactory>,
            ) -> Result<MockMyRouterFactory, BoxError>;
        }
    }

    mock! {
        #[derive(Debug)]
        MyRouterFactory {}

        impl RouterServiceFactory for MyRouterFactory {
            type RouterService = MockMyRouter;
            type Future = <Self::RouterService as Service<Request<graphql::Request>>>::Future;
            fn custom_endpoints(&self) -> std::collections::HashMap<String, crate::plugin::Handler>;
        }
        impl  NewService<Request<graphql::Request>> for MyRouterFactory {
            type Service = MockMyRouter;
            fn new_service(&self) -> MockMyRouter;
        }

        impl Clone for MyRouterFactory {
            fn clone(&self) -> MockMyRouterFactory;
        }
    }

    mock! {
        #[derive(Debug)]
        MyRouter {
            fn poll_ready(&mut self) -> Poll<Result<(), BoxError>>;
            fn service_call(&mut self, req: Request<crate::graphql::Request>) -> <MockMyRouter as Service<Request<crate::graphql::Request>>>::Future;
        }

        impl Clone for MyRouter {
            fn clone(&self) -> MockMyRouter;
        }
    }

    //mockall does not handle well the lifetime on Context
    impl Service<Request<crate::graphql::Request>> for MockMyRouter {
        type Response = Response<BoxStream<'static, graphql::Response>>;
        type Error = BoxError;
        type Future = BoxFuture<'static, Result<Self::Response, Self::Error>>;

        fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), BoxError>> {
            self.poll_ready()
        }
        fn call(&mut self, req: Request<crate::graphql::Request>) -> Self::Future {
            self.service_call(req)
        }
    }

    mock! {
        #[derive(Debug)]
        MyHttpServerFactory{
            fn create_server(&self,
                configuration: Arc<Configuration>,
                listener: Option<Listener>,) -> Result<HttpServerHandle, ApolloRouterError>;
        }
    }

    impl HttpServerFactory for MockMyHttpServerFactory {
        type Future =
            Pin<Box<dyn Future<Output = Result<HttpServerHandle, ApolloRouterError>> + Send>>;

        fn create<RF>(
            &self,
            _service_factory: RF,
            configuration: Arc<Configuration>,
            listener: Option<Listener>,
            _plugin_handlers: HashMap<String, Handler>,
        ) -> Self::Future
        where
            RF: RouterServiceFactory,
        {
            let res = self.create_server(configuration, listener);
            Box::pin(async move { res })
        }
    }

    async fn execute(
        server_factory: MockMyHttpServerFactory,
        router_factory: MockMyRouterConfigurator,
        events: Vec<Event>,
    ) -> Result<(), ApolloRouterError> {
        let state_machine = StateMachine::new(server_factory, router_factory);
        let result = state_machine
            .process_events(stream::iter(events).boxed())
            .await;
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

    fn create_mock_router_configurator(expect_times_called: usize) -> MockMyRouterConfigurator {
        let mut router_factory = MockMyRouterConfigurator::new();

        router_factory
            .expect_create()
            .times(expect_times_called)
            .returning(move |_, _, _| {
                let mut router = MockMyRouterFactory::new();
                router.expect_clone().return_once(MockMyRouterFactory::new);
                router.expect_custom_endpoints().returning(HashMap::new);
                Ok(router)
            });
        router_factory
    }
}
