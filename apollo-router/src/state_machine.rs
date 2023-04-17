// With regards to ELv2 licensing, this entire file is license key functionality

use std::fmt::Debug;
use std::fmt::Formatter;
use std::sync::Arc;

use futures::prelude::*;
use tokio::sync::mpsc;
use tokio::sync::OwnedRwLockWriteGuard;
use tokio::sync::RwLock;
use ApolloRouterError::ServiceCreationError;
use Event::NoMoreConfiguration;
use Event::NoMoreEntitlement;
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
use crate::router::Event::UpdateEntitlement;
use crate::router_factory::RouterFactory;
use crate::router_factory::RouterSuperServiceFactory;
use crate::spec::Schema;
use crate::uplink::entitlement::EntitlementReport;
use crate::uplink::entitlement::EntitlementState;
use crate::uplink::entitlement::ENTITLEMENT_EXPIRED_URL;
use crate::ApolloRouterError::NoEntitlement;

#[derive(Default, Clone)]
pub(crate) struct ListenAddresses {
    pub(crate) graphql_listen_address: Option<ListenAddr>,
    pub(crate) extra_listen_addresses: Vec<ListenAddr>,
}

/// This state maintains private information that is not exposed to the user via state listener.
#[allow(clippy::large_enum_variant)]
enum State<FA: RouterSuperServiceFactory> {
    Startup {
        configuration: Option<Arc<Configuration>>,
        schema: Option<Arc<String>>,
        entitlement: Option<EntitlementState>,
        listen_addresses_guard: OwnedRwLockWriteGuard<ListenAddresses>,
    },
    Running {
        configuration: Arc<Configuration>,
        schema: Arc<String>,
        entitlement: EntitlementState,
        server_handle: Option<HttpServerHandle>,
        router_service_factory: FA::RouterFactory,
        all_connections_stopped_signal: mpsc::Receiver<()>,
    },
    Stopped,
    Errored(ApolloRouterError),
}

impl<FA: RouterSuperServiceFactory> Debug for State<FA> {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Startup { .. } => {
                write!(f, "Startup")
            }
            Running { .. } => {
                write!(f, "Running")
            }
            Stopped => {
                write!(f, "Stopped")
            }
            Errored(_) => {
                write!(f, "Errored")
            }
        }
    }
}

impl<FA: RouterSuperServiceFactory> State<FA> {
    async fn no_more_configuration(self) -> Self {
        match self {
            Startup {
                configuration: None,
                ..
            } => Errored(NoConfiguration),
            _ => self,
        }
    }

    async fn no_more_schema(self) -> Self {
        match self {
            Startup { schema: None, .. } => Errored(NoSchema),
            _ => self,
        }
    }

    async fn no_more_entitlement(self) -> Self {
        match self {
            Startup {
                entitlement: None, ..
            } => Errored(NoEntitlement),
            _ => self,
        }
    }

