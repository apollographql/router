// With regards to ELv2 licensing, this entire file is license key functionality
#![allow(missing_docs)] // FIXME
#![allow(deprecated)] // Note: Required to prevents complaints on enum declaration

use std::fs;
use std::net::IpAddr;
use std::path::Path;
use std::path::PathBuf;
use std::pin::Pin;
use std::str::FromStr;
use std::sync::Arc;
use std::task::Context;
use std::task::Poll;
use std::time::Duration;

use derivative::Derivative;
use derive_more::Display;
use derive_more::From;
use displaydoc::Display as DisplayDoc;
use futures::channel::oneshot;
use futures::prelude::*;
use futures::FutureExt;
use http_body::Body as _;
use hyper::Body;
use thiserror::Error;
use tokio::sync::RwLock;
use tokio::task::spawn;
use tower::BoxError;
use tower::ServiceExt;
use tracing_futures::WithSubscriber;
use url::Url;

use self::Event::NoMoreConfiguration;
use self::Event::NoMoreSchema;
use self::Event::Shutdown;
use self::Event::UpdateConfiguration;
use self::Event::UpdateSchema;
use crate::axum_factory::make_axum_router;
use crate::axum_factory::AxumHttpServerFactory;
use crate::axum_factory::ListenAddrAndRouter;
use crate::configuration::Configuration;
use crate::configuration::ListenAddr;
use crate::orbiter::OrbiterRouterSuperServiceFactory;
use crate::plugin::DynPlugin;
use crate::router::Event::NoMoreEntitlement;
use crate::router::Event::UpdateEntitlement;
use crate::router_factory::RouterFactory;
use crate::router_factory::RouterSuperServiceFactory;
use crate::router_factory::YamlRouterFactory;
use crate::services::router;
use crate::spec::Schema;
use crate::state_machine::ListenAddresses;
use crate::state_machine::StateMachine;
use crate::uplink::entitlement::Entitlement;
use crate::uplink::entitlement_stream::EntitlementRequest;
use crate::uplink::schema_stream::SupergraphSdlQuery;
use crate::uplink::stream_from_uplink;
use crate::uplink::Endpoints;

// For now this is unused:
// TODO: Check with simon once the refactor is complete
#[allow(unused)]
// Later we might add a public API for this (probably a builder similar to `test_harness.rs`),
// see https://github.com/apollographql/router/issues/1496.
// In the meantime keeping this function helps make sure it still compiles.
async fn make_router_service<RF>(
    schema: &str,
    configuration: Arc<Configuration>,
    extra_plugins: Vec<(String, Box<dyn DynPlugin>)>,
) -> Result<router::BoxCloneService, BoxError> {
    let schema = Arc::new(Schema::parse(schema, &configuration)?);
    let service_factory = YamlRouterFactory
        .create(configuration.clone(), schema, None, Some(extra_plugins))
        .await?;
    let web_endpoints = service_factory.web_endpoints();
    let routers = make_axum_router(service_factory, &configuration, web_endpoints)?;
    let ListenAddrAndRouter(_listener, router) = routers.main;

    Ok(router
        .map_request(|req: router::Request| req.router_request)
        .map_err(|error| match error {})
        .map_response(|res| {
            res.map(|body| {
                // Axum makes this `body` have type:
                // https://docs.rs/http-body/0.4.5/http_body/combinators/struct.UnsyncBoxBody.html
                let mut body = Box::pin(body);
                // We make a stream based on its `poll_data` method
                // in order to create a `hyper::Body`.
                Body::wrap_stream(stream::poll_fn(move |ctx| body.as_mut().poll_data(ctx)))
                // … but we ignore the `poll_trailers` method:
                // https://docs.rs/http-body/0.4.5/http_body/trait.Body.html#tymethod.poll_trailers
                // Apparently HTTP/2 trailers are like headers, except after the response body.
                // I (Simon) believe nothing in the Apollo Router uses trailers as of this writing,
                // so ignoring `poll_trailers` is fine.
                // If we want to use trailers, we may need remove this convertion to `hyper::Body`
                // and return `UnsyncBoxBody` (a.k.a. `axum::BoxBody`) as-is.
            })
            .into()
        })
        .boxed_clone())
}

