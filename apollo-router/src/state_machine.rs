use std::fmt::Debug;
use std::fmt::Formatter;
use std::sync::Arc;

use ApolloRouterError::ServiceCreationError;
use Event::NoMoreConfiguration;
use Event::NoMoreLicense;
use Event::NoMoreSchema;
use Event::Reload;
use Event::RhaiReload;
use Event::Shutdown;
use State::Errored;
use State::Reloading;
use State::Running;
use State::Startup;
use State::Stopped;
use futures::prelude::*;
use itertools::Itertools;
#[cfg(test)]
use tokio::sync::Notify;
use tokio::sync::OwnedRwLockWriteGuard;
use tokio::sync::RwLock;
use tokio::sync::mpsc;
use tokio::time::Instant;

use super::http_server_factory::HttpServerFactory;
use super::http_server_factory::HttpServerHandle;
use super::router::ApolloRouterError::NoConfiguration;
use super::router::ApolloRouterError::NoSchema;
use super::router::ApolloRouterError::{self};
use super::router::Event::UpdateConfiguration;
use super::router::Event::UpdateSchema;
use super::router::Event::{self};
use crate::ApolloRouterError::NoLicense;
use crate::configuration::Configuration;
use crate::configuration::Discussed;
use crate::configuration::ListenAddr;
use crate::configuration::metrics::Metrics;
use crate::plugins::telemetry::reload::otel::apollo_opentelemetry_initialized;
use crate::router::Event::UpdateLicense;
use crate::router_factory::RouterFactory;
use crate::router_factory::RouterSuperServiceFactory;
use crate::spec::Schema;
use crate::uplink::feature_gate_enforcement::FeatureGateEnforcementReport;
use crate::uplink::license_enforcement::LICENSE_EXPIRED_URL;
use crate::uplink::license_enforcement::LicenseEnforcementReport;
use crate::uplink::license_enforcement::LicenseLimits;
use crate::uplink::license_enforcement::LicenseState;
use crate::uplink::schema::SchemaState;

const STATE_CHANGE: &str = "state change";

#[derive(Default, Clone)]
pub(crate) struct ListenAddresses {
    pub(crate) graphql_listen_address: Option<ListenAddr>,
    pub(crate) extra_listen_addresses: Vec<ListenAddr>,
}

/// Wraps a pending reload value and records whether it changed relative to the
/// last committed Running state.  The `Changed` variant carries both the
/// committed (old) value and the pending (new) value together, eliminating the
/// need for a separate committed-value field on the enclosing state.
enum PendingChange<T> {
    /// Value changed: `committed` is what was running before the reload was
    /// triggered; `pending` is the value we are trying to apply.
    Changed { committed: T, pending: T },
    /// Value carried forward from the committed state without modification.
    Unchanged(T),
}

impl<T> PendingChange<T> {
    /// Build from a committed value and an optional incoming update.
    /// `None` means no update arrived. `Some` applies the same equality check
    /// as `update`: reverts to `Unchanged` if the new value matches committed.
    fn new(committed: T, new: Option<T>) -> Self
    where
        T: PartialEq,
    {
        PendingChange::Unchanged(committed).update(new)
    }

    /// Conditionally set a new pending value, checking `new` against the committed value.
    /// If `new` matches committed, cancels any pending change and reverts to `Unchanged`.
    /// `None` is a no-op.
    fn update(self, new: Option<T>) -> Self
    where
        T: PartialEq,
    {
        match new {
            None => self,
            Some(new) if self.committed() == &new => PendingChange::Unchanged(new),
            Some(new) => self.set_pending(new),
        }
    }

    /// Unconditionally set a new pending value, preserving the committed value from `self`.
    /// Moves the committed value out of `self`, avoiding a clone.
    fn set_pending(self, pending: T) -> Self {
        PendingChange::Changed {
            committed: self.into_committed(),
            pending,
        }
    }

    /// The value we are trying to apply on the next try_start attempt.
    /// Equal to the committed value when there is no pending change.
    fn target(&self) -> &T {
        match self {
            PendingChange::Changed { pending, .. } => pending,
            PendingChange::Unchanged(v) => v,
        }
    }

    /// The last committed (currently serving) value.
    fn committed(&self) -> &T {
        match self {
            PendingChange::Changed { committed, .. } => committed,
            PendingChange::Unchanged(v) => v,
        }
    }

    /// Consume self and return the committed value.
    fn into_committed(self) -> T {
        match self {
            PendingChange::Changed { committed, .. } => committed,
            PendingChange::Unchanged(v) => v,
        }
    }

    /// True when there is a pending change relative to the committed (serving) value.
    fn is_pending(&self) -> bool {
        matches!(self, PendingChange::Changed { .. })
    }
}

/// This state maintains private information that is not exposed to the user via state listener.
#[allow(clippy::large_enum_variant)]
enum State<FA: RouterSuperServiceFactory> {
    Startup {
        configuration: Option<Arc<Configuration>>,
        schema: Option<Arc<SchemaState>>,
        license: Option<Arc<LicenseState>>,
        listen_addresses_guard: OwnedRwLockWriteGuard<ListenAddresses>,
    },
    Running {
        configuration: Arc<Configuration>,
        _metrics: Option<Metrics>,
        schema: Arc<SchemaState>,
        license: Arc<LicenseState>,
        server_handle: Option<HttpServerHandle>,
        router_service_factory: FA::RouterFactory,
        all_connections_stopped_signals: Vec<mpsc::Receiver<()>>,
    },
    /// Server is live on the committed state while a reload attempt is pending.
    /// If try_start fails we stay here and fire again at retry_after without
    /// requiring a new event from Uplink.
    Reloading {
        // Currently serving:
        // RAII guard — keeps committed metrics alive until the reload succeeds.
        _metrics: Option<Metrics>,
        server_handle: Option<HttpServerHandle>,
        // Passed as previous_router_service_factory to each try_start attempt
        // so the factory can reuse resources from the previous instance.
        router_service_factory: FA::RouterFactory,
        all_connections_stopped_signals: Vec<mpsc::Receiver<()>>,
        // What we are trying to apply:
        configuration: PendingChange<Arc<Configuration>>,
        schema: PendingChange<Arc<SchemaState>>,
        license: PendingChange<Arc<LicenseState>>,
        retry_after: Instant,
        /// Retries remaining for the current pending (configuration, schema, license).
        /// `None` means unlimited; `Some(0)` means exhausted.
        /// Reset to the configured `max_retries` whenever a new publish is received
        /// so that each publish gets a fresh budget of attempts.
        retries_remaining: Option<u32>,
    },
    Stopped,
    Errored(ApolloRouterError),
}

impl<FA: RouterSuperServiceFactory> Debug for State<FA> {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Startup { .. } => write!(f, "Startup"),
            Running { .. } => write!(f, "Running"),
            Reloading { .. } => write!(f, "Reloading"),
            Stopped => write!(f, "Stopped"),
            Errored(_) => write!(f, "Errored"),
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

    /// Returns true if the router is actively serving traffic under a valid license.
    fn is_licensed(&self) -> bool {
        match self {
            Running { license, .. } => license.is_licensed(),
            Reloading { license, .. } => license.committed().is_licensed(),
            _ => false,
        }
    }

    /// Returns the scheduled retry instant if the state machine is in `Reloading`
    /// with at least one attempt remaining.  Returns `None` when not reloading or
    /// when the retry budget is exhausted (`retries_remaining == Some(0)`).
    fn retry_after(&self) -> Option<Instant> {
        match self {
            Reloading {
                retry_after,
                retries_remaining,
                ..
            } if *retries_remaining != Some(0) => Some(*retry_after),
            _ => None,
        }
    }

    /// Pure step: merge new inputs into the current state, transitioning
    /// Running → Reloading when a reload is needed. No I/O.
    fn accumulate_inputs(
        self,
        new_schema: Option<Arc<SchemaState>>,
        new_configuration: Option<Arc<Configuration>>,
        new_license: Option<Arc<LicenseState>>,
        force_reload: bool,
    ) -> Self {
        // When we get an unlicensed event, if the router is already running with a license,
        // just carry on. Users can delete and undelete their graphs in Studio while
        // the router continues to run.
        if self.is_licensed() && new_license.as_deref().is_some_and(|l| l.is_unlicensed()) {
            tracing::info!(
                event = STATE_CHANGE,
                "ignoring reload because of loss of license"
            );
            return self;
        }

        match self {
            Startup {
                schema,
                configuration,
                license,
                listen_addresses_guard,
            } => Startup {
                schema: new_schema.or(schema),
                configuration: new_configuration.or(configuration),
                license: new_license.or(license),
                listen_addresses_guard,
            },

            Running {
                configuration,
                _metrics,
                schema,
                license,
                server_handle,
                router_service_factory,
                all_connections_stopped_signals,
            } => {
                // Configuration has no equality check — any new config unconditionally triggers a reload.
                let configuration = match new_configuration {
                    Some(nc) => PendingChange::Unchanged(configuration).set_pending(nc),
                    None => PendingChange::Unchanged(configuration),
                };
                let schema = PendingChange::new(schema, new_schema);
                let license = PendingChange::new(license, new_license);

                let need_reload = force_reload
                    || configuration.is_pending()
                    || schema.is_pending()
                    || license.is_pending();

                tracing::info!(
                    new_schema = schema.is_pending(),
                    new_license = license.is_pending(),
                    new_configuration = configuration.is_pending(),
                    event = STATE_CHANGE,
                    "processing event"
                );

                if need_reload {
                    Reloading {
                        _metrics,
                        server_handle,
                        router_service_factory,
                        all_connections_stopped_signals,
                        // Initialize the retry budget from the config we are about to apply.
                        // Add 1 so that max_retries reflects the number of *retries* after
                        // the initial attempt: the first attempt (event-triggered) consumes
                        // one slot, leaving max_retries timer-driven retries remaining.
                        retries_remaining: configuration.target().reload.max_retries.map(|n| n + 1),
                        configuration,
                        schema,
                        license,
                        retry_after: Instant::now(),
                    }
                } else {
                    tracing::info!(
                        new_schema = false,
                        new_license = false,
                        new_configuration = false,
                        event = STATE_CHANGE,
                        "no reload necessary"
                    );
                    Running {
                        configuration: configuration.into_committed(),
                        _metrics,
                        schema: schema.into_committed(),
                        license: license.into_committed(),
                        server_handle,
                        router_service_factory,
                        all_connections_stopped_signals,
                    }
                }
            }

            Reloading {
                _metrics,
                server_handle,
                router_service_factory,
                all_connections_stopped_signals,
                mut configuration,
                mut schema,
                mut license,
                retry_after: _,
                retries_remaining: _,
            } => {
                if let Some(nc) = new_configuration {
                    configuration = configuration.set_pending(nc);
                }
                schema = schema.update(new_schema);
                license = license.update(new_license);

                // Any event while reloading resets the retry budget: new inputs from
                // Uplink deserve a fresh set of attempts, and explicit Reload/RhaiReload
                // commands should also revive an exhausted budget.  Add 1 for the same
                // reason as above — the imminent event-triggered attempt uses one slot.
                let retries_remaining = configuration.target().reload.max_retries.map(|n| n + 1);

                tracing::info!(
                    // True when there is a pending change relative to what the router is serving.
                    new_schema = schema.is_pending(),
                    new_license = license.is_pending(),
                    new_configuration = configuration.is_pending(),
                    event = STATE_CHANGE,
                    "processing event while reloading"
                );

                Reloading {
                    _metrics,
                    server_handle,
                    router_service_factory,
                    all_connections_stopped_signals,
                    configuration,
                    schema,
                    license,
                    retry_after: Instant::now(),
                    retries_remaining,
                }
            }

            s => s,
        }
    }

