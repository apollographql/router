#![allow(clippy::disallowed_names)]

use async_trait::async_trait;
use bytes_utils::Str;
use fred::{prelude::*, types::Resolve};
use hickory_resolver::{
  config::{ResolverConfig, ResolverOpts},
  TokioAsyncResolver,
};
use std::{net::SocketAddr, sync::Arc};

pub struct HickoryDnsResolver(TokioAsyncResolver);

impl Default for HickoryDnsResolver {
  fn default() -> Self {
    HickoryDnsResolver(TokioAsyncResolver::tokio(
      ResolverConfig::default(),
      ResolverOpts::default(),
    ))
  }
}

#[async_trait]
impl Resolve for HickoryDnsResolver {
  async fn resolve(&self, host: Str, port: u16) -> Result<Vec<SocketAddr>, Error> {
    Ok(
      self
        .0
        .lookup_ip(&*host)
        .await?
        .into_iter()
        .map(|ip| SocketAddr::new(ip, port))
        .collect(),
    )
  }
}

#[tokio::main]
async fn main() -> Result<(), Error> {
  let client = Builder::default_centralized().build()?;
  client.set_resolver(Arc::new(HickoryDnsResolver::default())).await;
  client.init().await?;

  // ...

  client.quit().await?;
  Ok(())
}