/// Error types for FederatedServer.
#[derive(Error, Debug, DisplayDoc)]
pub enum ApolloRouterError {
    /// failed to start server
    StartupError,

    /// failed to stop HTTP Server
    HttpServerLifecycleError,

    /// no valid configuration was supplied
    NoConfiguration,

    /// no valid schema was supplied
    NoSchema,

    /// no valid entitlement was supplied
    NoEntitlement,

    /// could not create router: {0}
    ServiceCreationError(BoxError),

    /// could not create the HTTP server: {0}
    ServerCreationError(std::io::Error),

    /// tried to bind {0} and {1} on port {2}
    DifferentListenAddrsOnSamePort(IpAddr, IpAddr, u16),

    /// tried to register two endpoints on `{0}:{1}{2}`
    SameRouteUsedTwice(IpAddr, u16, String),
}

type SchemaStream = Pin<Box<dyn Stream<Item = String> + Send>>;

/// The user supplied schema. Either a static string or a stream for hot reloading.
#[derive(From, Display, Derivative)]
#[derivative(Debug)]
#[non_exhaustive]
pub enum SchemaSource {
    /// A static schema.
    #[display(fmt = "String")]
    Static { schema_sdl: String },

    /// A stream of schema.
    #[display(fmt = "Stream")]
    Stream(#[derivative(Debug = "ignore")] SchemaStream),

    /// A YAML file that may be watched for changes.
    #[display(fmt = "File")]
    File {
        /// The path of the schema file.
        path: PathBuf,

        /// `true` to watch the file for changes and hot apply them.
        watch: bool,

        /// When watching, the delay to wait before applying the new schema.
        /// Note: This variable is deprecated and has no effect.
        #[deprecated]
        delay: Option<Duration>,
    },

    /// Apollo managed federation.
    #[display(fmt = "Registry")]
    Registry {
        /// The Apollo key: `<YOUR_GRAPH_API_KEY>`
        apollo_key: String,

        /// The apollo graph reference: `<YOUR_GRAPH_ID>@<VARIANT>`
        apollo_graph_ref: String,

        /// The endpoint polled to fetch its latest supergraph schema.
        urls: Option<Vec<Url>>,

        /// The duration between polling
        poll_interval: Duration,

        /// The HTTP client timeout for each poll
        timeout: Duration,
    },
}

impl From<&'_ str> for SchemaSource {
    fn from(s: &'_ str) -> Self {
        Self::Static {
            schema_sdl: s.to_owned(),
        }
    }
}

impl SchemaSource {
    /// Convert this schema into a stream regardless of if is static or not. Allows for unified handling later.
    fn into_stream(self) -> impl Stream<Item = Event> {
        match self {
            SchemaSource::Static { schema_sdl: schema } => {
                stream::once(future::ready(UpdateSchema(schema))).boxed()
            }
            SchemaSource::Stream(stream) => stream.map(UpdateSchema).boxed(),
            #[allow(deprecated)]
            SchemaSource::File {
                path,
                watch,
                delay: _,
            } => {
                // Sanity check, does the schema file exists, if it doesn't then bail.
                if !path.exists() {
                    tracing::error!(
                        "Schema file at path '{}' does not exist.",
                        path.to_string_lossy()
                    );
                    stream::empty().boxed()
                } else {
                    //The schema file exists try and load it
                    match std::fs::read_to_string(&path) {
                        Ok(schema) => {
                            if watch {
                                crate::files::watch(&path)
                                    .filter_map(move |_| {
                                        future::ready(std::fs::read_to_string(&path).ok())
                                    })
                                    .map(UpdateSchema)
                                    .boxed()
                            } else {
                                stream::once(future::ready(UpdateSchema(schema))).boxed()
                            }
                        }
                        Err(err) => {
                            tracing::error!("Failed to read schema: {}", err);
                            stream::empty().boxed()
                        }
                    }
                }
            }
            SchemaSource::Registry {
                apollo_key,
                apollo_graph_ref,
                urls,
                poll_interval,
                timeout,
            } => {
                // With regards to ELv2 licensing, the code inside this block
                // is license key functionality
                stream_from_uplink::<SupergraphSdlQuery, String>(
                    apollo_key,
                    apollo_graph_ref,
                    urls.map(Endpoints::round_robin),
                    poll_interval,
                    timeout,
                )
                .filter_map(|res| {
                    future::ready(match res {
                        Ok(schema) => Some(UpdateSchema(schema)),
                        Err(e) => {
                            tracing::error!("{}", e);
                            None
                        }
                    })
                })
                .boxed()
            }
        }
        .chain(stream::iter(vec![NoMoreSchema]))
    }
}