    async fn update_inputs<S>(
        mut self,
        state_machine: &mut StateMachine<S, FA>,
        new_schema: Option<Arc<String>>,
        new_configuration: Option<Arc<Configuration>>,
        new_entitlement: Option<EntitlementState>,
    ) -> Self
    where
        S: HttpServerFactory,
    {
        let mut new_state = None;
        match &mut self {
            Startup {
                schema,
                configuration,
                entitlement,
                listen_addresses_guard,
            } => {
                *schema = new_schema.or_else(|| schema.take());
                *configuration = new_configuration.or_else(|| configuration.take());
                *entitlement = new_entitlement.or_else(|| entitlement.take());

                if let (Some(schema), Some(configuration), Some(entitlement)) =
                    (schema, configuration, entitlement)
                {
                    new_state = Some(
                        Self::try_start(
                            state_machine,
                            &mut None,
                            None,
                            configuration.clone(),
                            schema.clone(),
                            *entitlement,
                            listen_addresses_guard,
                        )
                        .map_ok_or_else(Errored, |f| f)
                        .await,
                    );
                }
            }
            Running {
                schema,
                configuration,
                entitlement,
                server_handle,
                router_service_factory,
                all_connections_stopped_signal: _,
            } => {
                tracing::info!("reloading");

                if new_entitlement == Some(EntitlementState::Unentitled)
                    && *entitlement != EntitlementState::Unentitled
                {
                    // When we get an unentitled event, if we were entitled before then just carry on.
                    // This means that users can delete and then undelete their graphs in studio while having their routers continue to run.
                    tracing::debug!("loss of entitlement detected, ignoring");
                    return self;
                }

                // We update the running config. This is OK even in the case that the router could not reload as we always want to retain the latest information for when we try to reload next.
                // In the case of a failed reload the server handle is retained, which has the old config/schema/entitlements in.
                if let Some(new_configuration) = new_configuration {
                    *configuration = new_configuration;
                }
                if let Some(new_schema) = new_schema {
                    *schema = new_schema;
                }
                if let Some(new_entitlement) = new_entitlement {
                    *entitlement = new_entitlement;
                }

                let mut guard = state_machine.listen_addresses.clone().write_owned().await;
                new_state = match Self::try_start(
                    state_machine,
                    server_handle,
                    Some(router_service_factory),
                    configuration.clone(),
                    schema.clone(),
                    *entitlement,
                    &mut guard,
                )
                .await
                {
                    Ok(new_state) => {
                        tracing::info!("reload complete");
                        Some(new_state)
                    }
                    Err(e) => {
                        // If we encountered an error it may be fatal depending on if we consumed the server handle or not.
                        match server_handle {
                            None => {
                                tracing::error!("fatal error while trying to reload; {}", e);
                                Some(Errored(e))
                            }
                            Some(_) => {
                                tracing::info!("error while reloading, continuing with previous configuration; {}", e);
                                None
                            }
                        }
                    }
                }
            }
            _ => {}
        }

        new_state.unwrap_or(self)
    }

    async fn shutdown(self) -> Self {
        match self {
            Running {
                server_handle: Some(server_handle),
                mut all_connections_stopped_signal,
                ..
            } => {
                tracing::info!("shutting down");
                let state = server_handle
                    .shutdown()
                    .map_ok_or_else(Errored, |_| Stopped)
                    .await;
                //FIXME: we might want to set a timeout here
                let _ = all_connections_stopped_signal.recv().await;
                tracing::info!("all connections shut down");
                state
            }
            _ => Stopped,
        }
    }

