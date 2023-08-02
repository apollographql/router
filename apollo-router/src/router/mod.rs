#![allow(missing_docs)] // FIXME
#![allow(deprecated)] // Note: Required to prevents complaints on enum declaration

mod error;
mod event;

use std::pin::Pin;
use std::sync::Arc;
use std::task::Context;
use std::task::Poll;

pub use error::ApolloRouterError;
pub use event::ConfigurationSource;
pub(crate) use event::Event;
pub use event::LicenseSource;
pub(crate) use event::ReloadSource;
pub use event::SchemaSource;
pub use event::ShutdownSource;
#[cfg(test)]
use futures::channel::mpsc;
#[cfg(test)]
use futures::channel::mpsc::SendError;
use futures::channel::oneshot;
use futures::prelude::*;
use futures::FutureExt;
#[cfg(test)]
use tokio::sync::Notify;
use tokio::sync::RwLock;
use tokio::task::spawn;
use tracing_futures::WithSubscriber;

use crate::axum_factory::AxumHttpServerFactory;
use crate::configuration::ListenAddr;
use crate::orbiter::OrbiterRouterSuperServiceFactory;
use crate::router_factory::YamlRouterFactory;
use crate::state_machine::ListenAddresses;
use crate::state_machine::StateMachine;
use crate::uplink::UplinkConfig;
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
    /// * `.license(impl Into<`[`LicenseSource`]`>)`
    ///   Optional.
    ///   Specifies where to find the router license which controls if commercial features are enabled or not.
    ///   If not provided then commercial features will not be enabled.
    ///
    /// * `.uplink(impl Into<`[UplinkConfig]>`)`
    ///   Optional.
    ///   Specifies the Uplink configuration options.
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
        license: Option<LicenseSource>,
        shutdown: Option<ShutdownSource>,
        uplink: Option<UplinkConfig>,
    ) -> RouterHttpServer {
        let (shutdown_sender, shutdown_receiver) = oneshot::channel::<()>();
        let event_stream = generate_event_stream(
            shutdown.unwrap_or(ShutdownSource::CtrlC),
            configuration.unwrap_or_default(),
            schema,
            uplink,
            license.unwrap_or_default(),
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
    uplink_config: Option<UplinkConfig>,
    license: LicenseSource,
    shutdown_receiver: oneshot::Receiver<()>,
) -> impl Stream<Item = Event> {
    let reload_source = ReloadSource::default();

    let stream = stream::select_all(vec![
        shutdown.into_stream().boxed(),
        schema.into_stream().boxed(),
        license.into_stream().boxed(),
        reload_source.clone().into_stream().boxed(),
        configuration
            .into_stream(uplink_config)
            .map(move |config_event| {
                if let Event::UpdateConfiguration(config) = &config_event {
                    reload_source.set_period(&config.experimental_chaos.force_reload)
                }
                config_event
            })
            .boxed(),
        shutdown_receiver
            .into_stream()
            .map(|_| Event::Shutdown)
            .boxed(),
    ])
    .take_while(|msg| future::ready(!matches!(msg, Event::Shutdown)))
    // Chain is required so that the final shutdown message is sent.
    .chain(stream::iter(vec![Event::Shutdown]))
    .boxed();
    stream
}

#[cfg(test)]
struct TestRouterHttpServer {
    router_http_server: RouterHttpServer,
    event_sender: mpsc::UnboundedSender<Event>,
    state_machine_update_notifier: Arc<Notify>,
}

#[cfg(test)]
impl TestRouterHttpServer {
    fn new() -> Self {
        let (event_sender, event_receiver) = mpsc::unbounded();
        let state_machine_update_notifier = Arc::new(Notify::new());

        let server_factory = AxumHttpServerFactory::new();
        let router_factory: OrbiterRouterSuperServiceFactory =
            OrbiterRouterSuperServiceFactory::new(YamlRouterFactory::default());
        let state_machine = StateMachine::for_tests(
            server_factory,
            router_factory,
            Arc::clone(&state_machine_update_notifier),
        );

        let listen_addresses = state_machine.listen_addresses.clone();
        let result = spawn(
            async move { state_machine.process_events(event_receiver).await }
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

        TestRouterHttpServer {
            router_http_server: RouterHttpServer {
                result,
                shutdown_sender: None,
                listen_addresses,
            },
            event_sender,
            state_machine_update_notifier,
        }
    }

    async fn request(
        &self,
        request: crate::graphql::Request,
    ) -> Result<crate::graphql::Response, crate::error::FetchError> {
        Ok(reqwest::Client::new()
            .post(format!("{}/", self.listen_address().await.unwrap()))
            .json(&request)
            .send()
            .await
            .expect("couldn't send request")
            .json()
            .await
            .expect("couldn't deserialize into json"))
    }

    async fn listen_address(&self) -> Option<ListenAddr> {
        self.router_http_server.listen_address().await
    }

    async fn send_event(&mut self, event: Event) -> Result<(), SendError> {
        let result = self.event_sender.send(event).await;
        self.state_machine_update_notifier.notified().await;
        result
    }

    async fn shutdown(mut self) -> Result<(), ApolloRouterError> {
        self.send_event(Event::Shutdown).await.unwrap();
        self.router_http_server.shutdown().await
    }
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use serde_json::to_string_pretty;

    use super::*;
    use crate::graphql;
    use crate::graphql::Request;
    use crate::router::Event::UpdateConfiguration;
    use crate::router::Event::UpdateLicense;
    use crate::router::Event::UpdateSchema;
    use crate::uplink::license_enforcement::LicenseState;
    use crate::Configuration;

    fn init_with_server() -> RouterHttpServer {
        let configuration =
            Configuration::from_str(include_str!("../testdata/supergraph_config.router.yaml"))
                .unwrap();
        let schema = include_str!("../testdata/supergraph.graphql");
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
    async fn basic_event_stream_test() {
        let mut router_handle = TestRouterHttpServer::new();

        let configuration =
            Configuration::from_str(include_str!("../testdata/supergraph_config.router.yaml"))
                .unwrap();
        let schema = include_str!("../testdata/supergraph.graphql");

        // let's push a valid configuration to the state machine, so it can start up
        router_handle
            .send_event(UpdateConfiguration(configuration))
            .await
            .unwrap();
        router_handle
            .send_event(UpdateSchema(schema.to_string()))
            .await
            .unwrap();
        router_handle
            .send_event(UpdateLicense(LicenseState::Unlicensed))
            .await
            .unwrap();

        let request = Request::builder().query(r#"{ me { username } }"#).build();

        let response = router_handle.request(request).await.unwrap();
        assert_eq!(
            "@ada",
            response
                .data
                .unwrap()
                .get("me")
                .unwrap()
                .get("username")
                .unwrap()
        );

        // shut the router down
        router_handle
            .send_event(Event::NoMoreConfiguration)
            .await
            .unwrap();
        router_handle.send_event(Event::NoMoreSchema).await.unwrap();
        router_handle.send_event(Event::Shutdown).await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn schema_update_test() {
        let mut router_handle = TestRouterHttpServer::new();
        // let's push a valid configuration to the state machine, so it can start up
        router_handle
            .send_event(UpdateConfiguration(
                Configuration::from_str(include_str!("../testdata/supergraph_config.router.yaml"))
                    .unwrap(),
            ))
            .await
            .unwrap();
        router_handle
            .send_event(UpdateSchema(
                include_str!("../testdata/supergraph_missing_name.graphql").to_string(),
            ))
            .await
            .unwrap();
        router_handle
            .send_event(UpdateLicense(LicenseState::Unlicensed))
            .await
            .unwrap();

        // let's send a valid query
        let request = Request::builder().query(r#"{ me { username } }"#).build();
        let response = router_handle.request(request).await.unwrap();

        assert_eq!(
            "@ada",
            response
                .data
                .unwrap()
                .get("me")
                .unwrap()
                .get("username")
                .unwrap()
        );

        // the name field is not present yet
        let request = Request::builder()
            .query(r#"{ me { username name } }"#)
            .build();
        let response = router_handle.request(request).await.unwrap();

        assert_eq!(
            "cannot query field 'name' on type 'User'",
            response.errors[0].message
        );
        assert_eq!(
            "INVALID_FIELD",
            response.errors[0].extensions.get("code").unwrap()
        );

        // let's update the schema to add the field
        router_handle
            .send_event(UpdateSchema(
                include_str!("../testdata/supergraph.graphql").to_string(),
            ))
            .await
            .unwrap();

        // the request should now make it through
        let request = Request::builder()
            .query(r#"{ me { username name } }"#)
            .build();

        let response = router_handle.request(request).await.unwrap();

        assert_eq!(
            "Ada Lovelace",
            response
                .data
                .unwrap()
                .get("me")
                .unwrap()
                .get("name")
                .unwrap()
        );

        // let's go back and remove the field
        router_handle
            .send_event(UpdateSchema(
                include_str!("../testdata/supergraph_missing_name.graphql").to_string(),
            ))
            .await
            .unwrap();

        let request = Request::builder().query(r#"{ me { username } }"#).build();
        let response = router_handle.request(request).await.unwrap();

        assert_eq!(
            "@ada",
            response
                .data
                .unwrap()
                .get("me")
                .unwrap()
                .get("username")
                .unwrap()
        );

        let request = Request::builder()
            .query(r#"{ me { username name } }"#)
            .build();
        let response = router_handle.request(request).await.unwrap();

        assert_eq!(
            "cannot query field 'name' on type 'User'",
            response.errors[0].message
        );
        assert_eq!(
            "INVALID_FIELD",
            response.errors[0].extensions.get("code").unwrap()
        );
        router_handle.shutdown().await.unwrap();
    }
}