type ConfigurationStream = Pin<Box<dyn Stream<Item = Configuration> + Send>>;

/// The user supplied config. Either a static instance or a stream for hot reloading.
#[derive(From, Display, Derivative)]
#[derivative(Debug)]
#[non_exhaustive]
pub enum ConfigurationSource {
    /// A static configuration.
    ///
    /// Can be created through `serde::Deserialize` from various formats,
    /// or inline in Rust code with `serde_json::json!` and `serde_json::from_value`.
    #[display(fmt = "Static")]
    #[from(types(Configuration))]
    Static(Box<Configuration>),

    /// A configuration stream where the server will react to new configuration. If possible
    /// the configuration will be applied without restarting the internal http server.
    #[display(fmt = "Stream")]
    Stream(#[derivative(Debug = "ignore")] ConfigurationStream),

    /// A yaml file that may be watched for changes
    #[display(fmt = "File")]
    File {
        /// The path of the configuration file.
        path: PathBuf,

        /// `true` to watch the file for changes and hot apply them.
        watch: bool,

        /// When watching, the delay to wait before applying the new configuration.
        /// Note: This variable is deprecated and has no effect.
        #[deprecated]
        delay: Option<Duration>,
    },
}

impl Default for ConfigurationSource {
    fn default() -> Self {
        ConfigurationSource::Static(Default::default())
    }
}

impl ConfigurationSource {
    /// Convert this config into a stream regardless of if is static or not. Allows for unified handling later.
    fn into_stream(self) -> impl Stream<Item = Event> {
        match self {
            ConfigurationSource::Static(instance) => {
                stream::iter(vec![UpdateConfiguration(*instance)]).boxed()
            }
            ConfigurationSource::Stream(stream) => stream.map(UpdateConfiguration).boxed(),
            #[allow(deprecated)]
            ConfigurationSource::File {
                path,
                watch,
                delay: _,
            } => {
                // Sanity check, does the config file exists, if it doesn't then bail.
                if !path.exists() {
                    tracing::error!(
                        "configuration file at path '{}' does not exist.",
                        path.to_string_lossy()
                    );
                    stream::empty().boxed()
                } else if watch {
                    crate::files::watch(&path)
                        .map(move |_| match ConfigurationSource::read_config(&path) {
                            Ok(config) => UpdateConfiguration(config),
                            Err(err) => {
                                tracing::error!("{}", err);
                                NoMoreConfiguration
                            }
                        })
                        .boxed()
                } else {
                    match ConfigurationSource::read_config(&path) {
                        Ok(configuration) => {
                            #[cfg(any(test, not(unix)))]
                            {
                                stream::once(future::ready(UpdateConfiguration(configuration)))
                                    .boxed()
                            }

                            #[cfg(all(not(test), unix))]
                            {
                                let mut sighup_stream = tokio::signal::unix::signal(
                                    tokio::signal::unix::SignalKind::hangup(),
                                )
                                .expect("Failed to install SIGHUP signal handler");

                                let (mut tx, rx) = futures::channel::mpsc::channel(1);
                                tokio::task::spawn(async move {
                                    while let Some(()) = sighup_stream.recv().await {
                                        tx.send(()).await.unwrap();
                                    }
                                });
                                futures::stream::select(
                                    stream::once(future::ready(UpdateConfiguration(configuration)))
                                        .boxed(),
                                    rx.filter_map(
                                        move |()| match ConfigurationSource::read_config(&path) {
                                            Ok(configuration) => future::ready(Some(
                                                UpdateConfiguration(configuration),
                                            )),
                                            Err(err) => {
                                                tracing::error!("{}", err);
                                                future::ready(None)
                                            }
                                        },
                                    )
                                    .boxed(),
                                )
                                .boxed()
                            }
                        }
                        Err(err) => {
                            tracing::error!("{}", err);
                            stream::empty().boxed()
                        }
                    }
                }
            }
        }
        .chain(stream::iter(vec![NoMoreConfiguration]))
        .boxed()
    }

