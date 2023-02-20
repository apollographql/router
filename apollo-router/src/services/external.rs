// With regards to ELv2 licensing, this entire file is license key functionality
#![allow(missing_docs)] // FIXME

use std::collections::HashMap;
use std::fmt::Debug;
use std::string::ToString;
use std::time::Duration;

use http::header::ACCEPT;
use http::header::CONTENT_TYPE;
use http::StatusCode;
use once_cell::sync::Lazy;
use reqwest::Client;
use schemars::JsonSchema;
use serde::de::DeserializeOwned;
use serde::Deserialize;
use serde::Serialize;
use strum_macros::Display;
use tower::BoxError;

use crate::error::LicenseError;
use crate::services::apollo_graph_reference;
use crate::tracer::TraceId;
use crate::Context;

const DEFAULT_EXTERNALIZATION_TIMEOUT: Duration = Duration::from_secs(1);

static CLIENT: Lazy<Result<Client, BoxError>> = Lazy::new(|| {
    apollo_graph_reference().ok_or(LicenseError::MissingGraphReference)?;
    Ok(Client::new())
});

/// Version of our externalised data. Rev this if it changes
const EXTERNALIZABLE_VERSION: u8 = 1;

#[derive(Clone, Debug, Display, Deserialize, PartialEq, Serialize, JsonSchema)]
pub(crate) enum PipelineStep {
    RouterRequest,
    RouterResponse,
    SupergraphRequest,
    SupergraphResponse,
    ExecutionRequest,
    ExecutionResponse,
    SubgraphRequest,
    SubgraphResponse,
}

#[derive(Clone, Debug, Display, Deserialize, PartialEq, Serialize, JsonSchema)]
pub(crate) enum Control {
    Continue,
    Break(u16),
}

impl Default for Control {
    fn default() -> Self {
        Control::Continue
    }
}

impl Control {
    #[allow(dead_code)]
    fn new(status: u16) -> Self {
        Control::Break(status)
    }

    pub(crate) fn get_http_status(&self) -> Result<StatusCode, BoxError> {
        match self {
            Control::Continue => Ok(StatusCode::OK),
            Control::Break(code) => StatusCode::from_u16(*code).map_err(|e| e.into()),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct Externalizable<T> {
    pub(crate) version: u8,
    pub(crate) stage: String,
    pub(crate) control: Control,
    pub(crate) id: Option<String>,
    pub(crate) headers: Option<HashMap<String, Vec<String>>>,
    pub(crate) body: Option<T>,
    pub(crate) context: Option<Context>,
    pub(crate) sdl: Option<String>,
}

impl<T> Externalizable<T>
where
    T: Debug + DeserializeOwned + Serialize + Send + Sync,
{
    pub(crate) fn new(
        stage: PipelineStep,
        headers: Option<HashMap<String, Vec<String>>>,
        body: Option<T>,
        context: Option<Context>,
        sdl: Option<String>,
    ) -> Self {
        Self {
            version: EXTERNALIZABLE_VERSION,
            stage: stage.to_string(),
            control: Control::default(),
            id: TraceId::maybe_new().map(|id| id.to_string()),
            headers,
            body,
            context,
            sdl,
        }
    }

    pub(crate) async fn call(self, url: &str, timeout: Option<Duration>) -> Result<Self, BoxError> {
        let my_client = CLIENT.as_ref().map_err(|e| e.to_string())?.clone();
        let t = timeout.unwrap_or(DEFAULT_EXTERNALIZATION_TIMEOUT);

        tracing::debug!("forwarding json: {}", serde_json::to_string(&self)?);
        let response = my_client
            .post(url)
            .json(&self)
            .header(ACCEPT, "application/json")
            .header(CONTENT_TYPE, "application/json")
            .timeout(t)
            .send()
            .await?;

        // Let's process our response
        let response: Self = response.json().await?;

        Ok(response)
    }
}

#[cfg(test)]
mod tests {
    use super::CLIENT;

    #[test]
    fn it_will_not_externalize_without_environment() {
        assert!(CLIENT.as_ref().is_err());
    }
}
