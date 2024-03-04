use std::fmt::Debug;
use std::net::IpAddr;

use displaydoc::Display as DisplayDoc;
use thiserror::Error;
use tower::BoxError;

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

    /// no valid license was supplied
    NoLicense,

    /// license violation
    LicenseViolation,

    /// could not create router: {0}
    ServiceCreationError(BoxError),

    /// could not create the HTTP server: {0}
    ServerCreationError(std::io::Error),

    /// tried to bind {0} and {1} on port {2}
    DifferentListenAddrsOnSamePort(IpAddr, IpAddr, u16),

    /// tried to register two endpoints on `{0}:{1}{2}`
    SameRouteUsedTwice(IpAddr, u16, String),

    /// TLS configuration error: {0}
    Rustls(rustls::Error),
}
