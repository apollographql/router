mod configuration;
mod license;
mod reload;
mod schema;
mod shutdown;

use std::fmt::Debug;
use std::fmt::Formatter;

pub use configuration::ConfigurationSource;
pub use license::LicenseSource;
pub(crate) use reload::ReloadSource;
pub use schema::SchemaSource;
pub use shutdown::ShutdownSource;

use self::Event::NoMoreConfiguration;
use self::Event::NoMoreLicense;
use self::Event::NoMoreSchema;
use self::Event::Reload;
use self::Event::Shutdown;
use self::Event::UpdateConfiguration;
use self::Event::UpdateLicense;
use self::Event::UpdateSchema;
use crate::uplink::license_enforcement::LicenseState;
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

    /// Update license {}
    UpdateLicense(LicenseState),

    /// There were no more updates to license.
    NoMoreLicense,

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
            UpdateLicense(e) => {
                write!(f, "UpdateLicense({e:?})")
            }
            NoMoreLicense => {
                write!(f, "NoMoreLicense")
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