    fn read_config(path: &Path) -> Result<Configuration, ReadConfigError> {
        let config = fs::read_to_string(path)?;
        config.parse().map_err(ReadConfigError::Validation)
    }
}
type EntitlementStream = Pin<Box<dyn Stream<Item = Entitlement> + Send>>;

/// Entitlement controls availability of certain features of the Router.
/// This API experimental and is subject to change outside of semver.
#[derive(From, Display, Derivative)]
#[derivative(Debug)]
#[non_exhaustive]
pub enum EntitlementSource {
    /// A static entitlement.
    #[display(fmt = "Static")]
    Static { entitlement: Entitlement },

    #[display(fmt = "Env")]
    Env,

    /// A stream of entitlement.
    #[display(fmt = "Stream")]
    Stream(#[derivative(Debug = "ignore")] EntitlementStream),

    /// A raw file that may be watched for changes.
    #[display(fmt = "File")]
    File {
        /// The path of the entitlement file.
        path: PathBuf,

        /// `true` to watch the file for changes and hot apply them.
        watch: bool,
    },

    /// Apollo uplink.
    #[display(fmt = "Registry")]
    Registry {
        /// The Apollo key: `<YOUR_GRAPH_API_KEY>`
        apollo_key: String,

        /// The apollo graph reference: `<YOUR_GRAPH_ID>@<VARIANT>`
        apollo_graph_ref: String,

        /// The endpoint polled to fetch its latest supergraph schema.
        urls: Option<Vec<Url>>,

        /// The duration between polling
        poll_interval: Duration,

        /// The HTTP client timeout for each poll
        timeout: Duration,
    },
}

impl Default for EntitlementSource {
    fn default() -> Self {
        EntitlementSource::Static {
            entitlement: Default::default(),
        }
    }
}

impl EntitlementSource {
    /// Convert this entitlement into a stream regardless of if is static or not. Allows for unified handling later.
    fn into_stream(self) -> impl Stream<Item = Event> {
        match self {
            EntitlementSource::Static { entitlement } => {
                stream::once(future::ready(UpdateEntitlement(entitlement))).boxed()
            }
            EntitlementSource::Stream(stream) => stream.map(UpdateEntitlement).boxed(),
            EntitlementSource::File { path, watch } => {
                // Sanity check, does the schema file exists, if it doesn't then bail.
                if !path.exists() {
                    tracing::error!(
                        "Schema file at path '{}' does not exist.",
                        path.to_string_lossy()
                    );
                    stream::empty().boxed()
                } else {
                    //The entitlement file exists try and load it
                    match std::fs::read_to_string(&path).map(|e| e.parse()) {
                        Ok(Ok(entitlement)) => {
                            if watch {
                                crate::files::watch(&path)
                                    .filter_map(move |_| {
                                        future::ready(std::fs::read_to_string(&path).ok())
                                    })
                                    .filter_map(|e| async move { e.parse().ok() })
                                    .map(UpdateEntitlement)
                                    .boxed()
                            } else {
                                stream::once(future::ready(UpdateEntitlement(entitlement))).boxed()
                            }
                        }
                        Ok(Err(err)) => {
                            tracing::error!("Failed to parse entitlement: {}", err);
                            stream::empty().boxed()
                        }
                        Err(err) => {
                            tracing::error!("Failed to read entitlement: {}", err);
                            stream::empty().boxed()
                        }
                    }
                }
            }
            EntitlementSource::Registry {
                apollo_key,
                apollo_graph_ref,
                urls,
                poll_interval,
                timeout,
            } => stream_from_uplink::<EntitlementRequest, Entitlement>(
                apollo_key,
                apollo_graph_ref,
                urls.map(Endpoints::round_robin),
                poll_interval,
                timeout,
            )
            .filter_map(|res| {
                future::ready(match res {
                    Ok(entitlement) => Some(UpdateEntitlement(entitlement)),
                    Err(e) => {
                        tracing::error!("{}", e);
                        None
                    }
                })
            })
            .boxed(),
            EntitlementSource::Env => {
                match std::env::var("APOLLO_ROUTER_ENTITLEMENT").map(|e| Entitlement::from_str(&e))
                {
                    Ok(Ok(entitlement)) => {
                        stream::once(future::ready(UpdateEntitlement(entitlement))).boxed()
                    }
                    Ok(Err(err)) => {
                        tracing::error!("Failed to parse entitlement: {}", err);
                        stream::empty().boxed()
                    }
                    Err(_) => {
                        stream::once(future::ready(UpdateEntitlement(Entitlement::default())))
                            .boxed()
                    }
                }
            }
        }
        .chain(stream::iter(vec![NoMoreEntitlement]))
    }
}

#[derive(From, Display)]
enum ReadConfigError {
    /// could not read configuration: {0}
    Io(std::io::Error),
    /// {0}
    Validation(crate::configuration::ConfigurationError),
}

type ShutdownFuture = Pin<Box<dyn Future<Output = ()> + Send>>;

/// Specifies when the Router’s HTTP server should gracefully shutdown
#[derive(Display, Derivative)]
#[derivative(Debug)]
#[non_exhaustive]
pub enum ShutdownSource {
    /// No graceful shutdown
    #[display(fmt = "None")]
    None,