    /// Async step: attempt a (re)load for states that are ready for one.
    /// Returns self unchanged for states that have nothing to do.
    async fn attempt_reload<S>(self, state_machine: &mut StateMachine<S, FA>) -> Self
    where
        S: HttpServerFactory,
    {
        match self {
            Startup {
                schema: Some(schema),
                configuration: Some(configuration),
                license: Some(license),
                mut listen_addresses_guard,
            } => {
                Self::try_start(
                    state_machine,
                    &mut None,
                    None,
                    configuration,
                    schema,
                    license,
                    &mut listen_addresses_guard,
                    vec![],
                )
                .map_ok_or_else(Errored, |f| f.0)
                .await
            }

            // Note: attempt_reload always tries regardless of `retries_remaining`.
            // The budget only gates the *timer* — a retry triggered by an external
            // event (new schema, RhaiReload, etc.) always gets an immediate attempt
            // even if the timer budget is exhausted.  This is intentional: a fresh
            // Uplink publish should never be silently ignored.
            Reloading {
                mut _metrics,
                mut server_handle,
                router_service_factory,
                all_connections_stopped_signals: signals,
                configuration,
                schema,
                license,
                retry_after: _,
                retries_remaining,
            } => {
                let mut guard = state_machine.listen_addresses.clone().write_owned().await;

                match Self::try_start(
                    state_machine,
                    &mut server_handle,
                    Some(&router_service_factory),
                    configuration.target().clone(),
                    schema.target().clone(),
                    license.target().clone(),
                    &mut guard,
                    signals,
                )
                .await
                {
                    Ok((new_state, new_schema)) => {
                        tracing::info!(
                            new_schema = schema.is_pending(),
                            new_license = license.is_pending(),
                            new_configuration = configuration.is_pending(),
                            event = STATE_CHANGE,
                            "reload complete"
                        );
                        // Explicitly drop the old factory before broadcasting notifications so
                        // that its resources (connections, background tasks) are fully torn down
                        // before any listeners act on the reload-complete signal.
                        drop(router_service_factory);
                        // Broadcast change notifications after pipelines have fully rolled over.
                        if configuration.is_pending() {
                            // Notify listeners on the *previous* configuration's channel that
                            // the configuration has changed, passing a weak ref to the new one.
                            configuration
                                .committed()
                                .notify
                                .broadcast_configuration(Arc::downgrade(configuration.target()));
                        }
                        if schema.is_pending() {
                            // Notify listeners on the *new* configuration's channel that
                            // the schema has changed.
                            configuration.target().notify.broadcast_schema(new_schema);
                        }
                        new_state
                    }
                    Err(e) if server_handle.is_some() => {
                        // Decrement the retry budget (saturating so it stops at 0, not wrapping).
                        let retries_remaining = retries_remaining.map(|n| n.saturating_sub(1));

                        tracing::error!(
                            error = %e,
                            retries_remaining = retries_remaining
                                .map_or("unlimited".to_string(), |n| n.to_string()),
                            event = STATE_CHANGE,
                            "error while reloading, still running with previous configuration"
                        );

                        let retry_delay =
                            retry_delay_with_jitter(configuration.target().reload.retry_delay);

                        Reloading {
                            _metrics,
                            server_handle,
                            router_service_factory,
                            // try_start consumed and dropped the signals on failure, as it
                            // did before the Reloading state was introduced. Connections
                            // from before this attempt will not be awaited on shutdown.
                            all_connections_stopped_signals: vec![],
                            configuration,
                            schema,
                            license,
                            retry_after: Instant::now() + retry_delay,
                            retries_remaining,
                        }
                    }
                    // The point of no return was passed — server handle consumed
                    // before the failure. Fatal.
                    Err(e) => {
                        tracing::error!(
                            error = %e,
                            event = STATE_CHANGE,
                            "fatal error while trying to reload"
                        );
                        drop(router_service_factory);
                        Errored(e)
                    }
                }
            }

            s => s,
        }
    }

