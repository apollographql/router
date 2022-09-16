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
use crate::router_factory::SupergraphServiceConfigurator;
use crate::router_factory::SupergraphServiceFactory;
use crate::Schema;

/// This state maintains private information that is not exposed to the user via state listener.
#[derive(derivative::Derivative)]
#[derivative(Debug)]
#[allow(clippy::large_enum_variant)]
enum State<RS> {
    Startup {
        configuration: Option<Configuration>,
        schema: Option<String>,
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
    FA: SupergraphServiceConfigurator,
{
    http_server_factory: S,
    router_configurator: FA,

    // The reason we have extra_listen_adresses and extra_listen_addresses_guard is that on startup we want ensure that we update the listen_addresses before users can read the value.
    pub(crate) graphql_listen_address: Arc<RwLock<Option<ListenAddr>>>,
    pub(crate) extra_listen_adresses: Arc<RwLock<Vec<ListenAddr>>>,
    extra_listen_addresses_guard: Option<OwnedRwLockWriteGuard<Vec<ListenAddr>>>,
    graphql_listen_address_guard: Option<OwnedRwLockWriteGuard<Option<ListenAddr>>>,
}

impl<S, FA> StateMachine<S, FA>
where
    S: HttpServerFactory,
    FA: SupergraphServiceConfigurator + Send,
    FA::SupergraphServiceFactory: SupergraphServiceFactory,
{
    pub(crate) fn new(http_server_factory: S, router_factory: FA) -> Self {
        let graphql_ready = Arc::new(RwLock::new(None));
        let graphql_ready_guard = graphql_ready.clone().try_write_owned().expect("owned lock");
        let extra_ready = Arc::new(RwLock::new(Vec::new()));
        let extra_ready_guard = extra_ready.clone().try_write_owned().expect("owned lock");
        Self {
            http_server_factory,
            router_configurator: router_factory,
            graphql_listen_address: graphql_ready,
            graphql_listen_address_guard: Some(graphql_ready_guard),
            extra_listen_adresses: extra_ready,
            extra_listen_addresses_guard: Some(extra_ready_guard),
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
                        schema: Some(new_schema),
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
                        router_service_factory,
                        server_handle,
                    },
                    UpdateSchema(new_schema),
                ) => {
                    tracing::info!("reloading schema");
                    match Schema::parse(&new_schema, &configuration) {
                        Ok(new_schema) => self
                            .reload_server(
                                configuration,
                                schema,
                                router_service_factory,
                                server_handle,
                                None,
                                Some(Arc::new(new_schema)),
                            )
                            .await
                            .into_ok_or_err2(),
                        Err(e) => {
                            tracing::error!("could not parse schema: {:?}", e);
                            Running {
                                configuration,
                                schema,
                                router_service_factory,
                                server_handle,
                            }
                        }
                    }
                }

                // Running: Handle configuration updates
                (
                    Running {
                        configuration,
                        schema,
                        router_service_factory,
                        server_handle,
                    },
                    UpdateConfiguration(new_configuration),
                ) => {
                    tracing::info!("reloading configuration");
                    if let Err(e) = configuration.is_compatible(&new_configuration) {
                        tracing::error!("could not reload configuration: {e}");

                        Running {
                            configuration,
                            schema,
                            router_service_factory,
                            server_handle,
                        }
                    } else {
                        self.reload_server(
                            configuration,
                            schema,
                            router_service_factory,
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
                }

                // Anything else we don't care about
                (state, message) => {
                    tracing::debug!("ignoring message transition {:?}", message);
                    state
                }
            };

            tracing::trace!("transitioned to {}", &new_state);
            state = new_state;

            // If we're running then let those waiting proceed.
            self.maybe_update_listen_addresses(&mut state).await;

            // If we've errored then exit even if there are potentially more messages
            if matches!(&state, Errored(_)) {
                break;
            }
        }
        tracing::debug!("stopped");

        // If the listen_address_guard has not been taken,
        // take it so that anything waiting on listen_address will proceed.
        self.extra_listen_addresses_guard.take();
        self.graphql_listen_address_guard.take();

        match state {
            Stopped => Ok(()),
            Errored(err) => Err(err),
            _ => {
                panic!("must finish on stopped or errored state")
            }
        }
    }

    async fn maybe_update_listen_addresses(
        &mut self,
        state: &mut State<<FA as SupergraphServiceConfigurator>::SupergraphServiceFactory>,
    ) {
        let (graphql_listen_address, extra_listen_addresses) =
            if let Running { server_handle, .. } = &state {
                let listen_addresses = server_handle.listen_addresses().to_vec();
                let graphql_listen_address = server_handle.graphql_listen_address().clone();
                (graphql_listen_address, listen_addresses)
            } else {
                return;
            };

        if let Some(mut listen_address_guard) = self.graphql_listen_address_guard.take() {
            *listen_address_guard = graphql_listen_address;
        } else {
            *self.graphql_listen_address.write().await = graphql_listen_address;
        }

        if let Some(mut extra_listen_addresses_guard) = self.extra_listen_addresses_guard.take() {
            *extra_listen_addresses_guard = extra_listen_addresses;
        } else {
            *self.extra_listen_adresses.write().await = extra_listen_addresses;
        }
    }

    async fn maybe_transition_to_running(
        &mut self,
        state: State<<FA as SupergraphServiceConfigurator>::SupergraphServiceFactory>,
    ) -> Result<
        State<<FA as SupergraphServiceConfigurator>::SupergraphServiceFactory>,
        State<<FA as SupergraphServiceConfigurator>::SupergraphServiceFactory>,
    > {
        if let Startup {
            configuration: Some(configuration),
            schema: Some(schema),
        } = state
        {
            let schema = match Schema::parse(&schema, &configuration) {
                Ok(schema) => schema,
                Err(e) => {
                    tracing::error!("could not parse schema: {:?}", e);
                    return Ok(Startup {
                        configuration: Some(configuration),
                        schema: None,
                    });
                }
            };
            tracing::debug!("starting http");
            let configuration = Arc::new(configuration);
            let schema = Arc::new(schema);

            let router_factory = self
                .router_configurator
                .create(configuration.clone(), schema.clone(), None, None)
                .await
                .map_err(|err| {
                    tracing::error!("cannot create the router: {}", err);
                    Errored(ApolloRouterError::ServiceCreationError(err))
                })?;

            let web_endpoints = router_factory.web_endpoints();

            let server_handle = self
                .http_server_factory
                .create(
                    router_factory.clone(),
                    configuration.clone(),
                    Default::default(),
                    Default::default(),
                    web_endpoints,
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
        router_service: <FA as SupergraphServiceConfigurator>::SupergraphServiceFactory,
        server_handle: HttpServerHandle,
        new_configuration: Option<Arc<Configuration>>,
        new_schema: Option<Arc<Schema>>,
    ) -> Result<
        State<<FA as SupergraphServiceConfigurator>::SupergraphServiceFactory>,
        State<<FA as SupergraphServiceConfigurator>::SupergraphServiceFactory>,
    > {
        let new_schema = new_schema.unwrap_or_else(|| schema.clone());
        let new_configuration = new_configuration.unwrap_or_else(|| configuration.clone());

        match self
            .router_configurator
            .create(
                new_configuration.clone(),
                new_schema.clone(),
                Some(&router_service),
                None,
            )
            .await
        {
            Ok(new_router_service) => {
                let web_endpoints = new_router_service.web_endpoints();

                let server_handle = server_handle
                    .restart(
                        &self.http_server_factory,
                        new_router_service.clone(),
                        new_configuration.clone(),
                        web_endpoints,
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
    use std::net::SocketAddr;
    use std::pin::Pin;
    use std::str::FromStr;
    use std::sync::Mutex;
    use std::task::Context;
    use std::task::Poll;

    use futures::channel::oneshot;
    use futures::future::BoxFuture;
    use mockall::mock;
    use mockall::Sequence;
    use multimap::MultiMap;
    use test_log::test;
    use tower::BoxError;
    use tower::Service;

    use super::*;
    use crate::graphql;
    use crate::http_server_factory::Listener;
    use crate::plugin::DynPlugin;
    use crate::router_factory::Endpoint;
    use crate::router_factory::SupergraphServiceConfigurator;
    use crate::router_factory::SupergraphServiceFactory;
    use crate::services::new_service::NewService;

    fn example_schema() -> String {
        include_str!("testdata/supergraph.graphql").to_owned()
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
                    UpdateConfiguration(Configuration::builder().build().unwrap().boxed()),
                    UpdateSchema(example_schema()),
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
                    UpdateConfiguration(Configuration::builder().build().unwrap().boxed()),
                    UpdateSchema(minimal_schema.to_owned()),
                    UpdateSchema(example_schema()),
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
                    UpdateConfiguration(Configuration::builder().build().unwrap().boxed()),
                    UpdateSchema(example_schema()),
                    UpdateConfiguration(
                        Configuration::builder()
                            .supergraph(
                                crate::configuration::Supergraph::builder()
                                    .listen(SocketAddr::from_str("127.0.0.1:4001").unwrap())
                                    .build()
                            )
                            .build()
                            .unwrap()
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
                    UpdateConfiguration(Configuration::builder().build().unwrap().boxed()),
                    UpdateSchema(example_schema()),
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
            .returning(|_, _, _, _| Err(BoxError::from("Error")));

        let (server_factory, shutdown_receivers) = create_mock_server_factory(0);

        assert!(matches!(
            execute(
                server_factory,
                router_factory,
                vec![
                    UpdateConfiguration(Configuration::builder().build().unwrap().boxed()),
                    UpdateSchema(example_schema()),
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
            .returning(|_, _, _, _| {
                let mut router = MockMyRouterFactory::new();
                router.expect_clone().return_once(MockMyRouterFactory::new);
                router.expect_web_endpoints().returning(MultiMap::new);
                Ok(router)
            });
        router_factory
            .expect_create()
            .times(1)
            .in_sequence(&mut seq)
            .returning(|_, _, _, _| Err(BoxError::from("error")));

        let (server_factory, shutdown_receivers) = create_mock_server_factory(1);

        assert!(matches!(
            execute(
                server_factory,
                router_factory,
                vec![
                    UpdateConfiguration(Configuration::builder().build().unwrap().boxed()),
                    UpdateSchema(example_schema()),
                    UpdateSchema(example_schema()),
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
        impl SupergraphServiceConfigurator for MyRouterConfigurator {
            type SupergraphServiceFactory = MockMyRouterFactory;

            async fn create<'a>(
                &'a mut self,
                configuration: Arc<Configuration>,
                schema: Arc<crate::Schema>,
                previous_router: Option<&'a MockMyRouterFactory>,
                extra_plugins: Option<Vec<(String, Box<dyn DynPlugin>)>>,
            ) -> Result<MockMyRouterFactory, BoxError>;
        }
    }

    mock! {
        #[derive(Debug)]
        MyRouterFactory {}

        impl SupergraphServiceFactory for MyRouterFactory {
            type SupergraphService = MockMyRouter;
            type Future = <Self::SupergraphService as Service<http::Request<graphql::Request>>>::Future;
            fn web_endpoints(&self) -> MultiMap<ListenAddr, Endpoint>;
        }
        impl  NewService<http::Request<graphql::Request>> for MyRouterFactory {
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
            fn service_call(&mut self, req: http::Request<crate::graphql::Request>) -> <MockMyRouter as Service<http::Request<crate::graphql::Request>>>::Future;
        }

        impl Clone for MyRouter {
            fn clone(&self) -> MockMyRouter;
        }
    }

    //mockall does not handle well the lifetime on Context
    impl Service<http::Request<crate::graphql::Request>> for MockMyRouter {
        type Response = http::Response<graphql::ResponseStream>;
        type Error = BoxError;
        type Future = BoxFuture<'static, Result<Self::Response, Self::Error>>;

        fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), BoxError>> {
            self.poll_ready()
        }
        fn call(&mut self, req: http::Request<crate::graphql::Request>) -> Self::Future {
            self.service_call(req)
        }
    }

    mock! {
        #[derive(Debug)]
        MyHttpServerFactory{
            fn create_server(&self,
                configuration: Arc<Configuration>,
                main_listener: Option<Listener>,) -> Result<HttpServerHandle, ApolloRouterError>;
        }
    }

    impl HttpServerFactory for MockMyHttpServerFactory {
        type Future =
            Pin<Box<dyn Future<Output = Result<HttpServerHandle, ApolloRouterError>> + Send>>;

        fn create<RF>(
            &self,
            _service_factory: RF,
            configuration: Arc<Configuration>,
            main_listener: Option<Listener>,
            _extra_listeners: Vec<(ListenAddr, Listener)>,
            _web_endpoints: MultiMap<ListenAddr, Endpoint>,
        ) -> Self::Future
        where
            RF: SupergraphServiceFactory,
        {
            let res = self.create_server(configuration, main_listener);
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
                move |configuration: Arc<Configuration>, mut main_listener: Option<Listener>| {
                    let (shutdown_sender, shutdown_receiver) = oneshot::channel();
                    shutdown_receivers_clone
                        .lock()
                        .unwrap()
                        .push(shutdown_receiver);

                    let server = async move {
                        let main_listener = match main_listener.take() {
                            Some(l) => l,
                            None => Listener::Tcp(
                                tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap(),
                            ),
                        };

                        Ok((main_listener, vec![]))
                    };

                    Ok(HttpServerHandle::new(
                        shutdown_sender,
                        Box::pin(server),
                        Some(configuration.supergraph.listen.clone()),
                        vec![],
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
            .returning(move |_, _, _, _| {
                let mut router = MockMyRouterFactory::new();
                router.expect_clone().return_once(MockMyRouterFactory::new);
                router.expect_web_endpoints().returning(MultiMap::new);
                Ok(router)
            });
        router_factory
    }
}
