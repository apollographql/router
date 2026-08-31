//! A plugin for enforcing product limitations in the router based on License claims

pub(crate) mod layer;

use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;
use tower::BoxError;

use crate::plugin::PluginInit;
use crate::plugin::PluginPrivate;

#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct LicenseEnforcement {}

/// The license enforcement plugin has no configuration.
#[derive(Debug, Default, Deserialize, JsonSchema, Serialize)]
pub(crate) struct LicenseEnforcementConfig {}

#[async_trait::async_trait]
impl PluginPrivate for LicenseEnforcement {
    type Config = LicenseEnforcementConfig;

    async fn new(_init: PluginInit<Self::Config>) -> Result<Self, BoxError> {
        Ok(Self {})
    }
}

register_private_plugin!("apollo", "license_enforcement", LicenseEnforcement);