    /// A custom shutdown future.
    #[display(fmt = "Custom")]
    Custom(#[derivative(Debug = "ignore")] ShutdownFuture),

    /// Watch for Ctl-C signal.
    #[display(fmt = "CtrlC")]
    CtrlC,
}

impl ShutdownSource {
    /// Convert this shutdown hook into a future. Allows for unified handling later.
    fn into_stream(self) -> impl Stream<Item = Event> {
        match self {
            ShutdownSource::None => stream::pending::<Event>().boxed(),
            ShutdownSource::Custom(future) => future.map(|_| Shutdown).into_stream().boxed(),
            ShutdownSource::CtrlC => {
                #[cfg(not(unix))]
                {
                    async {
                        tokio::signal::ctrl_c()
                            .await
                            .expect("Failed to install CTRL+C signal handler");
                    }
                    .map(|_| Shutdown)
                    .into_stream()
                    .boxed()
                }

                #[cfg(unix)]
                future::select(
                    tokio::signal::ctrl_c().map(|s| s.ok()).boxed(),
                    async {
                        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                            .expect("Failed to install SIGTERM signal handler")
                            .recv()
                            .await
                    }
                    .boxed(),
                )
                .map(|_| Shutdown)
                .into_stream()
                .boxed()
            }
        }
    }
}

/// The entry point for running the Router’s HTTP server.
///
/// # Examples
///
/// ```
/// use apollo_router::RouterHttpServer;
/// use apollo_router::Configuration;
///
/// async {
///     let configuration = serde_yaml::from_str::<Configuration>("Config").unwrap();
///     let schema = "schema";
///     RouterHttpServer::builder()
///             .configuration(configuration)
///             .schema(schema)
///             .start()
///             .await;
/// };
/// ```
///
/// Shutdown via handle.
/// ```
/// use apollo_router::RouterHttpServer;
/// use apollo_router::Configuration;
///
/// async {
///     let configuration = serde_yaml::from_str::<Configuration>("Config").unwrap();
///     let schema = "schema";
///     let mut server = RouterHttpServer::builder()
///             .configuration(configuration)
///             .schema(schema)
///             .start();
///     // …
///     server.shutdown().await
/// };
/// ```
///
pub struct RouterHttpServer {
    result: Pin<Box<dyn Future<Output = Result<(), ApolloRouterError>> + Send>>,
    listen_addresses: Arc<RwLock<ListenAddresses>>,
    shutdown_sender: Option<oneshot::Sender<()>>,
}

#[buildstructor::buildstructor]
impl RouterHttpServer {
    /// Returns a builder to start an HTTP server in a separate Tokio task.
    ///
    /// Builder methods:
    ///
    /// * `.schema(impl Into<`[`SchemaSource`]`>)`
    ///   Required.
    ///   Specifies where to find the supergraph schema definition.
    ///   Some sources support hot-reloading.
    ///
    /// * `.configuration(impl Into<`[`ConfigurationSource`]`>)`
    ///   Optional.
    ///   Specifies where to find the router configuration.
    ///   If not provided, the default configuration as with an empty YAML file.
    ///
    /// * `.entitlement(impl Into<`[`EntitlementSource`]`>)`
    ///   Optional.
    ///   Specifies where to find the router entitlement which controls if commercial features are enabled or not.
    ///   If not provided then commercial features will not be enabled.
    ///
    /// * `.shutdown(impl Into<`[`ShutdownSource`]`>)`
    ///   Optional.
    ///   Specifies when the server should gracefully shut down.
    ///   If not provided, the default is [`ShutdownSource::CtrlC`].
    ///
    /// * `.start()`
    ///   Finishes the builder,
    ///   starts an HTTP server in a separate Tokio task,
    ///   and returns a `RouterHttpServer` handle.
    ///
    /// The server handle can be used in multiple ways.
    /// As a [`Future`], it resolves to `Result<(), `[`ApolloRouterError`]`>`
    /// either when the server has finished gracefully shutting down
    /// or when it encounters a fatal error that prevents it from starting.
    ///
    /// If the handle is dropped before being awaited as a future,
    /// a graceful shutdown is triggered.
    /// In order to wait until shutdown finishes,
    /// use the [`shutdown`][Self::shutdown] method instead.
    #[builder(visibility = "pub", entry = "builder", exit = "start")]
    fn start(
        schema: SchemaSource,
        configuration: Option<ConfigurationSource>,
        entitlement: Option<EntitlementSource>,
        shutdown: Option<ShutdownSource>,
    ) -> RouterHttpServer {
        let (shutdown_sender, shutdown_receiver) = oneshot::channel::<()>();
        let event_stream = generate_event_stream(
            shutdown.unwrap_or(ShutdownSource::CtrlC),
            configuration.unwrap_or_default(),
            schema,
            entitlement.unwrap_or_default(),
            shutdown_receiver,
        );
        let server_factory = AxumHttpServerFactory::new();
        let router_factory = OrbiterRouterSuperServiceFactory::new(YamlRouterFactory::default());
        let state_machine = StateMachine::new(server_factory, router_factory);
        let listen_addresses = state_machine.listen_addresses.clone();
        let result = spawn(
            async move { state_machine.process_events(event_stream).await }
                .with_current_subscriber(),
        )
        .map(|r| match r {
            Ok(Ok(ok)) => Ok(ok),
            Ok(Err(err)) => Err(err),
            Err(err) => {
                tracing::error!("{}", err);
                Err(ApolloRouterError::StartupError)
            }
        })
        .with_current_subscriber()
        .boxed();

        RouterHttpServer {
            result,
            shutdown_sender: Some(shutdown_sender),
            listen_addresses,
        }
    }