    async fn shutdown(self) -> Self {
        match self {
            Running {
                server_handle: Some(server_handle),
                mut all_connections_stopped_signals,
                ..
            }
            | Reloading {
                server_handle: Some(server_handle),
                mut all_connections_stopped_signals,
                ..
            } => {
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

    /// Start a router. Returns the schema so active subscriptions on a previous
    /// configuration or schema can be notified of the new schema.
    #[allow(clippy::too_many_arguments)]
    async fn try_start<S>(
        state_machine: &mut StateMachine<S, FA>,
        server_handle: &mut Option<HttpServerHandle>,
        previous_router_service_factory: Option<&FA::RouterFactory>,
        configuration: Arc<Configuration>,
        schema_state: Arc<SchemaState>,
        license: Arc<LicenseState>,
        listen_addresses_guard: &mut OwnedRwLockWriteGuard<ListenAddresses>,
        mut all_connections_stopped_signals: Vec<mpsc::Receiver<()>>,
    ) -> Result<(State<FA>, Arc<Schema>), ApolloRouterError>
    where
        S: HttpServerFactory,
        FA: RouterSuperServiceFactory,
    {
        let schema = Arc::new(
            Schema::parse_arc(schema_state.clone(), &configuration)
                .map_err(|e| ServiceCreationError(e.to_string().into()))?,
        );
        // Check the license
        let report = LicenseEnforcementReport::build(&configuration, &schema, &license);

        let license_limits = match &*license {
            LicenseState::Licensed { limits } => {
                if report.uses_restricted_features() {
                    tracing::error!(
                        "The router is using features not available for your license:\n\n{}",
                        report
                    );
                    return Err(ApolloRouterError::LicenseViolation(
                        report.restricted_features_in_use(),
                    ));
                } else {
                    tracing::debug!("A valid Apollo license has been detected.");
                    limits
                }
            }
            LicenseState::LicensedWarn { limits } => {
                if report.uses_restricted_features() {
                    tracing::error!(
                        "License violation, the router is using features not available for your license:\n\n{}\n\nThe license warning period has started. The Router will stop serving requests after the license expires. See {LICENSE_EXPIRED_URL} for more information.",
                        report
                    );
                    return Err(ApolloRouterError::LicenseViolation(
                        report.restricted_features_in_use(),
                    ));
                } else {
                    tracing::warn!(
                        "License warning period has started. The Router will stop serving requests after the license expires. In order to continue using these features for a self-hosted instance of Apollo Router, the Router must be connected to a graph in GraphOS that provides an active license for the following features:\n\n{:?}\n\nSee {LICENSE_EXPIRED_URL} for more information.",
                        // The report does not contain any features because they are contained within the allowedFeatures claim,
                        // therefore we output all of the allowed features that the user's license enables them to use.
                        license.get_allowed_features()
                    );
                    limits
                }
            }
            // LicensedHalt doesn't return an error, which might be surprising; rather, the middleware in the axum
            // server (`license_handler`) will check for halted licenses and send back a canned response
            LicenseState::LicensedHalt { limits } => {
                if report.uses_restricted_features() {
                    tracing::error!(
                        "License has expired. The Router will no longer serve requests. In order to enable these features for a self-hosted instance of Apollo Router, the Router must be connected to a graph in GraphOS that provides an active license for the following features:\n\n{}\n\nSee {LICENSE_EXPIRED_URL} for more information.",
                        report
                    );
                    limits
                } else {
                    tracing::error!(
                        "License has expired. The Router will no longer serve requests. In order to enable these features for a self-hosted instance of Apollo Router, the Router must be connected to a graph in GraphOS that provides an active license for the following features:\n\n{:?}\n\nSee {LICENSE_EXPIRED_URL} for more information.",
                        // The report does not contain any features because they are contained within the allowedFeatures claim,
                        // therefore we output all of the allowed features that the user's license enables them to use.
                        license.get_allowed_features()
                    );
                    limits
                }
            }
            LicenseState::Unlicensed if report.uses_restricted_features() => {
                // This is OSS, so fail to reload or start.
                if crate::services::APOLLO_KEY.lock().is_some()
                    && crate::services::APOLLO_GRAPH_REF.lock().is_some()
                {
                    tracing::error!(
                        "License not found. In order to enable these features for a self-hosted instance of Apollo Router, the Router must be connected to a graph in GraphOS that provides a license for the following features:\n\n{}\n\nSee {LICENSE_EXPIRED_URL} for more information.",
                        report
                    );
                } else {
                    tracing::error!(
                        "Not connected to GraphOS. In order to enable these features for a self-hosted instance of Apollo Router, the Router must be connected to a graph in GraphOS (using APOLLO_KEY and APOLLO_GRAPH_REF) that provides a license for the following features:\n\n{}\n\nSee {LICENSE_EXPIRED_URL} for more information.",
                        report
                    );
                }
                return Err(ApolloRouterError::LicenseViolation(
                    report.restricted_features_in_use(),
                ));
            }
            _ => {
                tracing::debug!(
                    "A valid Apollo license was not detected. However, no restricted features are in use."
                );
                // Without restricted features, there's no need to limit the router
                &Option::<LicenseLimits>::None
            }
        };

        // If there are no restricted features in use then the effective license is Licensed as we don't need warn or halt behavior.
        let effective_license = if !report.uses_restricted_features() {
            Arc::new(LicenseState::Licensed {
                limits: license_limits.clone(),
            })
        } else {
            license.clone()
        };

        if let Err(feature_gate_violations) =
            FeatureGateEnforcementReport::build(&configuration, &schema).check()
        {
            tracing::error!(
                "The schema contains preview features not enabled in configuration.\n\n{}",
                feature_gate_violations.iter().join("\n")
            );
            return Err(ApolloRouterError::FeatureGateViolation);
        }

        let router_service_factory = state_machine
            .router_configurator
            .create(
                state_machine.is_telemetry_disabled,
                configuration.clone(),
                schema.clone(),
                previous_router_service_factory,
                None,
                effective_license.clone(),
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

        let metrics = apollo_opentelemetry_initialized()
            .then(|| Metrics::new(&configuration, Arc::as_ref(&license)));

        Ok((
            Running {
                configuration,
                _metrics: metrics,
                schema: schema_state,
                license,
                server_handle: Some(server_handle),
                router_service_factory,
                all_connections_stopped_signals,
            },
            schema,
        ))
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
    is_telemetry_disabled: bool,
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
    pub(crate) fn new(
        is_telemetry_disabled: bool,
        http_server_factory: S,
        router_factory: FA,
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
            is_telemetry_disabled,
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
            is_telemetry_disabled: false,
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

        // Process events and retry-timer ticks until we reach a terminal state or
        // run out of events.
        loop {
            // Arm the retry timer when there are retries remaining; otherwise
            // use a never-resolving future so the arm is never selected.
            let retry_future = if let Some(retry_after) = state.retry_after() {
                futures::future::Either::Left(tokio::time::sleep_until(retry_after))
            } else {
                futures::future::Either::Right(std::future::pending())
            };

            let (event_name, previous_state) = tokio::select! {
                biased;

                event = messages.next() => {
                    let Some(event) = event else { break };
                    let event_name = event.to_string();
                    let previous_state = format!("{state:?}");

                    state = match event {
                        UpdateConfiguration(configuration) => {
                            state.accumulate_inputs(None, Some(configuration), None, false)
                        }
                        NoMoreConfiguration => state.no_more_configuration().await,
                        UpdateSchema(schema) => {
                            state.accumulate_inputs(Some(Arc::new(schema)), None, None, false)
                        }
                        NoMoreSchema => state.no_more_schema().await,
                        UpdateLicense(license) => {
                            state.accumulate_inputs(None, None, Some(license), false)
                        }
                        Reload => state.accumulate_inputs(None, None, None, false),
                        RhaiReload => state.accumulate_inputs(None, None, None, true),
                        NoMoreLicense => state.no_more_license().await,
                        Shutdown => state.shutdown().await,
                    };
                    state = state.attempt_reload(&mut self).await;
                    (event_name, previous_state)
                }
                _ = retry_future => {
                    let previous_state = format!("{state:?}");
                    state = state.attempt_reload(&mut self).await;
                    (String::from("retry"), previous_state)
                }
            };

            // Update the shared state
            #[cfg(test)]
            self.notify_updated.notify_one();

            tracing::info!(
                event = event_name,
                state = ?state,
                previous_state,
                "state machine transitioned"
            );
            u64_counter!(
                "apollo.router.state.change.total",
                "Router state changes",
                1,
                event = event_name,
                state = format!("{state:?}"),
                previous_state = previous_state
            );

            // If we've errored then exit even if there are potentially more messages
            if matches!(&state, Stopped | Errored(_)) {
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

/// Computes the retry delay: base delay plus up to 25% random positive jitter.
/// `rand::random::<f64>()` returns a value in [0.0, 1.0), so the result is always
/// at least `base` and at most `base * 1.25` — never shorter than the base delay.
/// Jitter is suppressed in test builds so that timer-based tests are deterministic.
fn retry_delay_with_jitter(base: std::time::Duration) -> std::time::Duration {
    #[cfg(not(test))]
    {
        const JITTER_FACTOR: f64 = 0.25;
        base + base.mul_f64(rand::random::<f64>() * JITTER_FACTOR)
    }
    #[cfg(test)]
    {
        base
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;
    use std::net::SocketAddr;
    use std::pin::Pin;
    use std::str::FromStr;

    use futures::channel::oneshot;
    use mockall::Sequence;
    use mockall::mock;
    use multimap::MultiMap;
    use parking_lot::Mutex;
    use rstest::rstest;
    use serde_json::json;
    use test_log::test;
    use tower::BoxError;
    use tower::Service;

    use super::*;
    use crate::AllowedFeature;
    use crate::configuration::Homepage;
    use crate::http_server_factory::Listener;
    use crate::metrics::FutureMetricsExt;
    use crate::plugin::DynPlugin;
    use crate::router_factory::Endpoint;
    use crate::router_factory::RouterFactory;
    use crate::router_factory::RouterSuperServiceFactory;
    use crate::services::RouterRequest;
    use crate::services::new_service::ServiceFactory;
    use crate::services::router;
    use crate::services::router::pipeline_handle::PipelineRef;
    use crate::uplink::schema::SchemaState;

    type SharedOneShotReceiver = Arc<Mutex<Vec<oneshot::Receiver<()>>>>;

    fn example_schema() -> SchemaState {
        SchemaState {
            sdl: include_str!("testdata/supergraph.graphql").to_owned(),
            launch_id: None,
        }
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
        let (server_factory, _) = create_mock_server_factory(0);
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
        let (server_factory, _) = create_mock_server_factory(0);
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

    fn test_config_restricted() -> Arc<Configuration> {
        let mut config = Configuration::builder().build().unwrap();
        config.validated_yaml =
            Some(json!({"plugins":{"experimental.restricted":{"enabled":true}}}));
        Arc::new(config)
    }
    fn test_config_with_apq_caching() -> Arc<Configuration> {
        let mut config = Configuration::builder().build().unwrap();
        config.validated_yaml = Some(json!({"apq":{"router":{"cache":{"redis":{"pool_size":1}}}}}));
        Arc::new(config)
    }
    fn test_config_with_subscriptions() -> Arc<Configuration> {
        let mut config = Configuration::builder().build().unwrap();
        config.validated_yaml = Some(json!({"subscription":{"enabled":true}}));
        Arc::new(config)
    }
    fn test_config_with_demand_control() -> Arc<Configuration> {
        let mut config = Configuration::builder().build().unwrap();
        config.validated_yaml = Some(json!({"demand_control":{"enabled":true}}));
        Arc::new(config)
    }
    fn test_config_with_request_limits() -> Arc<Configuration> {
        let mut config = Configuration::builder().build().unwrap();
        config.validated_yaml = Some(json!({
            "limits": {
                "router": {
                    "max_height": 100,
                    "max_aliases": 100,
                    "max_depth": 20
                }
            }
        }));
        Arc::new(config)
    }
    fn test_config_with_auth() -> Arc<Configuration> {
        let mut config = Configuration::builder().build().unwrap();
        config.validated_yaml = Some(json!({
            "authentication": {
                "router": {
                    "sources": {}
                }
            },
            "authorization": {
                "require_authentication": true
            }
        }));
        Arc::new(config)
    }

    #[test(tokio::test)]
    async fn restricted_licensed() {
        let router_factory = create_mock_router_configurator(1);
        let (server_factory, shutdown_receivers) = create_mock_server_factory(1);

        assert_matches!(
            execute(
                server_factory,
                router_factory,
                stream::iter(vec![
                    UpdateConfiguration(test_config_restricted()),
                    UpdateSchema(example_schema()),
                    UpdateLicense(Arc::new(LicenseState::Licensed {
                        limits: Some(LicenseLimits::default())
                    })),
                    Shutdown
                ])
            )
            .await,
            Ok(())
        );
        assert_eq!(shutdown_receivers.0.lock().len(), 1);
    }

    #[test(tokio::test)]
    #[rstest]
    #[case::apq(test_config_with_apq_caching(), vec![AllowedFeature::ApqCaching])]
    #[case::subscriptions(test_config_with_subscriptions(), vec![AllowedFeature::Subscriptions])]
    #[case::demand_control(test_config_with_demand_control(), vec![AllowedFeature::Subscriptions, AllowedFeature::DemandControl])]
    #[case::request_limits(test_config_with_request_limits(), vec![AllowedFeature::Subscriptions, AllowedFeature::RequestLimits, AllowedFeature::DemandControl])]
    #[case::request_limits(test_config_with_auth(), vec![AllowedFeature::Authentication, AllowedFeature::RequestLimits, AllowedFeature::Authorization])]
    async fn restricted_licensed_with_allowed_features_feature_contained_in_allowed_features_claim(
        #[case] config: Arc<Configuration>,
        #[case] allowed_features: Vec<AllowedFeature>,
    ) {
        let router_factory = create_mock_router_configurator(1);
        let (server_factory, shutdown_receivers) = create_mock_server_factory(1);

        assert_matches!(
            execute(
                server_factory,
                router_factory,
                stream::iter(vec![
                    UpdateConfiguration(config),
                    UpdateSchema(example_schema()),
                    UpdateLicense(Arc::new(LicenseState::Licensed {
                        limits: Some(LicenseLimits {
                            tps: None,
                            allowed_features: (HashSet::from_iter(allowed_features))
                        })
                    })),
                    Shutdown
                ])
            )
            .await,
            Ok(())
        );
        assert_eq!(shutdown_receivers.0.lock().len(), 1);
    }

    #[test(tokio::test)]
    #[rstest]
    #[case::apq_empty_allowed_features(test_config_with_apq_caching(), vec![])]
    #[case::subscriptions_not_in_allowed_features(test_config_with_subscriptions(), vec![AllowedFeature::ApqCaching])]
    #[case::demand_control_not_in_allowed_features(test_config_with_demand_control(), vec![AllowedFeature::Subscriptions, AllowedFeature::ApqCaching])]
    #[case::request_limits_not_in_allowed_features(test_config_with_request_limits(), vec![AllowedFeature::Subscriptions, AllowedFeature::Subscriptions, AllowedFeature::DemandControl])]
    #[case::auth_not_in_allowed_features(test_config_with_auth(), vec![AllowedFeature::ApqCaching])]
    async fn restricted_licensed_with_allowed_features_feature_not_contained_in_allowed_features_claim(
        #[case] config: Arc<Configuration>,
        #[case] allowed_features: Vec<AllowedFeature>,
    ) {
        let router_factory = create_mock_router_configurator(0);
        let (server_factory, shutdown_receivers) = create_mock_server_factory(0);

        assert_matches!(
            execute(
                server_factory,
                router_factory,
                stream::iter(vec![
                    UpdateConfiguration(config),
                    UpdateSchema(example_schema()),
                    UpdateLicense(Arc::new(LicenseState::Licensed {
                        limits: Some(LicenseLimits {
                            tps: None,
                            allowed_features: HashSet::from_iter(allowed_features)
                        })
                    })),
                    Shutdown
                ])
            )
            .await,
            Err(ApolloRouterError::LicenseViolation(_))
        );
        assert_eq!(shutdown_receivers.0.lock().len(), 0);
    }

    #[test(tokio::test)]
    async fn restricted_licensed_halted() {
        let router_factory = create_mock_router_configurator(1);
        let (server_factory, shutdown_receivers) = create_mock_server_factory(1);

        assert_matches!(
            execute(
                server_factory,
                router_factory,
                stream::iter(vec![
                    UpdateConfiguration(test_config_restricted()),
                    UpdateSchema(example_schema()),
                    UpdateLicense(Arc::new(LicenseState::LicensedHalt {
                        limits: Some(LicenseLimits::default())
                    })),
                    Shutdown
                ])
            )
            .await,
            Ok(())
        );
        assert_eq!(shutdown_receivers.0.lock().len(), 1);
    }

    #[test(tokio::test)]
    async fn unrestricted_licensed_halted() {
        let router_factory = create_mock_router_configurator(1);
        let (server_factory, shutdown_receivers) = create_mock_server_factory(1);

        assert_matches!(
            execute(
                server_factory,
                router_factory,
                stream::iter(vec![
                    UpdateConfiguration(Arc::new(Configuration::builder().build().unwrap())),
                    UpdateSchema(example_schema()),
                    UpdateLicense(Arc::new(LicenseState::LicensedHalt {
                        limits: Some(LicenseLimits::default())
                    })),
                    Shutdown
                ])
            )
            .await,
            Ok(())
        );
        assert_eq!(shutdown_receivers.0.lock().len(), 1);
    }

    #[test(tokio::test)]
    #[rstest]
    #[case::apq(test_config_with_apq_caching(), vec![AllowedFeature::ApqCaching])]
    #[case::subscriptions(test_config_with_subscriptions(), vec![AllowedFeature::Subscriptions])]
    #[case::demand_control(test_config_with_demand_control(), vec![AllowedFeature::Subscriptions, AllowedFeature::DemandControl])]
    #[case::request_limits(test_config_with_request_limits(), vec![AllowedFeature::Subscriptions, AllowedFeature::RequestLimits, AllowedFeature::DemandControl])]
    #[case::auth(test_config_with_auth(), vec![AllowedFeature::Authentication, AllowedFeature::RequestLimits, AllowedFeature::Authorization])]
    async fn restricted_licensed_halted_with_allowed_features_feature_contained_in_allowed_features_claim(
        #[case] config: Arc<Configuration>,
        #[case] allowed_features: Vec<AllowedFeature>,
    ) {
        let router_factory = create_mock_router_configurator(1);
        let (server_factory, shutdown_receivers) = create_mock_server_factory(1);

        assert_matches!(
            execute(
                server_factory,
                router_factory,
                stream::iter(vec![
                    UpdateConfiguration(config),
                    UpdateSchema(example_schema()),
                    UpdateLicense(Arc::new(LicenseState::LicensedHalt {
                        limits: Some(LicenseLimits {
                            tps: None,
                            allowed_features: HashSet::from_iter(allowed_features)
                        })
                    })),
                    Shutdown
                ])
            )
            .await,
            Ok(())
        );
        assert_eq!(shutdown_receivers.0.lock().len(), 1);
    }

    #[test(tokio::test)]
    #[rstest]
    #[case::apq_empty_allowed_features(test_config_with_apq_caching(), vec![])]
    #[case::subscriptions_not_in_allowed_features(test_config_with_subscriptions(), vec![AllowedFeature::ApqCaching])]
    #[case::demand_control_not_in_allowed_features(test_config_with_demand_control(), vec![AllowedFeature::Subscriptions, AllowedFeature::ApqCaching])]
    #[case::request_limits_not_in_allowed_features(test_config_with_request_limits(), vec![AllowedFeature::Subscriptions, AllowedFeature::Subscriptions, AllowedFeature::ApqCaching])]
    #[case::auth_not_in_allowed_features(test_config_with_auth(), vec![AllowedFeature::ApqCaching])]
    async fn restricted_licensed_halted_with_allowed_features_feature_not_contained_in_allowed_features_claim(
        #[case] config: Arc<Configuration>,
        #[case] allowed_features: Vec<AllowedFeature>,
    ) {
        let router_factory = create_mock_router_configurator(1);
        let (server_factory, shutdown_receivers) = create_mock_server_factory(1);

        assert_matches!(
            execute(
                server_factory,
                router_factory,
                stream::iter(vec![
                    UpdateConfiguration(config),
                    UpdateSchema(example_schema()),
                    UpdateLicense(Arc::new(LicenseState::LicensedHalt {
                        limits: Some(LicenseLimits {
                            tps: None,
                            allowed_features: HashSet::from_iter(allowed_features)
                        })
                    })),
                    Shutdown
                ])
            )
            .await,
            Ok(())
        );
        assert_eq!(shutdown_receivers.0.lock().len(), 1);
    }

    #[test(tokio::test)]
    #[rstest]
    #[case::apq(test_config_with_apq_caching(), vec![AllowedFeature::ApqCaching])]
    #[case::subscriptions(test_config_with_subscriptions(), vec![AllowedFeature::Subscriptions])]
    #[case::demand_control(test_config_with_demand_control(), vec![AllowedFeature::Subscriptions, AllowedFeature::DemandControl])]
    #[case::request_limits(test_config_with_request_limits(), vec![AllowedFeature::Subscriptions, AllowedFeature::RequestLimits, AllowedFeature::DemandControl])]
    #[case::auth(test_config_with_auth(), vec![AllowedFeature::Authentication, AllowedFeature::RequestLimits, AllowedFeature::Authorization])]
    async fn restricted_license_warn_reloaded_with_license_halted_with_allowed_features_feature_contained_in_allowed_features_claim(
        #[case] config: Arc<Configuration>,
        #[case] allowed_features: Vec<AllowedFeature>,
    ) {
        let router_factory = create_mock_router_configurator(3);
        let (server_factory, shutdown_receivers) = create_mock_server_factory(3);

        assert_matches!(
            execute(
                server_factory,
                router_factory,
                stream::iter(vec![
                    UpdateConfiguration(config.clone()),
                    UpdateSchema(example_schema()),
                    UpdateLicense(Arc::new(LicenseState::LicensedWarn {
                        limits: Some(LicenseLimits {
                            tps: None,
                            allowed_features: HashSet::from_iter(allowed_features.clone())
                        })
                    })),
                    UpdateConfiguration(config),
                    UpdateLicense(Arc::new(LicenseState::LicensedHalt {
                        limits: Some(LicenseLimits {
                            tps: None,
                            allowed_features: HashSet::from_iter(allowed_features)
                        })
                    })),
                    Shutdown
                ])
            )
            .await,
            Ok(())
        );
        assert_eq!(shutdown_receivers.0.lock().len(), 3);
    }

    #[test(tokio::test)]
    async fn restricted_licensed_warn() {
        let router_factory = create_mock_router_configurator(1);
        let (server_factory, shutdown_receivers) = create_mock_server_factory(1);

        assert_matches!(
            execute(
                server_factory,
                router_factory,
                stream::iter(vec![
                    UpdateConfiguration(test_config_restricted()),
                    UpdateSchema(example_schema()),
                    UpdateLicense(Arc::new(LicenseState::LicensedWarn {
                        limits: Some(LicenseLimits::default())
                    })),
                    Shutdown
                ])
            )
            .await,
            Ok(())
        );
        assert_eq!(shutdown_receivers.0.lock().len(), 1);
    }

    #[test(tokio::test)]
    #[rstest]
    #[case::apq(test_config_with_apq_caching(), vec![AllowedFeature::ApqCaching])]
    #[case::subscriptions(test_config_with_subscriptions(), vec![AllowedFeature::Subscriptions])]
    #[case::demand_control(test_config_with_demand_control(), vec![AllowedFeature::Subscriptions, AllowedFeature::DemandControl])]
    #[case::request_limits(test_config_with_request_limits(), vec![AllowedFeature::Subscriptions, AllowedFeature::RequestLimits, AllowedFeature::DemandControl])]
    #[case::auth(test_config_with_auth(), vec![AllowedFeature::Authentication, AllowedFeature::RequestLimits, AllowedFeature::Authorization])]
    async fn restricted_licensed_warn_with_allowed_features_feature_contained_in_allowed_features_claim(
        #[case] config: Arc<Configuration>,
        #[case] allowed_features: Vec<AllowedFeature>,
    ) {
        let router_factory = create_mock_router_configurator(1);
        let (server_factory, shutdown_receivers) = create_mock_server_factory(1);

        assert_matches!(
            execute(
                server_factory,
                router_factory,
                stream::iter(vec![
                    UpdateConfiguration(config),
                    UpdateSchema(example_schema()),
                    UpdateLicense(Arc::new(LicenseState::LicensedWarn {
                        limits: Some(LicenseLimits {
                            tps: None,
                            allowed_features: HashSet::from_iter(allowed_features)
                        })
                    })),
                    Shutdown
                ])
            )
            .await,
            Ok(())
        );
        assert_eq!(shutdown_receivers.0.lock().len(), 1);
    }

    #[test(tokio::test)]
    #[rstest]
    #[case::apq_empty_allowed_features(test_config_with_apq_caching(), vec![])]
    #[case::subscriptions_not_in_allowed_features(test_config_with_subscriptions(), vec![AllowedFeature::ApqCaching])]
    #[case::demand_control_not_in_allowed_features(test_config_with_demand_control(), vec![AllowedFeature::Subscriptions, AllowedFeature::ApqCaching])]
    #[case::request_limits_not_in_allowed_features(test_config_with_request_limits(), vec![AllowedFeature::Subscriptions, AllowedFeature::Subscriptions, AllowedFeature::ApqCaching])]
    #[case::auth_not_in_allowed_features(test_config_with_auth(), vec![AllowedFeature::ApqCaching])]
    async fn restricted_licensed_warn_with_allowed_features_feature_not_contained_in_allowed_features_claim(
        #[case] config: Arc<Configuration>,
        #[case] allowed_features: Vec<AllowedFeature>,
    ) {
        // errors happen before this would be hit; so, 0, but we still need to pass _something_ to
        // execute()
        let router_factory = create_mock_router_configurator(0);
        let (server_factory, shutdown_receivers) = create_mock_server_factory(0);

        assert_matches!(
            execute(
                server_factory,
                router_factory,
                stream::iter(vec![
                    UpdateConfiguration(config),
                    UpdateSchema(example_schema()),
                    UpdateLicense(Arc::new(LicenseState::LicensedWarn {
                        limits: Some(LicenseLimits {
                            tps: None,
                            allowed_features: HashSet::from_iter(allowed_features)
                        })
                    })),
                    Shutdown
                ])
            )
            .await,
            // this is where the real test happens; we expect a license violation if we're using
            // features that aren't being paid for
            Err(ApolloRouterError::LicenseViolation(_))
        );
        assert_eq!(shutdown_receivers.0.lock().len(), 0);
    }

    #[test(tokio::test)]
    #[rstest]
    #[case::apq_empty_allowed_features(test_config_with_apq_caching(), vec![])]
    #[case::subscriptions_not_in_allowed_features(test_config_with_subscriptions(), vec![AllowedFeature::ApqCaching])]
    #[case::demand_control_not_in_allowed_features(test_config_with_demand_control(), vec![AllowedFeature::Subscriptions, AllowedFeature::ApqCaching])]
    #[case::request_limits_not_in_allowed_features(test_config_with_request_limits(), vec![AllowedFeature::Subscriptions, AllowedFeature::Subscriptions, AllowedFeature::DemandControl])]
    #[case::auth_not_in_allowed_features(test_config_with_auth(), vec![AllowedFeature::ApqCaching])]
    async fn restricted_licensed_unlicensed_with_feature_not_contained_in_allowed_features(
        #[case] config: Arc<Configuration>,
        #[case] allowed_features: Vec<AllowedFeature>,
    ) {
        let router_factory = create_mock_router_configurator(0);
        let (server_factory, shutdown_receivers) = create_mock_server_factory(0);

        // The unlicensed event is dropped so we should get a reload
        assert_matches!(
            execute(
                server_factory,
                router_factory,
                stream::iter(vec![
                    UpdateConfiguration(config.clone()),
                    UpdateSchema(example_schema()),
                    UpdateLicense(Arc::new(LicenseState::Licensed {
                        limits: Some(LicenseLimits {
                            tps: None,
                            allowed_features: HashSet::from_iter(allowed_features)
                        })
                    })),
                    UpdateLicense(Arc::new(LicenseState::Unlicensed)),
                    UpdateConfiguration(config),
                    Shutdown
                ])
            )
            .await,
            Err(ApolloRouterError::LicenseViolation(_))
        );
        assert_eq!(shutdown_receivers.0.lock().len(), 0);
    }

    #[test(tokio::test)]
    async fn restricted_unlicensed() {
        let router_factory = create_mock_router_configurator(0);
        let (server_factory, shutdown_receivers) = create_mock_server_factory(0);

        assert_matches!(
            execute(
                server_factory,
                router_factory,
                stream::iter(vec![
                    UpdateConfiguration(test_config_restricted()),
                    UpdateSchema(example_schema()),
                    UpdateLicense(Arc::new(LicenseState::Unlicensed)),
                    Shutdown
                ])
            )
            .await,
            Err(ApolloRouterError::LicenseViolation(_))
        );
        assert_eq!(shutdown_receivers.0.lock().len(), 0);
    }

    // NB: this behavior may change once all licenses contain an `allowed_features` claim
    #[test(tokio::test)]
    async fn unrestricted_unlicensed_reload_with_config_using_restricted_features_and_license() {
        let router_factory = create_mock_router_configurator_for_reload_with_new_license(2);
        let (server_factory, shutdown_receivers) = create_mock_server_factory(2);

        assert_matches!(
            execute(
                server_factory,
                router_factory,
                stream::iter(vec![
                    UpdateConfiguration(Arc::new(Configuration::builder().build().unwrap())),
                    UpdateSchema(example_schema()),
                    UpdateLicense(Arc::new(LicenseState::Unlicensed)),
                    UpdateConfiguration(test_config_restricted()),
                    UpdateLicense(Arc::new(LicenseState::Licensed {
                        limits: Some(LicenseLimits::default())
                    })),
                    Shutdown
                ])
            )
            .await,
            Ok(())
        );
        assert_eq!(shutdown_receivers.0.lock().len(), 2);
    }

    #[test(tokio::test)]
    async fn unrestricted_unlicensed_reload_with_config_using_restricted_feature_still_unlicensed_router_fails_to_reload()
     {
        // Expected times called = 1 since the router failed to reload due to the license violation
        let router_factory = create_mock_router_configurator(1);
        let (server_factory, shutdown_receivers) = create_mock_server_factory(1);

        assert_matches!(
            execute(
                server_factory,
                router_factory,
                stream::iter(vec![
                    UpdateConfiguration(Arc::new(Configuration::builder().build().unwrap())),
                    UpdateSchema(example_schema()),
                    UpdateLicense(Arc::new(LicenseState::Unlicensed)),
                    UpdateConfiguration(test_config_restricted()),
                    UpdateLicense(Arc::new(LicenseState::Unlicensed)),
                    Shutdown
                ])
            )
            .await,
            Ok(())
        );
        assert_eq!(shutdown_receivers.0.lock().len(), 1);
    }

    #[test(tokio::test)]
    #[rstest]
    #[case::apq_empty_allowed_features(test_config_with_apq_caching(), vec![])]
    #[case::subscriptions_not_in_allowed_features(test_config_with_subscriptions(), vec![AllowedFeature::ApqCaching])]
    #[case::demand_control_not_in_allowed_features(test_config_with_demand_control(), vec![AllowedFeature::Subscriptions, AllowedFeature::ApqCaching])]
    #[case::request_limits_not_in_allowed_features(test_config_with_request_limits(), vec![AllowedFeature::Subscriptions, AllowedFeature::Subscriptions, AllowedFeature::DemandControl])]
    #[case::auth_not_in_allowed_features(test_config_with_auth(), vec![AllowedFeature::ApqCaching])]
    async fn unrestricted_unlicensed_restricted_licensed_with_feature_not_contained_in_allowed_features(
        #[case] config: Arc<Configuration>,
        #[case] allowed_features: Vec<AllowedFeature>,
    ) {
        let router_factory = create_mock_router_configurator(1);
        let (server_factory, shutdown_receivers) = create_mock_server_factory(1);

        assert_matches!(
            execute(
                server_factory,
                router_factory,
                stream::iter(vec![
                    UpdateConfiguration(Arc::new(Configuration::builder().build().unwrap())),
                    UpdateSchema(example_schema()),
                    UpdateLicense(Arc::new(LicenseState::Unlicensed)),
                    UpdateConfiguration(config),
                    UpdateLicense(Arc::new(LicenseState::Licensed {
                        limits: Some(LicenseLimits {
                            tps: None,
                            allowed_features: HashSet::from_iter(allowed_features)
                        })
                    })),
                    Shutdown
                ])
            )
            .await,
            Ok(())
        );
        assert_eq!(shutdown_receivers.0.lock().len(), 1);
    }

    #[test(tokio::test)]
    async fn restricted_licensed_with_allowed_features_containing_feature_reload_with_empty_feature_set()
     {
        let router_factory = create_mock_router_configurator(1);
        let (server_factory, shutdown_receivers) = create_mock_server_factory(1);

        assert_matches!(
            execute(
                server_factory,
                router_factory,
                stream::iter(vec![
                    UpdateConfiguration(test_config_with_subscriptions()),
                    UpdateSchema(example_schema()),
                    UpdateLicense(Arc::new(LicenseState::Licensed {
                        limits: Some(LicenseLimits {
                            tps: None,
                            allowed_features: HashSet::from_iter(vec![
                                AllowedFeature::Subscriptions
                            ])
                        })
                    })),
                    UpdateLicense(Arc::new(LicenseState::Licensed {
                        limits: Some(LicenseLimits {
                            tps: None,
                            allowed_features: HashSet::new()
                        })
                    })),
                    Shutdown
                ])
            )
            .await,
            Ok(())
        );
        assert_eq!(shutdown_receivers.0.lock().len(), 1);
    }

    #[test(tokio::test)]
    async fn unlicensed_reload_with_license_and_use_feature_enabled_by_that_license() {
        let router_factory = create_mock_router_configurator_for_reload_with_new_license(3);
        let (server_factory, shutdown_receivers) = create_mock_server_factory(3);

        assert_matches!(
            execute(
                server_factory,
                router_factory,
                stream::iter(vec![
                    UpdateConfiguration(Arc::new(Configuration::builder().build().unwrap())),
                    UpdateLicense(Arc::new(LicenseState::Unlicensed)),
                    UpdateSchema(example_schema()),
                    UpdateLicense(Arc::new(LicenseState::Licensed {
                        limits: Some(LicenseLimits {
                            tps: None,
                            allowed_features: HashSet::from_iter(vec![
                                AllowedFeature::Subscriptions,
                            ])
                        })
                    })),
                    UpdateConfiguration(test_config_with_subscriptions()),
                    Shutdown
                ])
            )
            .await,
            Ok(())
        );
        assert_eq!(shutdown_receivers.0.lock().len(), 3);
    }

    #[test(tokio::test)]
    async fn unlicensed_reload_with_license_and_use_feature_not_enabled_by_that_license() {
        let router_factory = create_mock_router_configurator_for_reload_with_new_license(2);
        let (server_factory, shutdown_receivers) = create_mock_server_factory(2);

        assert_matches!(
            execute(
                server_factory,
                router_factory,
                stream::iter(vec![
                    UpdateConfiguration(Arc::new(Configuration::builder().build().unwrap())),
                    UpdateLicense(Arc::new(LicenseState::Unlicensed)),
                    UpdateSchema(example_schema()),
                    UpdateLicense(Arc::new(LicenseState::Licensed {
                        limits: Some(LicenseLimits {
                            tps: None,
                            allowed_features: HashSet::from_iter(vec![
                                AllowedFeature::DemandControl,
                            ])
                        })
                    })),
                    UpdateConfiguration(test_config_with_subscriptions()),
                    Shutdown
                ])
            )
            .await,
            Ok(())
        );
        assert_eq!(shutdown_receivers.0.lock().len(), 2);
    }

    // NB: this behavior will change once all licenses have an `allowed_features` claim
    #[test(tokio::test)]
    async fn unlicensed_reload_with_license_using_default_limits() {
        let router_factory = create_mock_router_configurator_for_reload_with_new_license(3);
        let (server_factory, shutdown_receivers) = create_mock_server_factory(3);

        assert_matches!(
            execute(
                server_factory,
                router_factory,
                stream::iter(vec![
                    UpdateConfiguration(Arc::new(Configuration::builder().build().unwrap())),
                    UpdateLicense(Arc::new(LicenseState::Unlicensed)),
                    UpdateSchema(example_schema()),
                    UpdateLicense(Arc::new(LicenseState::Licensed {
                        limits: Default::default()
                    })),
                    UpdateConfiguration(test_config_with_subscriptions()),
                    Shutdown
                ])
            )
            .await,
            Ok(())
        );
        assert_eq!(shutdown_receivers.0.lock().len(), 3);
    }

    #[test(tokio::test)]
    async fn licensed_with_feature_contained_in_allowed_features_reload_with_feature_set_not_containing_feature_used()
     {
        let router_factory = create_mock_router_configurator(1);
        let (server_factory, shutdown_receivers) = create_mock_server_factory(1);

        assert_matches!(
            execute(
                server_factory,
                router_factory,
                stream::iter(vec![
                    UpdateConfiguration(test_config_with_subscriptions()),
                    UpdateSchema(example_schema()),
                    UpdateLicense(Arc::new(LicenseState::Licensed {
                        limits: Some(LicenseLimits {
                            tps: None,
                            allowed_features: HashSet::from_iter(vec![
                                AllowedFeature::Subscriptions,
                            ])
                        })
                    })),
                    UpdateLicense(Arc::new(LicenseState::Licensed {
                        limits: Some(LicenseLimits {
                            tps: None,
                            allowed_features: HashSet::from_iter(vec![
                                AllowedFeature::Authentication,
                                AllowedFeature::Authorization
                            ])
                        })
                    })),
                    Shutdown
                ])
            )
            .await,
            Ok(())
        );
        assert_eq!(shutdown_receivers.0.lock().len(), 1);
    }

    #[test(tokio::test)]
    async fn licensed_with_feature_contained_in_allowed_features_reload_with_feature_set_still_containing_restricted_feature_in_use()
     {
        let router_factory = create_mock_router_configurator_for_reload_with_new_license(2);
        let (server_factory, shutdown_receivers) = create_mock_server_factory(2);

        assert_matches!(
            execute(
                server_factory,
                router_factory,
                stream::iter(vec![
                    UpdateConfiguration(test_config_with_subscriptions()),
                    UpdateSchema(example_schema()),
                    UpdateLicense(Arc::new(LicenseState::Licensed {
                        limits: Some(LicenseLimits {
                            tps: None,
                            allowed_features: HashSet::from_iter(vec![
                                AllowedFeature::Subscriptions,
                            ])
                        })
                    })),
                    UpdateLicense(Arc::new(LicenseState::Licensed {
                        limits: Some(LicenseLimits {
                            tps: None,
                            allowed_features: HashSet::from_iter(vec![
                                AllowedFeature::Authentication,
                                AllowedFeature::Authorization,
                                AllowedFeature::Subscriptions,
                            ])
                        })
                    })),
                    Shutdown
                ])
            )
            .await,
            Ok(())
        );
        assert_eq!(shutdown_receivers.0.lock().len(), 2);
    }

    // NB: This behavior will change once all licenses have an `allowed_features` claim
    #[test(tokio::test)]
    async fn licensed_with_feature_contained_in_allowed_features_reload_with_license_with_default_limits()
     {
        let router_factory = create_mock_router_configurator_for_reload_with_new_license(2);
        let (server_factory, shutdown_receivers) = create_mock_server_factory(2);

        assert_matches!(
            execute(
                server_factory,
                router_factory,
                stream::iter(vec![
                    UpdateConfiguration(test_config_with_subscriptions()),
                    UpdateSchema(example_schema()),
                    UpdateLicense(Arc::new(LicenseState::Licensed {
                        limits: Some(LicenseLimits {
                            tps: None,
                            allowed_features: HashSet::from_iter(vec![
                                AllowedFeature::Subscriptions,
                            ])
                        })
                    })),
                    UpdateLicense(Arc::new(LicenseState::Licensed {
                        limits: Default::default()
                    })),
                    Shutdown
                ])
            )
            .await,
            Ok(())
        );
        assert_eq!(shutdown_receivers.0.lock().len(), 2);
    }

    // NB: this behavior will change once all licenses have an `allowed_features` claim
    #[test(tokio::test)]
    async fn restricted_licensed_with_default_license_limits() {
        let router_factory = create_mock_router_configurator(1);
        let (server_factory, shutdown_receivers) = create_mock_server_factory(1);

        assert_matches!(
            execute(
                server_factory,
                router_factory,
                stream::iter(vec![
                    UpdateConfiguration(test_config_with_subscriptions()),
                    UpdateSchema(example_schema()),
                    UpdateLicense(Arc::new(LicenseState::Licensed {
                        limits: Default::default()
                    })),
                    Shutdown
                ])
            )
            .await,
            Ok(())
        );
        assert_eq!(shutdown_receivers.0.lock().len(), 1);
    }

    #[test(tokio::test)]
    async fn listen_addresses_are_locked() {
        let router_factory = create_mock_router_configurator(0);
        let (server_factory, _) = create_mock_server_factory(0);
        let is_telemetry_disabled = false;
        let state_machine =
            StateMachine::new(is_telemetry_disabled, server_factory, router_factory);
        assert!(state_machine.listen_addresses.try_read().is_err());
    }

    #[test(tokio::test)]
    async fn shutdown_during_startup() {
        let router_factory = create_mock_router_configurator(0);
        let (server_factory, _) = create_mock_server_factory(0);
        assert_matches!(
            execute(server_factory, router_factory, stream::iter(vec![Shutdown])).await,
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
                stream::iter(vec![
                    UpdateConfiguration(Arc::new(Configuration::builder().build().unwrap())),
                    UpdateSchema(example_schema()),
                    UpdateLicense(Default::default()),
                    Shutdown
                ])
            )
            .await,
            Ok(())
        );
        assert_eq!(shutdown_receivers.0.lock().len(), 1);
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
                stream::iter(vec![
                    UpdateConfiguration(Arc::new(Configuration::builder().build().unwrap())),
                    UpdateSchema(SchemaState {
                        sdl: minimal_schema.to_owned(),
                        launch_id: None
                    }),
                    UpdateLicense(Default::default()),
                    UpdateSchema(example_schema()),
                    Shutdown
                ])
            )
            .await,
            Ok(())
        );
        assert_eq!(shutdown_receivers.0.lock().len(), 2);
    }

    #[test(tokio::test)]
    async fn startup_no_reload_schema() {
        let router_factory = create_mock_router_configurator(1);
        let (server_factory, shutdown_receivers) = create_mock_server_factory(1);
        let minimal_schema = include_str!("testdata/minimal_supergraph.graphql");
        assert_matches!(
            execute(
                server_factory,
                router_factory,
                stream::iter(vec![
                    UpdateConfiguration(Arc::new(Configuration::builder().build().unwrap())),
                    UpdateSchema(SchemaState {
                        sdl: minimal_schema.to_owned(),
                        launch_id: None
                    }),
                    UpdateLicense(Default::default()),
                    UpdateSchema(SchemaState {
                        sdl: minimal_schema.to_owned(),
                        launch_id: None
                    }),
                    Shutdown
                ])
            )
            .await,
            Ok(())
        );
        assert_eq!(shutdown_receivers.0.lock().len(), 1);
    }

    #[test(tokio::test)]
    async fn startup_reload_license() {
        let router_factory = create_mock_router_configurator(2);
        let (server_factory, shutdown_receivers) = create_mock_server_factory(2);
        let minimal_schema = include_str!("testdata/minimal_supergraph.graphql");
        assert_matches!(
            execute(
                server_factory,
                router_factory,
                stream::iter(vec![
                    UpdateConfiguration(Arc::new(Configuration::builder().build().unwrap())),
                    UpdateSchema(SchemaState {
                        sdl: minimal_schema.to_owned(),
                        launch_id: None
                    }),
                    UpdateLicense(Default::default()),
                    UpdateLicense(Arc::new(LicenseState::Licensed {
                        limits: Some(LicenseLimits::default())
                    })),
                    Shutdown
                ])
            )
            .await,
            Ok(())
        );
        assert_eq!(shutdown_receivers.0.lock().len(), 2);
    }

    #[test(tokio::test)]
    async fn startup_reload_configuration() {
        let router_factory = create_mock_router_configurator(2);
        let (server_factory, shutdown_receivers) = create_mock_server_factory(2);

        assert_matches!(
            execute(
                server_factory,
                router_factory,
                stream::iter(vec![
                    UpdateConfiguration(Arc::new(Configuration::builder().build().unwrap())),
                    UpdateSchema(example_schema()),
                    UpdateLicense(Default::default()),
                    UpdateConfiguration(Arc::new(
                        Configuration::builder()
                            .supergraph(
                                crate::configuration::Supergraph::builder()
                                    .listen(SocketAddr::from_str("127.0.0.1:4001").unwrap())
                                    .build()
                            )
                            .build()
                            .unwrap()
                    )),
                    Shutdown
                ])
            )
            .await,
            Ok(())
        );
        assert_eq!(shutdown_receivers.0.lock().len(), 2);
    }

    #[test(tokio::test)]
    async fn extract_routing_urls() {
        let router_factory = create_mock_router_configurator(1);
        let (server_factory, shutdown_receivers) = create_mock_server_factory(1);

        assert_matches!(
            execute(
                server_factory,
                router_factory,
                stream::iter(vec![
                    UpdateConfiguration(Arc::new(Configuration::builder().build().unwrap())),
                    UpdateSchema(example_schema()),
                    UpdateLicense(Default::default()),
                    Shutdown
                ])
            )
            .await,
            Ok(())
        );
        assert_eq!(shutdown_receivers.0.lock().len(), 1);
    }

    #[test(tokio::test)]
    async fn router_factory_error_startup() {
        let mut router_factory = MockMyRouterConfigurator::new();
        router_factory
            .expect_create()
            .times(1)
            .returning(|_, _, _, _, _, _| Err(BoxError::from("Error")));

        let (server_factory, shutdown_receivers) = create_mock_server_factory(0);

        assert_matches!(
            execute(
                server_factory,
                router_factory,
                stream::iter(vec![
                    UpdateConfiguration(Arc::new(Configuration::builder().build().unwrap())),
                    UpdateSchema(example_schema()),
                    UpdateLicense(Default::default()),
                ])
            )
            .await,
            Err(ApolloRouterError::ServiceCreationError(_))
        );
        assert_eq!(shutdown_receivers.0.lock().len(), 0);
    }

    #[test(tokio::test)]
    async fn router_factory_error_restart() {
        let mut seq = Sequence::new();
        let mut router_factory = MockMyRouterConfigurator::new();
        router_factory
            .expect_create()
            .times(1)
            .in_sequence(&mut seq)
            .returning(|_, _, _, _, _, _| {
                let mut router = MockMyRouterFactory::new();
                router.expect_clone().return_once(MockMyRouterFactory::new);
                router.expect_web_endpoints().returning(MultiMap::new);
                Ok(router)
            });
        router_factory
            .expect_create()
            .times(1)
            .in_sequence(&mut seq)
            .returning(|_, _, _, _, _, _| Err(BoxError::from("error")));

        let (server_factory, shutdown_receivers) = create_mock_server_factory(1);
        let minimal_schema = include_str!("testdata/minimal_supergraph.graphql");

        assert_matches!(
            execute(
                server_factory,
                router_factory,
                stream::iter(vec![
                    UpdateConfiguration(Arc::new(Configuration::builder().build().unwrap())),
                    UpdateSchema(example_schema()),
                    UpdateLicense(Default::default()),
                    UpdateSchema(SchemaState {
                        sdl: minimal_schema.to_owned(),
                        launch_id: None
                    }),
                    Shutdown
                ])
            )
            .await,
            Ok(())
        );
        assert_eq!(shutdown_receivers.0.lock().len(), 1);
    }

    #[test(tokio::test)]
    async fn router_factory_ok_error_restart() {
        let mut seq = Sequence::new();
        let mut router_factory = MockMyRouterConfigurator::new();
        router_factory
            .expect_create()
            .times(1)
            .in_sequence(&mut seq)
            .returning(|_, _, _, _, _, _| {
                let mut router = MockMyRouterFactory::new();
                router.expect_clone().return_once(MockMyRouterFactory::new);
                router.expect_web_endpoints().returning(MultiMap::new);
                Ok(router)
            });
        router_factory
            .expect_create()
            .times(1)
            .in_sequence(&mut seq)
            .returning(|_, _, _, _, _, _| Err(BoxError::from("error")));
        router_factory
            .expect_create()
            .times(1)
            .in_sequence(&mut seq)
            .withf(|_, configuration, _, _, _, _| configuration.homepage.enabled)
            .returning(|_, _, _, _, _, _| {
                let mut router = MockMyRouterFactory::new();
                router.expect_clone().return_once(MockMyRouterFactory::new);
                router.expect_web_endpoints().returning(MultiMap::new);
                Ok(router)
            });

        let (server_factory, shutdown_receivers) = create_mock_server_factory(2);
        let minimal_schema = include_str!("testdata/minimal_supergraph.graphql");

        assert_matches!(
            execute(
                server_factory,
                router_factory,
                stream::iter(vec![
                    UpdateConfiguration(Arc::new(Configuration::builder().build().unwrap())),
                    UpdateSchema(example_schema()),
                    UpdateLicense(Default::default()),
                    UpdateConfiguration(Arc::new(
                        Configuration::builder()
                            .homepage(Homepage::builder().enabled(true).build())
                            .build()
                            .unwrap()
                    )),
                    UpdateSchema(SchemaState {
                        sdl: minimal_schema.to_owned(),
                        launch_id: None
                    }),
                    Shutdown
                ]),
            )
            .await,
            Ok(())
        );
        assert_eq!(shutdown_receivers.0.lock().len(), 2);
    }

    #[test(tokio::test)]
    async fn state_change_metrics() {
        let router_factory = create_mock_router_configurator(2);
        let (server_factory, _) = create_mock_server_factory(2);
        let minimal_schema = include_str!("testdata/minimal_supergraph.graphql");
        async {
            assert_matches!(
                execute(
                    server_factory,
                    router_factory,
                    stream::iter(vec![
                        UpdateConfiguration(Arc::new(Configuration::builder().build().unwrap())),
                        NoMoreConfiguration,
                        UpdateSchema(SchemaState {
                            sdl: minimal_schema.to_owned(),
                            launch_id: None
                        }),
                        NoMoreSchema,
                        UpdateLicense(Default::default()),
                        NoMoreLicense,
                        Reload,
                        RhaiReload,
                        Shutdown
                    ])
                )
                .await,
                Ok(())
            );

            assert_counter!(
                "apollo.router.state.change.total",
                1,
                "event" = "UpdateConfiguration",
                "previous_state" = "Startup",
                "state" = "Startup"
            );
            assert_counter!(
                "apollo.router.state.change.total",
                1,
                "event" = "NoMoreConfiguration",
                "previous_state" = "Startup",
                "state" = "Startup"
            );
            assert_counter!(
                "apollo.router.state.change.total",
                1,
                "event" = "UpdateSchema",
                "previous_state" = "Startup",
                "state" = "Startup"
            );
            assert_counter!(
                "apollo.router.state.change.total",
                1,
                "event" = "NoMoreSchema",
                "previous_state" = "Startup",
                "state" = "Startup"
            );
            assert_counter!(
                "apollo.router.state.change.total",
                1,
                "event" = "UpdateLicense(Unlicensed)",
                "previous_state" = "Startup",
                "state" = "Running"
            );
            assert_counter!(
                "apollo.router.state.change.total",
                1,
                "event" = "NoMoreLicense",
                "previous_state" = "Running",
                "state" = "Running"
            );
            assert_counter!(
                "apollo.router.state.change.total",
                1,
                "event" = "ForcedHotReload",
                "previous_state" = "Running",
                "state" = "Running"
            );
            assert_counter!(
                "apollo.router.state.change.total",
                1,
                "event" = "RhaiReload",
                "previous_state" = "Running",
                "state" = "Running"
            );
            assert_counter!(
                "apollo.router.state.change.total",
                1,
                "event" = "Shutdown",
                "previous_state" = "Running",
                "state" = "Stopped"
            );
        }
        .with_metrics()
        .await;
    }

    mock! {
        #[derive(Debug)]
        MyRouterConfigurator {}

        #[async_trait::async_trait]
        impl RouterSuperServiceFactory for MyRouterConfigurator {
            type RouterFactory = MockMyRouterFactory;

            async fn create<'a>(
                &'a mut self,
                is_telemetry_disabled: bool,
                configuration: Arc<Configuration>,
                schema: Arc<Schema>,
                previous_router_service_factory: Option<&'a MockMyRouterFactory>,
                extra_plugins: Option<Vec<(String, Box<dyn DynPlugin>)>>,
                license: Arc<LicenseState>
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
            fn pipeline_ref(&self) -> Arc<PipelineRef>;
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

            _license: Arc<LicenseState>,
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
        events: impl Stream<Item = Event> + Unpin,
    ) -> Result<(), ApolloRouterError> {
        let is_telemetry_disabled = false;
        let state_machine =
            StateMachine::new(is_telemetry_disabled, server_factory, router_factory);
        state_machine.process_events(events).await
    }

    fn create_mock_server_factory(
        expect_times_called: usize,
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
                    shutdown_receivers_clone.lock().push(shutdown_receiver);
                    extra_shutdown_receivers_clone
                        .lock()
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
            .returning(move |_, _, _, _, _, _| {
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
                    move |_,
                          _configuration: &Arc<Configuration>,
                          _,
                          previous_router_service_factory: &Option<&MockMyRouterFactory>,
                          _extra_plugins: &Option<Vec<(String, Box<dyn DynPlugin>)>>,
                          _| { previous_router_service_factory.is_some() },
                )
                .returning(move |_, _, _, _, _, _| {
                    let mut router = MockMyRouterFactory::new();
                    router.expect_clone().return_once(MockMyRouterFactory::new);
                    router.expect_web_endpoints().returning(MultiMap::new);
                    Ok(router)
                });
        }

        router_factory
    }

    fn create_mock_router_configurator_for_reload_with_new_license(
        expect_times_called: usize,
    ) -> MockMyRouterConfigurator {
        let mut router_factory = MockMyRouterConfigurator::new();

        router_factory
            .expect_create()
            .times(expect_times_called)
            .returning(move |_, _, _, _, _, _| {
                let mut router = MockMyRouterFactory::new();
                router.expect_clone().return_once(MockMyRouterFactory::new);
                router.expect_web_endpoints().returning(MultiMap::new);
                Ok(router)
            });

        router_factory
    }

    // Tests for the Reloading state: retry-on-failure, timer-driven retries,
    // configurable budgets, and event-driven immediate retries.
    mod reload {
        use std::str::FromStr;
        use std::time::Duration;

        use mockall::Sequence;
        use test_log::test;
        use tokio::sync::Notify;
        use tokio_stream::wrappers::ReceiverStream;
        use tower::BoxError;

        use super::*;

        fn mock_router_ok() -> MockMyRouterFactory {
            let mut router = MockMyRouterFactory::new();
            router.expect_clone().return_once(MockMyRouterFactory::new);
            router.expect_web_endpoints().returning(MultiMap::new);
            router
        }

        /// Build a factory from a sequence of outcomes: `Ok(())` → one successful
        /// `create()` call, `Err(())` → one failing call.  Ordering is enforced by a
        /// mockall `Sequence` (the Arc counter inside it outlives the helper frame).
        fn factory(outcomes: &[Result<(), ()>]) -> MockMyRouterConfigurator {
            let mut seq = Sequence::new();
            let mut factory = MockMyRouterConfigurator::new();
            for &outcome in outcomes {
                factory
                    .expect_create()
                    .times(1)
                    .in_sequence(&mut seq)
                    .returning(move |_, _, _, _, _, _| {
                        if outcome.is_ok() {
                            Ok(mock_router_ok())
                        } else {
                            Err(BoxError::from("transient error"))
                        }
                    });
            }
            factory
        }

        fn minimal_schema() -> SchemaState {
            SchemaState {
                sdl: include_str!("testdata/minimal_supergraph.graphql").to_owned(),
                launch_id: None,
            }
        }

        /// Drives a state machine through a reload scenario.
        ///
        /// Creates the channel, spawns `process_events`, and provides
        /// `send_and_wait` / `advance_and_wait` helpers so each test only
        /// expresses the interesting sequence of events.
        struct Harness {
            tx: tokio::sync::mpsc::Sender<Event>,
            notify: Arc<Notify>,
            handle: tokio::task::JoinHandle<Result<(), ApolloRouterError>>,
        }

        impl Harness {
            fn new(router_factory: MockMyRouterConfigurator, server_starts: usize) -> Self {
                let (server_factory, _shutdown_receivers) =
                    create_mock_server_factory(server_starts);
                let notify = Arc::new(Notify::new());
                let state_machine =
                    StateMachine::for_tests(server_factory, router_factory, notify.clone());
                let (tx, rx) = tokio::sync::mpsc::channel::<Event>(16);
                let stream = ReceiverStream::new(rx);
                let handle = tokio::spawn(state_machine.process_events(stream));
                Self { tx, notify, handle }
            }

            /// Send an event and wait for the state machine to acknowledge it.
            async fn send_and_wait(&self, event: Event) {
                let notified = self.notify.notified();
                self.tx.send(event).await.unwrap();
                notified.await;
            }

            /// Advance the Tokio mock clock and wait for the retry timer to fire.
            async fn advance_and_wait(&self, duration: Duration) {
                let notified = self.notify.notified();
                tokio::time::advance(duration).await;
                notified.await;
            }

            /// Send the three startup events and wait for each, bringing the
            /// state machine to `Running` with default configuration.
            async fn startup(&self) {
                self.startup_with_config(Arc::new(Configuration::default()))
                    .await;
            }

            /// Like `startup`, but uses `config` instead of the default.
            async fn startup_with_config(&self, config: Arc<Configuration>) {
                self.send_and_wait(UpdateConfiguration(config)).await;
                self.send_and_wait(UpdateSchema(example_schema())).await;
                self.send_and_wait(UpdateLicense(Default::default())).await;
            }

            /// Shut down and assert the state machine exited cleanly.
            /// Dropping `self` also drops the mock router factory, which causes
            /// mockall to assert that all expected `create()` calls were made.
            async fn finish(self) {
                self.tx.send(Shutdown).await.unwrap();
                drop(self.tx);
                self.handle.await.unwrap().unwrap();
            }
        }

        // After a reload attempt fails, the state machine should automatically retry
        // after a delay (without requiring a new external event from Uplink). This
        // prevents routers from being permanently stuck on a stale schema when a
        // transient error (e.g. NAT port exhaustion during a burst) causes try_start
        // to fail.
        #[test(tokio::test(start_paused = true))]
        async fn with_retry() {
            // startup ok, reload fails, retry succeeds
            let harness = Harness::new(factory(&[Ok(()), Err(()), Ok(())]), 2);
            harness.startup().await;
            harness.send_and_wait(UpdateSchema(minimal_schema())).await; // reload fails
            harness.advance_and_wait(Duration::from_secs(11)).await; // timer fires, retry succeeds
            harness.finish().await;
        }

        #[test(tokio::test(start_paused = true))]
        async fn repeated_retry() {
            // startup ok, reload fails, first retry fails, second retry succeeds
            let harness = Harness::new(factory(&[Ok(()), Err(()), Err(()), Ok(())]), 2);
            harness.startup().await;
            harness.send_and_wait(UpdateSchema(minimal_schema())).await; // initial reload fails
            harness.advance_and_wait(Duration::from_secs(11)).await; // first retry fails
            harness.advance_and_wait(Duration::from_secs(11)).await; // second retry succeeds
            harness.finish().await;
        }

        // A newer schema arrives while we are still in Reloading (e.g. Uplink publishes
        // again before the retry timer fires).  The state machine should immediately
        // re-attempt with the new schema via the event arm — no clock advance needed.
        #[test(tokio::test(start_paused = true))]
        async fn new_schema_while_reloading() {
            // startup ok, reload fails, new-schema event triggers immediate retry → ok
            let harness = Harness::new(factory(&[Ok(()), Err(()), Ok(())]), 2);
            harness.startup().await;
            harness.send_and_wait(UpdateSchema(minimal_schema())).await; // initial reload fails
            // Send a newer schema before the timer fires — retries immediately.
            harness.send_and_wait(UpdateSchema(example_schema())).await;
            harness.finish().await;
        }

        // max_retries: 1 means one retry after the initial attempt — two total creates.
        // This catches the off-by-one where the initial attempt consumes the whole budget.
        #[test(tokio::test(start_paused = true))]
        async fn one_retry() {
            let one_retry = Arc::new(
                Configuration::from_str("reload:\n  max_retries: 1")
                    .expect("config with max_retries: 1 must be valid"),
            );
            let harness = Harness::new(factory(&[Ok(()), Err(()), Ok(())]), 2);
            harness.startup_with_config(one_retry).await;
            harness.send_and_wait(UpdateSchema(minimal_schema())).await; // initial attempt fails
            harness.advance_and_wait(Duration::from_secs(11)).await; // one retry succeeds
            harness.finish().await;
        }

        // With max_retries: 0 the retry timer is never armed.  The router should keep
        // serving the committed state until a new event arrives, and Shutdown should
        // still work cleanly.
        #[test(tokio::test(start_paused = true))]
        async fn retries_exhausted() {
            // startup ok, reload fails — no timer retry scheduled with max_retries: 0
            let zero_retries = Arc::new(
                Configuration::from_str("reload:\n  max_retries: 0")
                    .expect("config with max_retries: 0 must be valid"),
            );
            let harness = Harness::new(factory(&[Ok(()), Err(())]), 1);
            harness.startup_with_config(zero_retries).await;
            harness.send_and_wait(UpdateSchema(minimal_schema())).await; // fails, no retry scheduled
            // Advance well past any retry delay — the timer must not fire.
            tokio::time::advance(Duration::from_secs(60)).await;
            harness.finish().await;
        }

        // After the retry budget is exhausted (timer disabled), a new schema event
        // resets the budget and triggers an immediate retry via the event arm.
        #[test(tokio::test(start_paused = true))]
        async fn budget_reset_after_exhaustion() {
            // startup ok, reload fails (budget exhausted), new schema resets budget → ok
            let zero_retries = Arc::new(
                Configuration::from_str("reload:\n  max_retries: 0")
                    .expect("config with max_retries: 0 must be valid"),
            );
            let harness = Harness::new(factory(&[Ok(()), Err(()), Ok(())]), 2);
            harness.startup_with_config(zero_retries).await;
            harness.send_and_wait(UpdateSchema(minimal_schema())).await; // fails, budget exhausted
            harness.send_and_wait(UpdateSchema(example_schema())).await; // resets + retries immediately
            harness.finish().await;
        }

        // A RhaiReload event arrives while the state machine is already in Reloading
        // (e.g. a configuration reload failed and a concurrent rhai script change is
        // detected before the retry timer fires).  The state machine must retry
        // immediately via the event arm without waiting for the timer.
        #[test(tokio::test(start_paused = true))]
        async fn rhai_reload_while_reloading() {
            // startup ok, config reload fails, RhaiReload triggers immediate retry → ok
            let harness = Harness::new(factory(&[Ok(()), Err(()), Ok(())]), 2);
            harness.startup().await;
            // Trigger a failing configuration reload (distinct from the startup config
            // so accumulate_inputs sees a change).
            harness
                .send_and_wait(UpdateConfiguration(Arc::new(Configuration::default())))
                .await;
            // Rhai script change arrives before the retry timer — retries immediately.
            harness.send_and_wait(RhaiReload).await;
            harness.finish().await;
        }

        // When a failing reload carries a new retry_delay in its pending config,
        // the retry timer must use that new delay (from target()), not the old one
        // (from committed()).  If the bug were reintroduced (using committed()), the
        // timer would fire at 10 s, the notification would be consumed during the
        // first advance, and advance_and_wait would hang because there is nothing
        // left to wake it.
        #[test(tokio::test(start_paused = true))]
        async fn retry_uses_pending_config_delay() {
            let long_delay = Arc::new(
                Configuration::from_str("reload:\n  retry_delay: 30s")
                    .expect("config with retry_delay: 30s must be valid"),
            );
            let harness = Harness::new(factory(&[Ok(()), Err(()), Ok(())]), 2);
            harness.startup().await;
            // The failing reload carries a 30 s retry_delay as the pending config.
            harness.send_and_wait(UpdateConfiguration(long_delay)).await;
            // Advance past the old default (10 s) but not the new delay (30 s).
            tokio::time::advance(Duration::from_secs(11)).await;
            // Advance past the new delay — the timer fires and the retry succeeds.
            harness.advance_and_wait(Duration::from_secs(21)).await;
            harness.finish().await;
        }

        // With max_retries: null the retry timer must keep firing past the default
        // limit of 5 retries.  This test fails 6 times (one more than the default)
        // and then succeeds, proving the budget is truly unlimited.
        #[test(tokio::test(start_paused = true))]
        async fn unlimited_retries() {
            // startup ok, then 6 failures (one more than the default max_retries: 5),
            // then success — proves the None budget never stops the timer.
            let mut outcomes = vec![Ok(())];
            outcomes.extend(std::iter::repeat_n(Err(()), 6));
            outcomes.push(Ok(()));
            let unlimited = Arc::new(
                Configuration::from_str("reload:\n  max_retries: null")
                    .expect("config with max_retries: null must be valid"),
            );
            let harness = Harness::new(factory(&outcomes), 2);
            harness.startup_with_config(unlimited).await;
            harness.send_and_wait(UpdateSchema(minimal_schema())).await; // attempt 1 fails
            // Five more timer-driven failures — total 6, one more than the default max.
            for _ in 0..5 {
                harness.advance_and_wait(Duration::from_secs(11)).await;
            }
            harness.advance_and_wait(Duration::from_secs(11)).await; // 7th attempt succeeds
            harness.finish().await;
        }

        // Verify that the router can perform more than two consecutive successful reloads
        // without getting stuck.  Each UpdateSchema event should transition from the
        // previous Running state to a fresh Running state via Reloading.
        #[test(tokio::test)]
        async fn multiple_successive_reloads() {
            // startup + three successful reloads = four total factory creates
            let harness = Harness::new(factory(&[Ok(()), Ok(()), Ok(()), Ok(())]), 4);
            harness.startup().await;
            harness.send_and_wait(UpdateSchema(minimal_schema())).await;
            harness.send_and_wait(UpdateSchema(example_schema())).await;
            harness.send_and_wait(UpdateSchema(minimal_schema())).await;
            harness.finish().await;
        }
    }
}
