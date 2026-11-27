use async_trait::async_trait;
use tower::BoxError;

use crate::plugin::PluginInit;
use crate::plugin::PluginPrivate;
use crate::register_private_plugin;
use crate::uplink::license_enforcement::AllowedFeature;
use crate::uplink::license_enforcement::LicensedSetting;

/// Restricted plugin (for testing purposes only)
#[apollo_configuration::configuration]
struct Config {
    /// Enable the restricted plugin (for testing purposes only)
    #[config(required)]
    enabled: bool,
}

/// Dummy plugin (for testing purposes only)
struct Restricted;

register_private_plugin!("experimental", "restricted", Restricted);

#[async_trait]
impl PluginPrivate for Restricted {
    type Config = Config;

    fn licensed_features(
        config: &Self::Config,
        _supergraph: &apollo_federation::Supergraph,
    ) -> Vec<LicensedSetting> {
        if config.enabled {
            vec![LicensedSetting {
                feature: AllowedFeature::Experimental,
                path: r#"plugins."experimental.restricted".enabled"#,
            }]
        } else {
            Vec::new()
        }
    }

    async fn new(init: PluginInit<Self::Config>) -> Result<Self, BoxError>
    where
        Self: Sized,
    {
        tracing::info!("restricted plugin enabled: {}", init.config.enabled);
        Ok(Restricted)
    }
}