    #[allow(clippy::too_many_arguments)]
    async fn try_start<S>(
        state_machine: &mut StateMachine<S, FA>,
        server_handle: &mut Option<HttpServerHandle>,
        previous_router_service_factory: Option<&FA::RouterFactory>,
        configuration: Arc<Configuration>,
        schema: Arc<String>,
        entitlement: EntitlementState,
        listen_addresses_guard: &mut OwnedRwLockWriteGuard<ListenAddresses>,
    ) -> Result<State<FA>, ApolloRouterError>
    where
        S: HttpServerFactory,
        FA: RouterSuperServiceFactory,
    {
        let parsed_schema = Arc::new(
            Schema::parse(&schema, &configuration, None)
                .map_err(|e| ServiceCreationError(e.to_string().into()))?,
        );

        // Check the entitlements
        let report = EntitlementReport::build(&configuration, &parsed_schema);

        match entitlement {
            EntitlementState::Entitled => {
                tracing::debug!("A valid Apollo entitlement has been detected.");
            }
            EntitlementState::EntitledWarn if report.uses_restricted_features() => {
                tracing::error!("Entitlement has expired. The Router will soon stop serving requests. In order to enable these features for a self-hosted instance of Apollo Router, the Router must be connected to a graph in GraphOS that provides an active entitlement for the following features:\n\n{}\n\nSee {ENTITLEMENT_EXPIRED_URL} for more information.", report);
            }
            EntitlementState::EntitledHalt if report.uses_restricted_features() => {
                tracing::error!("Entitlement has expired. The Router will no longer serve requests. In order to enable these features for a self-hosted instance of Apollo Router, the Router must be connected to a graph in GraphOS that provides an active entitlement for the following features:\n\n{}\n\nSee {ENTITLEMENT_EXPIRED_URL} for more information.", report);
            }
            EntitlementState::Unentitled if report.uses_restricted_features() => {
                // This is OSS, so fail to reload or start.
                if std::env::var("APOLLO_KEY").is_ok() && std::env::var("APOLLO_GRAPH_REF").is_ok()
                {
                    tracing::error!("Entitlement not found. In order to enable these features for a self-hosted instance of Apollo Router, the Router must be connected to a graph in GraphOS that provides an entitlement for the following features:\n\n{}\n\nSee {ENTITLEMENT_EXPIRED_URL} for more information.", report);
                } else {
                    tracing::error!("Not connected to GraphOS. In order to enable these features for a self-hosted instance of Apollo Router, the Router must be connected to a graph in GraphOS (using APOLLO_KEY and APOLLO_GRAPH_REF) that provides an entitlement for the following features:\n\n{}\n\nSee {ENTITLEMENT_EXPIRED_URL} for more information.", report);
                }

                return Err(ApolloRouterError::EntitlementViolation);
            }
            _ => {
                tracing::debug!("A valid Apollo entitlement was not detected. However, no restricted features are in use.");
            }
        }

        // If there are no restricted featured in use then the effective entitlement is Entitled as we don't need warn or halt behavior.
        let effective_entitlement = if !report.uses_restricted_features() {
            EntitlementState::Entitled
        } else {
            entitlement
        };

        let router_service_factory = state_machine
            .router_configurator
            .create(
                configuration.clone(),
                schema.to_string(),
                previous_router_service_factory,
                None,
            )
            .await
            .map_err(ServiceCreationError)?;

        // used to track if there are still in flight connections when shutting down
        let (all_connections_stopped_sender, all_connections_stopped_signal) =
            mpsc::channel::<()>(1);
        let web_endpoints = router_service_factory.web_endpoints();

        // The point of no return. We take the previous server handle.
        let server_handle = match server_handle.take() {
            None => {
                state_machine
                    .http_server_factory
                    .create(
                        router_service_factory.clone(),
                        configuration.clone(),
                        Default::default(),
                        Default::default(),
                        web_endpoints,
                        effective_entitlement,
                        all_connections_stopped_sender,
                    )
                    .await?
            }
            Some(server_handle) => {
                server_handle
                    .restart(
                        &state_machine.http_server_factory,
                        router_service_factory.clone(),
                        configuration.clone(),
                        web_endpoints,
                        effective_entitlement,
                    )
                    .await?
            }
        };

        listen_addresses_guard.extra_listen_addresses = server_handle.listen_addresses().to_vec();
        listen_addresses_guard.graphql_listen_address =
            server_handle.graphql_listen_address().clone();

        Ok(Running {
            configuration,
            schema,
            entitlement,
            server_handle: Some(server_handle),
            router_service_factory,
            all_connections_stopped_signal,
        })
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
    FA: RouterSuperServiceFactory,
{
    http_server_factory: S,
    router_configurator: FA,
    pub(crate) listen_addresses: Arc<RwLock<ListenAddresses>>,
    listen_addresses_guard: Option<OwnedRwLockWriteGuard<ListenAddresses>>,
}

impl<S, FA> StateMachine<S, FA>
where
    S: HttpServerFactory,
    FA: RouterSuperServiceFactory + Send,
    FA::RouterFactory: RouterFactory,
{
    pub(crate) fn new(http_server_factory: S, router_factory: FA) -> Self {
        // Listen address is created locked so that if a consumer tries to examine the listen address before the state machine has reached running state they are blocked.
        let listen_addresses: Arc<RwLock<ListenAddresses>> = Default::default();
        let listen_addresses_guard = Some(
            listen_addresses
                .clone()
                .try_write_owned()
                .expect("lock just created, qed"),
        );
        Self {
            http_server_factory,
            router_configurator: router_factory,
            listen_addresses,
            listen_addresses_guard,
        }
    }

    pub(crate) async fn process_events(
        mut self,
        mut messages: impl Stream<Item = Event> + Unpin,
    ) -> Result<(), ApolloRouterError> {
        tracing::debug!("starting");
        // The listen address guard is transferred to the startup state. It will get consumed when moving to running.
        let mut state: State<FA> = Startup {
            configuration: None,
            schema: None,
            entitlement: None,
            listen_addresses_guard: self
                .listen_addresses_guard
                .take()
                .expect("must have listen address guard"),
        };

        // Process all the events in turn until we get to error state or we run out of events.
        while let Some(event) = messages.next().await {
            let event_name = format!("{event:?}");
            let last_state = format!("{state:?}");
            state = match event {
                UpdateConfiguration(configuration) => {
                    state
                        .update_inputs(&mut self, None, Some(Arc::new(configuration)), None)
                        .await
                }
                NoMoreConfiguration => state.no_more_configuration().await,
                UpdateSchema(schema) => {
                    state
                        .update_inputs(&mut self, Some(Arc::new(schema)), None, None)
                        .await
                }
                NoMoreSchema => state.no_more_schema().await,
                UpdateEntitlement(entitlement) => {
                    state
                        .update_inputs(&mut self, None, None, Some(entitlement))
                        .await
                }
                NoMoreEntitlement => state.no_more_entitlement().await,
                Shutdown => state.shutdown().await,
            };
            tracing::debug!(
                "state machine event: {event_name}, transitioned from: {last_state} to: {state:?}"
            );

            // If we've errored then exit even if there are potentially more messages
            if matches!(&state, Errored(_)) {
                break;
            }
        }
        tracing::info!("stopped");

        match state {
            Stopped => Ok(()),
            Errored(err) => Err(err),
            _ => {
                panic!("must finish on stopped or errored state")
            }
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
    use serde_json::json;
    use test_log::test;
    use tower::BoxError;
    use tower::Service;

    use super::*;
    use crate::configuration::Homepage;
    use crate::http_server_factory::Listener;
    use crate::plugin::DynPlugin;
    use crate::router_factory::Endpoint;
    use crate::router_factory::RouterFactory;
    use crate::router_factory::RouterSuperServiceFactory;
    use crate::services::new_service::ServiceFactory;
    use crate::services::RouterRequest;
    use crate::services::RouterResponse;

    fn example_schema() -> String {
        include_str!("testdata/supergraph.graphql").to_owned()
    }

    macro_rules! assert_matches {
        // `()` indicates that the macro takes no argument.
        ($actual:expr, $pattern:pat) => {
            let result = $actual;
            if !matches!(result, $pattern) {
                panic!("got {:?} but expected {}", result, stringify!($pattern));
            }
        };
    }

    #[test(tokio::test)]
    async fn no_configuration() {
        let router_factory = create_mock_router_configurator(0);
        let (server_factory, _) = create_mock_server_factory(0);
        assert_matches!(
            execute(server_factory, router_factory, vec![NoMoreConfiguration],).await,
            Err(NoConfiguration)
        );
    }

    #[test(tokio::test)]
    async fn no_schema() {
        let router_factory = create_mock_router_configurator(0);
        let (server_factory, _) = create_mock_server_factory(0);
        assert_matches!(
            execute(server_factory, router_factory, vec![NoMoreSchema],).await,
            Err(NoSchema)
        );
    }

    #[test(tokio::test)]
    async fn no_entitlement() {
        let router_factory = create_mock_router_configurator(0);
        let (server_factory, _) = create_mock_server_factory(0);
        assert_matches!(
            execute(server_factory, router_factory, vec![NoMoreEntitlement],).await,
            Err(NoEntitlement)
        );
    }
    fn test_config_restricted() -> Configuration {
        let mut config = Configuration::builder().build().unwrap();
        config.validated_yaml =
            Some(json!({"plugins":{"experimental.restricted":{"enabled":true}}}));
        config
    }

    #[test(tokio::test)]
    async fn restricted_entitled() {
        let router_factory = create_mock_router_configurator(1);
        let (server_factory, shutdown_receivers) = create_mock_server_factory(1);

        assert_matches!(
            execute(
                server_factory,
                router_factory,
                vec![
                    UpdateConfiguration(test_config_restricted()),
                    UpdateSchema(example_schema()),
                    UpdateEntitlement(EntitlementState::Entitled),
                    Shutdown
                ],
            )
            .await,
            Ok(())
        );
        assert_eq!(shutdown_receivers.lock().unwrap().len(), 1);
    }

    #[test(tokio::test)]
    async fn restricted_entitled_halted() {
        let router_factory = create_mock_router_configurator(1);
        let (server_factory, shutdown_receivers) = create_mock_server_factory(1);

        assert_matches!(
            execute(
                server_factory,
                router_factory,
                vec![
                    UpdateConfiguration(test_config_restricted()),
                    UpdateSchema(example_schema()),
                    UpdateEntitlement(EntitlementState::EntitledHalt),
                    Shutdown
                ],
            )
            .await,
            Ok(())
        );
        assert_eq!(shutdown_receivers.lock().unwrap().len(), 1);
    }

    #[test(tokio::test)]
    async fn restricted_entitled_warn() {
        let router_factory = create_mock_router_configurator(1);
        let (server_factory, shutdown_receivers) = create_mock_server_factory(1);

        assert_matches!(
            execute(
                server_factory,
                router_factory,
                vec![
                    UpdateConfiguration(test_config_restricted()),
                    UpdateSchema(example_schema()),
                    UpdateEntitlement(EntitlementState::EntitledWarn),
                    Shutdown
                ],
            )
            .await,
            Ok(())
        );
        assert_eq!(shutdown_receivers.lock().unwrap().len(), 1);
    }

    #[test(tokio::test)]
    async fn restricted_entitled_unentitled() {
        let router_factory = create_mock_router_configurator(2);
        let (server_factory, shutdown_receivers) = create_mock_server_factory(2);

        // The unentitled event is dropped so we should get a reload
        assert_matches!(
            execute(
                server_factory,
                router_factory,
                vec![
                    UpdateConfiguration(test_config_restricted()),
                    UpdateSchema(example_schema()),
                    UpdateEntitlement(EntitlementState::Entitled),
                    UpdateEntitlement(EntitlementState::Unentitled),
                    UpdateConfiguration(test_config_restricted()),
                    Shutdown
                ],
            )
            .await,
            Ok(())
        );
        assert_eq!(shutdown_receivers.lock().unwrap().len(), 2);
    }

    #[test(tokio::test)]
    async fn restricted_unentitled() {
        let router_factory = create_mock_router_configurator(0);
        let (server_factory, shutdown_receivers) = create_mock_server_factory(0);

        assert_matches!(
            execute(
                server_factory,
                router_factory,
                vec![
                    UpdateConfiguration(test_config_restricted()),
                    UpdateSchema(example_schema()),
                    UpdateEntitlement(EntitlementState::Unentitled),
                    Shutdown
                ],
            )
            .await,
            Err(ApolloRouterError::EntitlementViolation)
        );
        assert_eq!(shutdown_receivers.lock().unwrap().len(), 0);
    }

    #[test(tokio::test)]
    async fn unrestricted_unentitled_restricted_entitled() {
        let router_factory = create_mock_router_configurator(2);
        let (server_factory, shutdown_receivers) = create_mock_server_factory(2);

        assert_matches!(
            execute(
                server_factory,
                router_factory,
                vec![
                    UpdateConfiguration(Configuration::builder().build().unwrap()),
                    UpdateSchema(example_schema()),
                    UpdateEntitlement(EntitlementState::Unentitled),
                    UpdateConfiguration(test_config_restricted()),
                    UpdateEntitlement(EntitlementState::Entitled),
                    Shutdown
                ],
            )
            .await,
            Ok(())
        );
        assert_eq!(shutdown_receivers.lock().unwrap().len(), 2);
    }

    #[test(tokio::test)]
    async fn listen_addresses_are_locked() {
        let router_factory = create_mock_router_configurator(0);
        let (server_factory, _) = create_mock_server_factory(0);
        let state_machine = StateMachine::new(server_factory, router_factory);
        assert!(state_machine.listen_addresses.try_read().is_err());
    }

    #[test(tokio::test)]
    async fn shutdown_during_startup() {
        let router_factory = create_mock_router_configurator(0);
        let (server_factory, _) = create_mock_server_factory(0);
        assert_matches!(
            execute(server_factory, router_factory, vec![Shutdown],).await,
            Ok(())
        );
    }

    #[test(tokio::test)]
    async fn startup_shutdown() {
        let router_factory = create_mock_router_configurator(1);
        let (server_factory, shutdown_receivers) = create_mock_server_factory(1);

        assert_matches!(
            execute(
                server_factory,
                router_factory,
                vec![
                    UpdateConfiguration(Configuration::builder().build().unwrap()),
                    UpdateSchema(example_schema()),
                    UpdateEntitlement(EntitlementState::default()),
                    Shutdown
                ],
            )
            .await,
            Ok(())
        );
        assert_eq!(shutdown_receivers.lock().unwrap().len(), 1);
    }

    #[test(tokio::test)]
    async fn startup_reload_schema() {
        let router_factory = create_mock_router_configurator(2);
        let (server_factory, shutdown_receivers) = create_mock_server_factory(2);
        let minimal_schema = include_str!("testdata/minimal_supergraph.graphql");
        assert_matches!(
            execute(
                server_factory,
                router_factory,
                vec![
                    UpdateConfiguration(Configuration::builder().build().unwrap()),
                    UpdateSchema(minimal_schema.to_owned()),
                    UpdateEntitlement(EntitlementState::default()),
                    UpdateSchema(example_schema()),
                    Shutdown
                ],
            )
            .await,
            Ok(())
        );
        assert_eq!(shutdown_receivers.lock().unwrap().len(), 2);
    }

    #[test(tokio::test)]
    async fn startup_reload_entitlement() {
        let router_factory = create_mock_router_configurator(2);
        let (server_factory, shutdown_receivers) = create_mock_server_factory(2);
        let minimal_schema = include_str!("testdata/minimal_supergraph.graphql");
        assert_matches!(
            execute(
                server_factory,
                router_factory,
                vec![
                    UpdateConfiguration(Configuration::builder().build().unwrap()),
                    UpdateSchema(minimal_schema.to_owned()),
                    UpdateEntitlement(EntitlementState::default()),
                    UpdateEntitlement(EntitlementState::default()),
                    Shutdown
                ],
            )
            .await,
            Ok(())
        );
        assert_eq!(shutdown_receivers.lock().unwrap().len(), 2);
    }

    #[test(tokio::test)]
    async fn startup_reload_configuration() {
        let router_factory = create_mock_router_configurator(2);
        let (server_factory, shutdown_receivers) = create_mock_server_factory(2);

        assert_matches!(
            execute(
                server_factory,
                router_factory,
                vec![
                    UpdateConfiguration(Configuration::builder().build().unwrap()),
                    UpdateSchema(example_schema()),
                    UpdateEntitlement(EntitlementState::default()),
                    UpdateConfiguration(
                        Configuration::builder()
                            .supergraph(
                                crate::configuration::Supergraph::builder()
                                    .listen(SocketAddr::from_str("127.0.0.1:4001").unwrap())
                                    .build()
                            )
                            .build()
                            .unwrap()
                    ),
                    Shutdown
                ],
            )
            .await,
            Ok(())
        );
        assert_eq!(shutdown_receivers.lock().unwrap().len(), 2);
    }

    #[test(tokio::test)]
    async fn extract_routing_urls() {
        let router_factory = create_mock_router_configurator(1);
        let (server_factory, shutdown_receivers) = create_mock_server_factory(1);

        assert_matches!(
            execute(
                server_factory,
                router_factory,
                vec![
                    UpdateConfiguration(Configuration::builder().build().unwrap()),
                    UpdateSchema(example_schema()),
                    UpdateEntitlement(EntitlementState::default()),
                    Shutdown
                ],
            )
            .await,
            Ok(())
        );
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

        assert_matches!(
            execute(
                server_factory,
                router_factory,
                vec![
                    UpdateConfiguration(Configuration::builder().build().unwrap()),
                    UpdateSchema(example_schema()),
                    UpdateEntitlement(EntitlementState::default()),
                ],
            )
            .await,
            Err(ApolloRouterError::ServiceCreationError(_))
        );
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

        assert_matches!(
            execute(
                server_factory,
                router_factory,
                vec![
                    UpdateConfiguration(Configuration::builder().build().unwrap()),
                    UpdateSchema(example_schema()),
                    UpdateEntitlement(EntitlementState::default()),
                    UpdateSchema(example_schema()),
                    Shutdown
                ],
            )
            .await,
            Ok(())
        );
        assert_eq!(shutdown_receivers.lock().unwrap().len(), 1);
    }

    #[test(tokio::test)]
    async fn router_factory_ok_error_restart() {
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
        router_factory
            .expect_create()
            .times(1)
            .in_sequence(&mut seq)
            .withf(|configuration, _, _, _| configuration.homepage.enabled)
            .returning(|_, _, _, _| {
                let mut router = MockMyRouterFactory::new();
                router.expect_clone().return_once(MockMyRouterFactory::new);
                router.expect_web_endpoints().returning(MultiMap::new);
                Ok(router)
            });

        let (server_factory, shutdown_receivers) = create_mock_server_factory(2);

        assert_matches!(
            execute(
                server_factory,
                router_factory,
                vec![
                    UpdateConfiguration(Configuration::builder().build().unwrap()),
                    UpdateSchema(example_schema()),
                    UpdateEntitlement(EntitlementState::default()),
                    UpdateConfiguration(
                        Configuration::builder()
                            .homepage(Homepage::builder().enabled(true).build())
                            .build()
                            .unwrap()
                    ),
                    UpdateSchema(example_schema()),
                    Shutdown
                ],
            )
            .await,
            Ok(())
        );
        assert_eq!(shutdown_receivers.lock().unwrap().len(), 2);
    }

    mock! {
        #[derive(Debug)]
        MyRouterConfigurator {}

        #[async_trait::async_trait]
        impl RouterSuperServiceFactory for MyRouterConfigurator {
            type RouterFactory = MockMyRouterFactory;

            async fn create<'a>(
                &'a mut self,
                configuration: Arc<Configuration>,
                schema: String,
                previous_router_service_factory: Option<&'a MockMyRouterFactory>,
                extra_plugins: Option<Vec<(String, Box<dyn DynPlugin>)>>,
            ) -> Result<MockMyRouterFactory, BoxError>;
        }
    }

    mock! {
        #[derive(Debug)]
        MyRouterFactory {}

        impl RouterFactory for MyRouterFactory {
            type RouterService = MockMyRouter;
            type Future = <Self::RouterService as Service<RouterRequest>>::Future;
            fn web_endpoints(&self) -> MultiMap<ListenAddr, Endpoint>;
        }
        impl ServiceFactory<RouterRequest> for MyRouterFactory {
            type Service = MockMyRouter;
            fn create(&self) -> MockMyRouter;
        }

        impl Clone for MyRouterFactory {
            fn clone(&self) -> MockMyRouterFactory;
        }
    }

    mock! {
        #[derive(Debug)]
        MyRouter {
            fn poll_ready(&mut self) -> Poll<Result<(), BoxError>>;
            fn service_call(&mut self, req: RouterRequest) -> <MockMyRouter as Service<RouterRequest>>::Future;
        }

        impl Clone for MyRouter {
            fn clone(&self) -> MockMyRouter;
        }
    }

    //mockall does not handle well the lifetime on Context
    impl Service<RouterRequest> for MockMyRouter {
        type Response = RouterResponse;
        type Error = BoxError;
        type Future = BoxFuture<'static, Result<Self::Response, Self::Error>>;

        fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), BoxError>> {
            self.poll_ready()
        }
        fn call(&mut self, req: RouterRequest) -> Self::Future {
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

            _entitlment: EntitlementState,
            _all_connections_stopped_sender: mpsc::Sender<()>,
        ) -> Self::Future
        where
            RF: RouterFactory,
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
        state_machine
            .process_events(stream::iter(events).boxed())
            .await
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

                    let (all_connections_stopped_sender, _) = mpsc::channel::<()>(1);

                    Ok(HttpServerHandle::new(
                        shutdown_sender,
                        Box::pin(server),
                        Some(configuration.supergraph.listen.clone()),
                        vec![],
                        all_connections_stopped_sender,
                    ))
                },
            );
        (server_factory, shutdown_receivers)
    }

