use schemars::JsonSchema;
use serde::Deserialize;

use crate::plugins::traffic_shaping::Http2Config;

#[derive(Debug, Clone, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, default)]
pub(crate) struct Client {
    pub(crate) experimental_http2: Option<Http2Config>,
}