    /// Returns the listen address when the router is ready to receive GraphQL requests.
    ///
    /// This can be useful when the `server.listen` configuration specifies TCP port 0,
    /// which instructs the operating system to pick an available port number.
    ///
    /// Note: if configuration is dynamic, the listen address can change over time.
    pub async fn listen_address(&self) -> Option<ListenAddr> {
        self.listen_addresses
            .read()
            .await
            .graphql_listen_address
            .clone()
    }

    /// Returns the extra listen addresses the router can receive requests to.
    ///
    /// Combine it with `listen_address` to have an exhaustive list
    /// of all addresses used by the router.
    /// Note: if configuration is dynamic, the listen address can change over time.
    pub async fn extra_listen_adresses(&self) -> Vec<ListenAddr> {
        self.listen_addresses
            .read()
            .await
            .extra_listen_addresses
            .clone()
    }

    /// Trigger and wait for graceful shutdown
    pub async fn shutdown(&mut self) -> Result<(), ApolloRouterError> {
        if let Some(sender) = self.shutdown_sender.take() {
            let _ = sender.send(());
        }
        (&mut self.result).await
    }
}

/// Messages that are broadcast across the app.
#[derive(Debug)]
pub(crate) enum Event {
    /// The configuration was updated.
    UpdateConfiguration(Configuration),

