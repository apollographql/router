// With regards to ELv2 licensing, this entire file is license key functionality

mod configuration;
mod entitlement;
mod reload;
mod schema;
mod shutdown;

use std::fmt::Debug;
use std::fmt::Formatter;

pub use configuration::ConfigurationSource;
pub use entitlement::EntitlementSource;
pub(crate) use reload::ReloadSource;
pub use schema::SchemaSource;
pub use shutdown::ShutdownSource;

use self::Event::NoMoreConfiguration;
use self::Event::NoMoreEntitlement;
use self::Event::NoMoreSchema;
use self::Event::Reload;
use self::Event::Shutdown;
use self::Event::UpdateConfiguration;
use self::Event::UpdateEntitlement;
use self::Event::UpdateSchema;
use crate::uplink::entitlement::EntitlementState;
use crate::Configuration;

/// Messages that are broadcast across the app.
pub(crate) enum Event {
    /// The configuration was updated.
    UpdateConfiguration(Configuration),

    /// There are no more updates to the configuration
    NoMoreConfiguration,

    /// The schema was updated.
    UpdateSchema(String),

    /// There are no more updates to the schema
    NoMoreSchema,

    /// Update entitlement {}
    UpdateEntitlement(EntitlementState),

    /// There were no more updates to entitlement.
    NoMoreEntitlement,

    /// Artificial hot reload for chaos testing
    Reload,

    /// The server should gracefully shutdown.
    Shutdown,
}

impl Debug for Event {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            UpdateConfiguration(_) => {
                write!(f, "UpdateConfiguration(<redacted>)")
            }
            NoMoreConfiguration => {
                write!(f, "NoMoreConfiguration")
            }
            UpdateSchema(_) => {
                write!(f, "UpdateSchema(<redacted>)")
            }
            NoMoreSchema => {
                write!(f, "NoMoreSchema")
            }
            UpdateEntitlement(e) => {
                write!(f, "UpdateEntitlement({e:?})")
            }
            NoMoreEntitlement => {
                write!(f, "NoMoreEntitlement")
            }
            Reload => {
                write!(f, "ForcedHotReload")
            }
            Shutdown => {
                write!(f, "Shutdown")
            }
        }
    }
}
