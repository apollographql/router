#![allow(missing_docs)] // FIXME

use once_cell::sync::Lazy;
use reqwest::Client;
use serde::Deserialize;
use serde::Serialize;
use tower::BoxError;

use crate::graphql;
use crate::Context;

static CLIENT: Lazy<Client> = Lazy::new(Client::new);

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Output {
    pub context: Context,
    pub sdl: String,
    pub body: graphql::Request,
}

pub async fn call_service(
    url: &str,
    request: graphql::Request,
    context: Context,
    sdl: String,
) -> Result<Output, BoxError> {
    let my_client = CLIENT.clone();
    // Call into our out of process processor with a body of our body
    let output = Output {
        context,
        sdl,
        body: request,
    };

    tracing::info!("forwarding query: {:?}", output.body.query);
    let response = my_client.post(url).json(&output).send().await?;

    // First, let's update our request
    let modified_output: Output = response.json().await?;
    // tracing::info!("modified output: {:?}", modified_output);
    tracing::info!("modified query: {:?}", modified_output.body.query);

    Ok(modified_output)
}
