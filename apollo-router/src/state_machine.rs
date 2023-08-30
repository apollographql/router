use std::fmt::Debug;
use std::fmt::Formatter;
use std::sync::Arc;

use futures::prelude::*;
use tokio::sync::mpsc;
#[cfg(test)]
use tokio::sync::Notify;
use tokio::sync::OwnedRwLockWriteGuard;
use tokio::sync::RwLock;
use ApolloRouterError::ServiceCreationError;
use Event::NoMoreConfiguration;
use Event::NoMoreLicense;
use Event::NoMoreSchema;
use Event::Reload;
use Event::Shutdown;
use State::Errored;
use State::Running;
use State::Startup;
use State::Stopped;

use super::http_server_factory::HttpServerFactory;
use super::http_server_factory::HttpServerHandle;
use super::router::ApolloRouterError::NoConfiguration;
use super::router::ApolloRouterError::NoSchema;
use super::router::ApolloRouterError::{self};
use super::router::Event::UpdateConfiguration;
use super::router::Event::UpdateSchema;
use super::router::Event::{self};
use crate::configuration::metrics::Metrics;
use crate::configuration::metrics::MetricsHandle;
use crate::configuration::Configuration;
use crate::configuration::Discussed;
use crate::configuration::ListenAddr;
use crate::router::Event::UpdateLicense;
use crate::router_factory::RouterFactory;
use crate::router_factory::RouterSuperServiceFactory;
use crate::uplink::license_enforcement::LicenseEnforcementReport;
use crate::uplink::license_enforcement::LicenseState;
use crate::uplink::license_enforcement::LICENSE_EXPIRED_URL;
use crate::ApolloRouterError::NoLicense;