    /// There are no more updates to the configuration
    NoMoreConfiguration,

    /// The schema was updated.
    UpdateSchema(String),

    /// There are no more updates to the schema
    NoMoreSchema,

    /// Update entitlement.
    UpdateEntitlement(Entitlement),

    /// The entitlement has entered warn_at has passed.
    WarnEntitlement,

    /// The entitlement has halt_at has passed.
    HaltEntitlement,

    /// There were no more updates to entitlement.
    NoMoreEntitlement,

    /// The server should gracefully shutdown.
    Shutdown,
}

impl Drop for RouterHttpServer {
    fn drop(&mut self) {
        if let Some(sender) = self.shutdown_sender.take() {
            let _ = sender.send(());
        }
    }
}

impl Future for RouterHttpServer {
    type Output = Result<(), ApolloRouterError>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        self.result.poll_unpin(cx)
    }
}

/// Create the unified event stream.
/// This merges all contributing streams and sets up shutdown handling.
/// When a shutdown message is received no more events are emitted.
fn generate_event_stream(
    shutdown: ShutdownSource,
    configuration: ConfigurationSource,
    schema: SchemaSource,
    entitlement: EntitlementSource,
    shutdown_receiver: oneshot::Receiver<()>,
) -> impl Stream<Item = Event> {
    // Chain is required so that the final shutdown message is sent.
    stream::select_all(vec![
        shutdown.into_stream().boxed(),
        configuration.into_stream().boxed(),
        schema.into_stream().boxed(),
        entitlement.into_stream().boxed(),
        shutdown_receiver.into_stream().map(|_| Shutdown).boxed(),
    ])
    .take_while(|msg| future::ready(!matches!(msg, Shutdown)))
    .chain(stream::iter(vec![Shutdown]))
    .boxed()
}

#[cfg(test)]
mod tests {
    use std::env::temp_dir;

    use serde_json::to_string_pretty;
    use test_log::test;

    use super::*;
    use crate::files::tests::create_temp_file;
    use crate::files::tests::write_and_flush;
    use crate::graphql;
    use crate::graphql::Request;

