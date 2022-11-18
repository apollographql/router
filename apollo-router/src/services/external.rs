#![allow(missing_docs)] // FIXME

use std::collections::HashMap;
use std::fmt::Debug;
use std::string::ToString;

use http::header::ACCEPT;
use http::header::CONTENT_TYPE;
use once_cell::sync::Lazy;
use reqwest::Client;
use serde::de::DeserializeOwned;
use serde::Deserialize;
use serde::Serialize;
use strum_macros::Display;
use tower::BoxError;

use crate::Context;

static CLIENT: Lazy<Client> = Lazy::new(Client::new);

/// Version of our externalised data. Rev this if it changes
const EXTERNALIZABLE_VERSION: u8 = 1;

// TODO: ALLOW DEAD CODE FOR NOW UNTIL DECIDE IF RESPONSE IS TO BE IMPLEMENTED
#[allow(dead_code)]
#[derive(Display)]
pub(crate) enum PipelineStep {
    SupergraphRequest,
    SupergraphResponse,
    ExecutionRequest,
    ExecutionResponse,
    SubgraphRequest,
    SubgraphResponse,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct Externalizable<T> {
    pub(crate) version: u8,
    pub(crate) stage: String,
    pub(crate) headers: HashMap<String, Vec<String>>,
    pub(crate) body: T,
    pub(crate) context: Context,
    pub(crate) sdl: String,
}

impl<T> Externalizable<T>
where
    T: Debug + DeserializeOwned + Serialize + Send + Sync,
{
    pub(crate) fn new(
        stage: PipelineStep,
        headers: HashMap<String, Vec<String>>,
        body: T,
        context: Context,
        sdl: String,
    ) -> Self {
        Self {
            version: EXTERNALIZABLE_VERSION,
            stage: stage.to_string(),
            headers,
            body,
            context,
            sdl,
        }
    }

    pub(crate) async fn call(self, url: &str) -> Result<Self, BoxError> {
        let my_client = CLIENT.clone();

        tracing::debug!("forwarding headers: {:?}", self.headers);
        tracing::debug!("forwarding body: {:?}", self.body);
        let response = my_client
            .post(url)
            .json(&self)
            .header(ACCEPT, "application/json")
            .header(CONTENT_TYPE, "application/json")
            .send()
            .await?;

        // Let's process our response
        let response: Self = response.json().await?;
        tracing::debug!("response body: {:?}", response.body);
        tracing::debug!("response headers: {:?}", response.headers);

        Ok(response)
    }
}