const STATE_CHANGE: &str = "state change";

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
        license: Option<LicenseState>,
        listen_addresses_guard: OwnedRwLockWriteGuard<ListenAddresses>,
    },
    Running {
        configuration: Arc<Configuration>,
        _metrics_handle: MetricsHandle,
        schema: Arc<String>,
        license: LicenseState,
        server_handle: Option<HttpServerHandle>,
        router_service_factory: FA::RouterFactory,
        all_connections_stopped_signals: Vec<mpsc::Receiver<()>>,
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

    async fn no_more_license(self) -> Self {
        match self {
            Startup { license: None, .. } => Errored(NoLicense),
            _ => self,
        }
    }

    async fn update_inputs<S>(
        mut self,
        state_machine: &mut StateMachine<S, FA>,
        new_schema: Option<Arc<String>>,
        new_configuration: Option<Arc<Configuration>>,
        new_license: Option<LicenseState>,
    ) -> Self
    where
        S: HttpServerFactory,
    {
        let mut new_state = None;
        match &mut self {
            Startup {
                schema,
                configuration,
                license,
                listen_addresses_guard,
            } => {
                *schema = new_schema.or_else(|| schema.take());
                *configuration = new_configuration.or_else(|| configuration.take());
                *license = new_license.or_else(|| license.take());

                if let (Some(schema), Some(configuration), Some(license)) =
                    (schema, configuration, license)
                {
                    new_state = Some(
                        Self::try_start(
                            state_machine,
                            &mut None,
                            None,
                            configuration.clone(),
                            schema.clone(),
                            *license,
                            listen_addresses_guard,
                            vec![],
                        )
                        .map_ok_or_else(Errored, |f| f)
                        .await,
                    );
                    if matches!(new_state, Some(Running { .. })) {
                        state_machine.http_server_factory.ready(true);
                    }
                }
            }
            Running {
                schema,
                configuration,
                license,
                server_handle,
                router_service_factory,
                all_connections_stopped_signals,
                ..
            } => {
                // When we get an unlicensed event, if we were licensed before then just carry on.
                // This means that users can delete and then undelete their graphs in studio while having their routers continue to run.
                if new_license == Some(LicenseState::Unlicensed)
                    && *license != LicenseState::Unlicensed
                {
                    tracing::info!(
                        event = STATE_CHANGE,
                        "ignoring reload because of loss of license"
                    );
                    return self;
                }

                // Have things actually changed?
                let (mut license_reload, mut schema_reload, mut configuration_reload) =
                    (false, false, false);
                if let Some(new_configuration) = new_configuration {
                    *configuration = new_configuration;
                    configuration_reload = true;
                }
                if let Some(new_schema) = new_schema {
                    if schema.as_ref() != new_schema.as_ref() {
                        *schema = new_schema;
                        schema_reload = true;
                    }
                }
                if let Some(new_license) = new_license {
                    if *license != new_license {
                        *license = new_license;
                        license_reload = true;
                    }
                }

                // Let users know we are about to process a state reload event
                tracing::info!(
                    new_schema = schema_reload,
                    new_license = license_reload,
                    new_configuration = configuration_reload,
                    event = STATE_CHANGE,
                    "processing event"
                );

                let need_reload = schema_reload || license_reload || configuration_reload;

                if need_reload {
                    // We update the running config. This is OK even in the case that the router could not reload as we always want to retain the latest information for when we try to reload next.
                    // In the case of a failed reload the server handle is retained, which has the old config/schema/license in.
                    let mut guard = state_machine.listen_addresses.clone().write_owned().await;
                    let signals = std::mem::take(all_connections_stopped_signals);
                    new_state = match Self::try_start(
                        state_machine,
                        server_handle,
                        Some(router_service_factory),
                        configuration.clone(),
                        schema.clone(),
                        *license,
                        &mut guard,
                        signals,
                    )
                    .await
                    {
                        Ok(new_state) => {
                            tracing::info!(
                                new_schema = schema_reload,
                                new_license = license_reload,
                                new_configuration = configuration_reload,
                                event = STATE_CHANGE,
                                "reload complete"
                            );
                            Some(new_state)
                        }
                        Err(e) => {
                            // If we encountered an error it may be fatal depending on if we consumed the server handle or not.
                            match server_handle {
                                None => {
                                    tracing::error!(
                                        error = %e,
                                        event = STATE_CHANGE,
                                        "fatal error while trying to reload"
                                    );
                                    Some(Errored(e))
                                }
                                Some(_) => {
                                    tracing::error!(error = %e, event = STATE_CHANGE, "error while reloading, continuing with previous configuration");
                                    None
                                }
                            }
                        }
                    }
                } else {
                    tracing::info!(
                        new_schema = schema_reload,
                        new_license = license_reload,
                        new_configuration = configuration_reload,
                        event = STATE_CHANGE,
                        "no reload necessary"
                    );
                }
            }
            _ => {}
        }

        new_state.unwrap_or(self)
    }

    async fn shutdown<S>(self, http_server_factory: &S) -> Self
    where
        S: HttpServerFactory,
    {
        match self {
            Running {
                server_handle: Some(server_handle),
                mut all_connections_stopped_signals,
                ..
            } => {
                // We want to set the ready state to false before we start shutting down the server.
                http_server_factory.ready(false);
                tracing::info!("shutting down");
                let state = server_handle
                    .shutdown()
                    .map_ok_or_else(Errored, |_| Stopped)
                    .await;
                let futs: futures::stream::FuturesUnordered<_> = all_connections_stopped_signals
                    .iter_mut()
                    .map(|receiver| receiver.recv())
                    .collect();
                // We ignore the results of recv()
                let _: Vec<_> = futs.collect().await;
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
        license: LicenseState,
        listen_addresses_guard: &mut OwnedRwLockWriteGuard<ListenAddresses>,
        mut all_connections_stopped_signals: Vec<mpsc::Receiver<()>>,
    ) -> Result<State<FA>, ApolloRouterError>
    where
        S: HttpServerFactory,
        FA: RouterSuperServiceFactory,
    {
        // Check the license
        let report = LicenseEnforcementReport::build(&configuration);

        match license {
            LicenseState::Licensed => {
                tracing::debug!("A valid Apollo license has been detected.");
            }
            LicenseState::LicensedWarn if report.uses_restricted_features() => {
                tracing::error!("License has expired. The Router will soon stop serving requests. In order to enable these features for a self-hosted instance of Apollo Router, the Router must be connected to a graph in GraphOS that provides an active license for the following features:\n\n{}\n\nSee {LICENSE_EXPIRED_URL} for more information.", report);
            }
            LicenseState::LicensedHalt if report.uses_restricted_features() => {
                tracing::error!("License has expired. The Router will no longer serve requests. In order to enable these features for a self-hosted instance of Apollo Router, the Router must be connected to a graph in GraphOS that provides an active license for the following features:\n\n{}\n\nSee {LICENSE_EXPIRED_URL} for more information.", report);
            }
            LicenseState::Unlicensed if report.uses_restricted_features() => {
                // This is OSS, so fail to reload or start.
                if std::env::var("APOLLO_KEY").is_ok() && std::env::var("APOLLO_GRAPH_REF").is_ok()
                {
                    tracing::error!("License not found. In order to enable these features for a self-hosted instance of Apollo Router, the Router must be connected to a graph in GraphOS that provides a license for the following features:\n\n{}\n\nSee {LICENSE_EXPIRED_URL} for more information.", report);
                } else {
                    tracing::error!("Not connected to GraphOS. In order to enable these features for a self-hosted instance of Apollo Router, the Router must be connected to a graph in GraphOS (using APOLLO_KEY and APOLLO_GRAPH_REF) that provides a license for the following features:\n\n{}\n\nSee {LICENSE_EXPIRED_URL} for more information.", report);
                }

                return Err(ApolloRouterError::LicenseViolation);
            }
            _ => {
                tracing::debug!("A valid Apollo license was not detected. However, no restricted features are in use.");
            }
        }

        // If there are no restricted featured in use then the effective license is Licensed as we don't need warn or halt behavior.
        let effective_license = if !report.uses_restricted_features() {
            LicenseState::Licensed
        } else {
            license
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
        all_connections_stopped_signals.push(all_connections_stopped_signal);
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
                        effective_license,
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
                        effective_license,
                    )
                    .await?
            }
        };

        listen_addresses_guard.extra_listen_addresses = server_handle.listen_addresses().to_vec();
        listen_addresses_guard.graphql_listen_address =
            server_handle.graphql_listen_address().clone();

        // Log that we are using experimental features. It is best to do this here rather than config
        // validation as it will actually log issues rather than return structured validation errors.
        // Logging here also means that this is actually configuration that took effect
        if let Some(yaml) = &configuration.validated_yaml {
            let discussed = Discussed::new();
            discussed.log_experimental_used(yaml);
            discussed.log_preview_used(yaml);
        }

        let metrics_handle = Metrics::spawn(&configuration).await;

        Ok(Running {
            configuration,
            _metrics_handle: metrics_handle,
            schema,
            license,
            server_handle: Some(server_handle),
            router_service_factory,
            all_connections_stopped_signals,
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
    #[cfg(test)]
    notify_updated: Arc<Notify>,
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
            #[cfg(test)]
            notify_updated: Default::default(),
        }
    }

    #[cfg(test)]
    pub(crate) fn for_tests(
        http_server_factory: S,
        router_factory: FA,
        notify_updated: Arc<Notify>,
    ) -> Self {
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
            notify_updated,
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
            license: None,
            listen_addresses_guard: self
                .listen_addresses_guard
                .take()
                .expect("must have listen address guard"),
        };

        // Mark ourselves as live at this point

        self.http_server_factory.live(true);

        // Process all the events in turn until we get to error state or we run out of events.
        while let Some(event) = messages.next().await {
            let event_name = format!("{event:?}");
            let previous_state = format!("{state:?}");

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
                UpdateLicense(license) => {
                    state
                        .update_inputs(&mut self, None, None, Some(license))
                        .await
                }
                Reload => state.update_inputs(&mut self, None, None, None).await,
                NoMoreLicense => state.no_more_license().await,
                Shutdown => state.shutdown(&self.http_server_factory).await,
            };

            // Update the shared state
            #[cfg(test)]
            self.notify_updated.notify_one();

            tracing::debug!(
                monotonic_counter.apollo_router_state_change_total = 1u64,
                event = event_name,
                state = ?state,
                previous_state,
                "state machine transitioned"
            );

            // If we've errored then exit even if there are potentially more messages
            if matches!(&state, Stopped | Errored(_)) {
                break;
            }
        }
        tracing::info!("stopped");

        // Note that ready(false) will not be called on a non-graceful shutdown.
        self.http_server_factory.live(false);

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

    use futures::channel::oneshot;
    use mockall::mock;
    use mockall::predicate::eq;
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
    use crate::services::router;
    use crate::services::RouterRequest;

    type SharedOneShotReceiver = Arc<Mutex<Vec<oneshot::Receiver<()>>>>;

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
        let (server_factory, _) = create_mock_server_factory(0, 1, 0, 1, 0);
        assert_matches!(
            execute(
                server_factory,
                router_factory,
                stream::iter(vec![NoMoreConfiguration])
            )
            .await,
            Err(NoConfiguration)
        );
    }

    #[test(tokio::test)]
    async fn no_schema() {
        let router_factory = create_mock_router_configurator(0);
        let (server_factory, _) = create_mock_server_factory(0, 1, 0, 1, 0);
        assert_matches!(
            execute(
                server_factory,
                router_factory,
                stream::iter(vec![NoMoreSchema])
            )
            .await,
            Err(NoSchema)
        );
    }

    #[test(tokio::test)]
    async fn no_license() {
        let router_factory = create_mock_router_configurator(0);
        let (server_factory, _) = create_mock_server_factory(0, 1, 0, 1, 0);
        assert_matches!(
            execute(
                server_factory,
                router_factory,
                stream::iter(vec![NoMoreLicense])
            )
            .await,
            Err(NoLicense)
        );
    }
    fn test_config_restricted() -> Configuration {
        let mut config = Configuration::builder().build().unwrap();
        config.validated_yaml =
            Some(json!({"plugins":{"experimental.restricted":{"enabled":true}}}));
        config
    }

    #[test(tokio::test)]
    async fn restricted_licensed() {
        let router_factory = create_mock_router_configurator(1);
        let (server_factory, shutdown_receivers) = create_mock_server_factory(1, 1, 1, 1, 1);

        assert_matches!(
            execute(
                server_factory,
                router_factory,
                stream::iter(vec![
                    UpdateConfiguration(test_config_restricted()),
                    UpdateSchema(example_schema()),
                    UpdateLicense(LicenseState::Licensed),
                    Shutdown
                ])
            )
            .await,
            Ok(())
        );
        assert_eq!(shutdown_receivers.0.lock().unwrap().len(), 1);
    }

    #[test(tokio::test)]
    async fn restricted_licensed_halted() {
        let router_factory = create_mock_router_configurator(1);
        let (server_factory, shutdown_receivers) = create_mock_server_factory(1, 1, 1, 1, 1);

        assert_matches!(
            execute(
                server_factory,
                router_factory,
                stream::iter(vec![
                    UpdateConfiguration(test_config_restricted()),
                    UpdateSchema(example_schema()),
                    UpdateLicense(LicenseState::LicensedHalt),
                    Shutdown
                ])
            )
            .await,
            Ok(())
        );
        assert_eq!(shutdown_receivers.0.lock().unwrap().len(), 1);
    }

    #[test(tokio::test)]
    async fn restricted_licensed_warn() {
        let router_factory = create_mock_router_configurator(1);
        let (server_factory, shutdown_receivers) = create_mock_server_factory(1, 1, 1, 1, 1);

        assert_matches!(
            execute(
                server_factory,
                router_factory,
                stream::iter(vec![
                    UpdateConfiguration(test_config_restricted()),
                    UpdateSchema(example_schema()),
                    UpdateLicense(LicenseState::LicensedWarn),
                    Shutdown
                ])
            )
            .await,
            Ok(())
        );
        assert_eq!(shutdown_receivers.0.lock().unwrap().len(), 1);
    }

    #[test(tokio::test)]
    async fn restricted_licensed_unlicensed() {
        let router_factory = create_mock_router_configurator(2);
        let (server_factory, shutdown_receivers) = create_mock_server_factory(2, 1, 1, 1, 1);

        // The unlicensed event is dropped so we should get a reload
        assert_matches!(
            execute(
                server_factory,
                router_factory,
                stream::iter(vec![
                    UpdateConfiguration(test_config_restricted()),
                    UpdateSchema(example_schema()),
                    UpdateLicense(LicenseState::Licensed),
                    UpdateLicense(LicenseState::Unlicensed),
                    UpdateConfiguration(test_config_restricted()),
                    Shutdown
                ])
            )
            .await,
            Ok(())
        );
        assert_eq!(shutdown_receivers.0.lock().unwrap().len(), 2);
    }

    #[test(tokio::test)]
    async fn restricted_unlicensed() {
        let router_factory = create_mock_router_configurator(0);
        let (server_factory, shutdown_receivers) = create_mock_server_factory(0, 1, 0, 1, 0);

        assert_matches!(
            execute(
                server_factory,
                router_factory,
                stream::iter(vec![
                    UpdateConfiguration(test_config_restricted()),
                    UpdateSchema(example_schema()),
                    UpdateLicense(LicenseState::Unlicensed),
                    Shutdown
                ])
            )
            .await,
            Err(ApolloRouterError::LicenseViolation)
        );
        assert_eq!(shutdown_receivers.0.lock().unwrap().len(), 0);
    }

    #[test(tokio::test)]
    async fn unrestricted_unlicensed_restricted_licensed() {
        let router_factory = create_mock_router_configurator(2);
        let (server_factory, shutdown_receivers) = create_mock_server_factory(2, 1, 1, 1, 1);

        assert_matches!(
            execute(
                server_factory,
                router_factory,
                stream::iter(vec![
                    UpdateConfiguration(Configuration::builder().build().unwrap()),
                    UpdateSchema(example_schema()),
                    UpdateLicense(LicenseState::Unlicensed),
                    UpdateConfiguration(test_config_restricted()),
                    UpdateLicense(LicenseState::Licensed),
                    Shutdown
                ])
            )
            .await,
            Ok(())
        );
        assert_eq!(shutdown_receivers.0.lock().unwrap().len(), 2);
    }

    #[test(tokio::test)]
    async fn listen_addresses_are_locked() {
        let router_factory = create_mock_router_configurator(0);
        let (server_factory, _) = create_mock_server_factory(0, 0, 0, 0, 0);
        let state_machine = StateMachine::new(server_factory, router_factory);
        assert!(state_machine.listen_addresses.try_read().is_err());
    }

    #[test(tokio::test)]
    async fn shutdown_during_startup() {
        let router_factory = create_mock_router_configurator(0);
        let (server_factory, _) = create_mock_server_factory(0, 1, 0, 1, 0);
        assert_matches!(
            execute(server_factory, router_factory, stream::iter(vec![Shutdown])).await,
            Ok(())
        );
    }

    #[test(tokio::test)]
    async fn startup_shutdown() {
        let router_factory = create_mock_router_configurator(1);
        let (server_factory, shutdown_receivers) = create_mock_server_factory(1, 1, 1, 1, 1);

        assert_matches!(
            execute(
                server_factory,
                router_factory,
                stream::iter(vec![
                    UpdateConfiguration(Configuration::builder().build().unwrap()),
                    UpdateSchema(example_schema()),
                    UpdateLicense(LicenseState::default()),
                    Shutdown
                ])
            )
            .await,
            Ok(())
        );
        assert_eq!(shutdown_receivers.0.lock().unwrap().len(), 1);
    }

    #[test(tokio::test)]
    async fn startup_reload_schema() {
        let router_factory = create_mock_router_configurator(2);
        let (server_factory, shutdown_receivers) = create_mock_server_factory(2, 1, 1, 1, 1);
        let minimal_schema = include_str!("testdata/minimal_supergraph.graphql");
        assert_matches!(
            execute(
                server_factory,
                router_factory,
                stream::iter(vec![
                    UpdateConfiguration(Configuration::builder().build().unwrap()),
                    UpdateSchema(minimal_schema.to_owned()),
                    UpdateLicense(LicenseState::default()),
                    UpdateSchema(example_schema()),
                    Shutdown
                ])
            )
            .await,
            Ok(())
        );
        assert_eq!(shutdown_receivers.0.lock().unwrap().len(), 2);
    }

    #[test(tokio::test)]
    async fn startup_no_reload_schema() {
        let router_factory = create_mock_router_configurator(1);
        let (server_factory, shutdown_receivers) = create_mock_server_factory(1, 1, 1, 1, 1);
        let minimal_schema = include_str!("testdata/minimal_supergraph.graphql");
        assert_matches!(
            execute(
                server_factory,
                router_factory,
                stream::iter(vec![
                    UpdateConfiguration(Configuration::builder().build().unwrap()),
                    UpdateSchema(minimal_schema.to_owned()),
                    UpdateLicense(LicenseState::default()),
                    UpdateSchema(minimal_schema.to_owned()),
                    Shutdown
                ])
            )
            .await,
            Ok(())
        );
        assert_eq!(shutdown_receivers.0.lock().unwrap().len(), 1);
    }

    #[test(tokio::test)]
    async fn startup_reload_license() {
        let router_factory = create_mock_router_configurator(2);
        let (server_factory, shutdown_receivers) = create_mock_server_factory(2, 1, 1, 1, 1);
        let minimal_schema = include_str!("testdata/minimal_supergraph.graphql");
        assert_matches!(
            execute(
                server_factory,
                router_factory,
                stream::iter(vec![
                    UpdateConfiguration(Configuration::builder().build().unwrap()),
                    UpdateSchema(minimal_schema.to_owned()),
                    UpdateLicense(LicenseState::default()),
                    UpdateLicense(LicenseState::Licensed),
                    Shutdown
                ])
            )
            .await,
            Ok(())
        );
        assert_eq!(shutdown_receivers.0.lock().unwrap().len(), 2);
    }

    #[test(tokio::test)]
    async fn startup_reload_configuration() {
        let router_factory = create_mock_router_configurator(2);
        let (server_factory, shutdown_receivers) = create_mock_server_factory(2, 1, 1, 1, 1);

        assert_matches!(
            execute(
                server_factory,
                router_factory,
                stream::iter(vec![
                    UpdateConfiguration(Configuration::builder().build().unwrap()),
                    UpdateSchema(example_schema()),
                    UpdateLicense(LicenseState::default()),
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
                ])
            )
            .await,
            Ok(())
        );
        assert_eq!(shutdown_receivers.0.lock().unwrap().len(), 2);
    }

    #[test(tokio::test)]
    async fn extract_routing_urls() {
        let router_factory = create_mock_router_configurator(1);
        let (server_factory, shutdown_receivers) = create_mock_server_factory(1, 1, 1, 1, 1);

        assert_matches!(
            execute(
                server_factory,
                router_factory,
                stream::iter(vec![
                    UpdateConfiguration(Configuration::builder().build().unwrap()),
                    UpdateSchema(example_schema()),
                    UpdateLicense(LicenseState::default()),
                    Shutdown
                ])
            )
            .await,
            Ok(())
        );
        assert_eq!(shutdown_receivers.0.lock().unwrap().len(), 1);
    }

    #[test(tokio::test)]
    async fn router_factory_error_startup() {
        let mut router_factory = MockMyRouterConfigurator::new();
        router_factory
            .expect_create()
            .times(1)
            .returning(|_, _, _, _| Err(BoxError::from("Error")));

        let (server_factory, shutdown_receivers) = create_mock_server_factory(0, 1, 0, 1, 0);

        assert_matches!(
            execute(
                server_factory,
                router_factory,
                stream::iter(vec![
                    UpdateConfiguration(Configuration::builder().build().unwrap()),
                    UpdateSchema(example_schema()),
                    UpdateLicense(LicenseState::default()),
                ])
            )
            .await,
            Err(ApolloRouterError::ServiceCreationError(_))
        );
        assert_eq!(shutdown_receivers.0.lock().unwrap().len(), 0);
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

        let (server_factory, shutdown_receivers) = create_mock_server_factory(1, 1, 1, 1, 1);
        let minimal_schema = include_str!("testdata/minimal_supergraph.graphql");

        assert_matches!(
            execute(
                server_factory,
                router_factory,
                stream::iter(vec![
                    UpdateConfiguration(Configuration::builder().build().unwrap()),
                    UpdateSchema(example_schema()),
                    UpdateLicense(LicenseState::default()),
                    UpdateSchema(minimal_schema.to_owned()),
                    Shutdown
                ])
            )
            .await,
            Ok(())
        );
        assert_eq!(shutdown_receivers.0.lock().unwrap().len(), 1);
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

        let (server_factory, shutdown_receivers) = create_mock_server_factory(2, 1, 1, 1, 1);
        let minimal_schema = include_str!("testdata/minimal_supergraph.graphql");

        assert_matches!(
            execute(
                server_factory,
                router_factory,
                stream::iter(vec![
                    UpdateConfiguration(Configuration::builder().build().unwrap()),
                    UpdateSchema(example_schema()),
                    UpdateLicense(LicenseState::default()),
                    UpdateConfiguration(
                        Configuration::builder()
                            .homepage(Homepage::builder().enabled(true).build())
                            .build()
                            .unwrap()
                    ),
                    UpdateSchema(minimal_schema.to_owned()),
                    Shutdown
                ]),
            )
            .await,
            Ok(())
        );
        assert_eq!(shutdown_receivers.0.lock().unwrap().len(), 2);
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
            type RouterService = router::BoxService;
            type Future = <Self::RouterService as Service<RouterRequest>>::Future;
            fn web_endpoints(&self) -> MultiMap<ListenAddr, Endpoint>;
        }
        impl ServiceFactory<RouterRequest> for MyRouterFactory {
            type Service = router::BoxService;
            fn create(&self) -> router::BoxService;
        }

        impl Clone for MyRouterFactory {
            fn clone(&self) -> MockMyRouterFactory;
        }
    }

    mock! {
        #[derive(Debug)]
        MyHttpServerFactory{
            fn create_server(&self,
                configuration: Arc<Configuration>,
                main_listener: Option<Listener>,) -> Result<HttpServerHandle, ApolloRouterError>;
            fn live(&self, live: bool);
            fn ready(&self, ready: bool);
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

            _license: LicenseState,
            _all_connections_stopped_sender: mpsc::Sender<()>,
        ) -> Self::Future
        where
            RF: RouterFactory,
        {
            let res = self.create_server(configuration, main_listener);
            Box::pin(async move { res })
        }
        fn live(&self, live: bool) {
            self.live(live);
        }
        fn ready(&self, ready: bool) {
            self.ready(ready);
        }
    }

    async fn execute(
        server_factory: MockMyHttpServerFactory,
        router_factory: MockMyRouterConfigurator,
        events: impl Stream<Item = Event> + Unpin,
    ) -> Result<(), ApolloRouterError> {
        let state_machine = StateMachine::new(server_factory, router_factory);
        state_machine.process_events(events).await
    }

    fn create_mock_server_factory(
        expect_times_called: usize,
        live_true_times: usize,
        ready_true_times: usize,
        live_false_times: usize,
        ready_false_times: usize,
    ) -> (
        MockMyHttpServerFactory,
        (SharedOneShotReceiver, SharedOneShotReceiver),
    ) {
        let mut server_factory = MockMyHttpServerFactory::new();
        let shutdown_receivers = Arc::new(Mutex::new(vec![]));
        let extra_shutdown_receivers = Arc::new(Mutex::new(vec![]));
        let shutdown_receivers_clone = shutdown_receivers.to_owned();
        let extra_shutdown_receivers_clone = extra_shutdown_receivers.to_owned();
        server_factory
            .expect_create_server()
            .times(expect_times_called)
            .returning(
                move |configuration: Arc<Configuration>, mut main_listener: Option<Listener>| {
                    let (shutdown_sender, shutdown_receiver) = oneshot::channel();
                    let (extra_shutdown_sender, extra_shutdown_receiver) = oneshot::channel();
                    shutdown_receivers_clone
                        .lock()
                        .unwrap()
                        .push(shutdown_receiver);
                    extra_shutdown_receivers_clone
                        .lock()
                        .unwrap()
                        .push(extra_shutdown_receiver);

                    let server = async move {
                        let main_listener = match main_listener.take() {
                            Some(l) => l,
                            None => Listener::Tcp(
                                tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap(),
                            ),
                        };

                        Ok(main_listener)
                    };

                    let (all_connections_stopped_sender, _) = mpsc::channel::<()>(1);

                    Ok(HttpServerHandle::new(
                        shutdown_sender,
                        extra_shutdown_sender,
                        Box::pin(server),
                        Box::pin(async { Ok(vec![]) }),
                        Some(configuration.supergraph.listen.clone()),
                        vec![],
                        all_connections_stopped_sender,
                    ))
                },
            );
        server_factory
            .expect_live()
            .with(eq(true))
            .times(live_true_times)
            .return_const(());
        server_factory
            .expect_ready()
            .with(eq(true))
            .times(ready_true_times)
            .return_const(());
        server_factory
            .expect_live()
            .with(eq(false))
            .times(live_false_times)
            .return_const(());
        server_factory
            .expect_ready()
            .with(eq(false))
            .times(ready_false_times)
            .return_const(());
        (
            server_factory,
            (shutdown_receivers, extra_shutdown_receivers),
        )
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
                          _extra_plugins: &Option<Vec<(String, Box<dyn DynPlugin>)>>|
                          {
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