    fn init_with_server() -> RouterHttpServer {
        let configuration =
            Configuration::from_str(include_str!("testdata/supergraph_config.router.yaml"))
                .unwrap();
        let schema = include_str!("testdata/supergraph.graphql");
        RouterHttpServer::builder()
            .configuration(configuration)
            .schema(schema)
            .start()
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn basic_request() {
        let mut router_handle = init_with_server();
        let listen_address = router_handle
            .listen_address()
            .await
            .expect("router failed to start");

        assert_federated_response(&listen_address, r#"{ topProducts { name } }"#).await;
        router_handle.shutdown().await.unwrap();
    }

    async fn assert_federated_response(listen_addr: &ListenAddr, request: &str) {
        let request = Request::builder().query(request).build();
        let expected = query(listen_addr, &request).await.unwrap();

        let response = to_string_pretty(&expected).unwrap();
        assert!(!response.is_empty());
    }

    async fn query(
        listen_addr: &ListenAddr,
        request: &graphql::Request,
    ) -> Result<graphql::Response, crate::error::FetchError> {
        Ok(reqwest::Client::new()
            .post(format!("{listen_addr}/"))
            .json(request)
            .send()
            .await
            .expect("couldn't send request")
            .json()
            .await
            .expect("couldn't deserialize into json"))
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn config_by_file_watching() {
        let (path, mut file) = create_temp_file();
        let contents = include_str!("testdata/supergraph_config.router.yaml");
        write_and_flush(&mut file, contents).await;
        let mut stream = ConfigurationSource::File {
            path,
            watch: true,
            delay: None,
        }
        .into_stream()
        .boxed();

        // First update is guaranteed
        assert!(matches!(
            stream.next().await.unwrap(),
            UpdateConfiguration(_)
        ));

        // Need different contents, since we won't get an event if content is the same
        let contents_datadog = include_str!("testdata/datadog.router.yaml");
        // Modify the file and try again
        write_and_flush(&mut file, contents_datadog).await;
        assert!(matches!(
            stream.next().await.unwrap(),
            UpdateConfiguration(_)
        ));

        // This time write garbage, there should not be an update.
        write_and_flush(&mut file, ":garbage").await;
        let event = stream.into_future().now_or_never();
        assert!(event.is_none() || matches!(event, Some((Some(NoMoreConfiguration), _))));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn config_by_file_invalid() {
        let (path, mut file) = create_temp_file();
        write_and_flush(&mut file, "Garbage").await;
        let mut stream = ConfigurationSource::File {
            path,
            watch: true,
            delay: None,
        }
        .into_stream();

        // First update fails because the file is invalid.
        assert!(matches!(stream.next().await.unwrap(), NoMoreConfiguration));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn config_by_file_missing() {
        let mut stream = ConfigurationSource::File {
            path: temp_dir().join("does_not_exit"),
            watch: true,
            delay: None,
        }
        .into_stream();

        // First update fails because the file is invalid.
        assert!(matches!(stream.next().await.unwrap(), NoMoreConfiguration));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn config_by_file_no_watch() {
        let (path, mut file) = create_temp_file();
        let contents = include_str!("testdata/supergraph_config.router.yaml");
        write_and_flush(&mut file, contents).await;

        let mut stream = ConfigurationSource::File {
            path,
            watch: false,
            delay: None,
        }
        .into_stream();
        assert!(matches!(
            stream.next().await.unwrap(),
            UpdateConfiguration(_)
        ));
        assert!(matches!(stream.next().await.unwrap(), NoMoreConfiguration));
    }

    #[test(tokio::test)]
    async fn schema_by_file_watching() {
        let (path, mut file) = create_temp_file();
        let schema = include_str!("testdata/supergraph.graphql");
        write_and_flush(&mut file, schema).await;
        let mut stream = SchemaSource::File {
            path,
            watch: true,
            delay: None,
        }
        .into_stream()
        .boxed();

        // First update is guaranteed
        assert!(matches!(stream.next().await.unwrap(), UpdateSchema(_)));

        // Need different contents, since we won't get an event if content is the same
        let schema_minimal = include_str!("testdata/minimal_supergraph.graphql");
        // Modify the file and try again
        write_and_flush(&mut file, schema_minimal).await;
        assert!(matches!(stream.next().await.unwrap(), UpdateSchema(_)));
    }

    #[test(tokio::test)]
    async fn schema_by_file_missing() {
        let mut stream = SchemaSource::File {
            path: temp_dir().join("does_not_exist"),
            watch: true,
            delay: None,
        }
        .into_stream();

        // First update fails because the file is invalid.
        assert!(matches!(stream.next().await.unwrap(), NoMoreSchema));
    }

    #[test(tokio::test)]
    async fn schema_by_file_no_watch() {
        let (path, mut file) = create_temp_file();
        let schema = include_str!("testdata/supergraph.graphql");
        write_and_flush(&mut file, schema).await;

        let mut stream = SchemaSource::File {
            path,
            watch: false,
            delay: None,
        }
        .into_stream();
        assert!(matches!(stream.next().await.unwrap(), UpdateSchema(_)));
        assert!(matches!(stream.next().await.unwrap(), NoMoreSchema));
    }
}