    fn create_mock_router_configurator(expect_times_called: usize) -> MockMyRouterConfigurator {
        let mut router_factory = MockMyRouterConfigurator::new();

        router_factory
            .expect_create()
            .times(if expect_times_called > 1 {
                1
            } else {
                expect_times_called
            })
            .returning(move |_, _, _, _| {
                let mut router = MockMyRouterFactory::new();
                router.expect_clone().return_once(MockMyRouterFactory::new);
                router.expect_web_endpoints().returning(MultiMap::new);
                Ok(router)
            });

        // verify reloads have the last previous_router_service_factory parameter
        if expect_times_called > 0 {
            router_factory
                .expect_create()
                .times(expect_times_called - 1)
                .withf(
                    move |_configuration: &Arc<Configuration>,
                          _,
                          previous_router_service_factory: &Option<&MockMyRouterFactory>,
                          _extra_plugins: &Option<Vec<(String, Box<dyn DynPlugin>)>>| {
                        previous_router_service_factory.is_some()
                    },
                )
                .returning(move |_, _, _, _| {
                    let mut router = MockMyRouterFactory::new();
                    router.expect_clone().return_once(MockMyRouterFactory::new);
                    router.expect_web_endpoints().returning(MultiMap::new);
                    Ok(router)
                });
        }

        router_factory
    }
}
