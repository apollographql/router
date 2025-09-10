#![allow(dead_code)]
use crate::integration::{
  docker::env::{COMPOSE_NETWORK_NAME, NETWORK_NAME},
  utils,
};
use bollard::{
  container::{
    Config, CreateContainerOptions, LogOutput, NetworkingConfig, RemoveContainerOptions, StartContainerOptions,
  },
  errors::Error as BollardError,
  exec::{CreateExecOptions, StartExecResults},
  network::{ConnectNetworkOptions, ListNetworksOptions},
  ClientVersion, Docker, API_DEFAULT_VERSION,
};
use bytes::Bytes;
use fred::prelude::*;
use fred::types::ClusterRouting;
use futures::stream::StreamExt;
use redis_protocol::resp2::decode::decode_bytes as resp2_decode;
use std::collections::HashMap;

macro_rules! e (
  ($arg:expr) => ($arg.map_err(|e| RedisError::new(RedisErrorKind::Unknown, format!("{:?}", e))))
);

pub mod env {
  use fred::error::{RedisError, RedisErrorKind};
  use std::env;

  // compat check
  pub const COMPOSE_NETWORK_NAME: &str = "compose_fred-tests";
  pub const NETWORK_NAME: &str = "fred-tests";

  pub const CENTRALIZED_HOST: &str = "FRED_REDIS_CENTRALIZED_HOST";
  pub const CENTRALIZED_PORT: &str = "FRED_REDIS_CENTRALIZED_PORT";
  pub const CLUSTER_HOST: &str = "FRED_REDIS_CLUSTER_HOST";
  pub const CLUSTER_PORT: &str = "FRED_REDIS_CLUSTER_PORT";
  pub const CLUSTER_TLS_HOST: &str = "FRED_REDIS_CLUSTER_TLS_HOST";
  pub const CLUSTER_TLS_PORT: &str = "FRED_REDIS_CLUSTER_TLS_PORT";
  pub const SENTINEL_HOST: &str = "FRED_REDIS_SENTINEL_HOST";
  pub const SENTINEL_PORT: &str = "FRED_REDIS_SENTINEL_PORT";

  pub fn read(name: &str) -> Option<String> {
    env::var_os(name).and_then(|s| s.into_string().ok())
  }

  pub fn try_read(name: &str) -> Result<String, RedisError> {
    read(name).ok_or(RedisError::new(RedisErrorKind::Unknown, "Failed to read env"))
  }
}

/// Read the name of the network, which may have a different prefix on older docker installs.
pub async fn read_network_name(docker: &Docker) -> Result<String, RedisError> {
  let networks = e!(docker.list_networks(None::<ListNetworksOptions<String>>).await)?;

  for network in networks.into_iter() {
    if let Some(ref name) = network.name {
      if name == NETWORK_NAME || name == COMPOSE_NETWORK_NAME {
        return Ok(name.to_owned());
      }
    }
  }
  Err(RedisError::new(
    RedisErrorKind::Unknown,
    "Failed to read fred test network.",
  ))
}

/// Run a command in the bitnami redis container.
pub async fn run_in_redis_container(docker: &Docker, command: Vec<String>) -> Result<Vec<u8>, RedisError> {
  let redis_version = env::try_read("REDIS_VERSION")?;

  let redis_container_config = Config {
    image: Some(format!("bitnami/redis:{}", redis_version)),
    tty: Some(true),
    ..Default::default()
  };
  debug!("Creating test cli container...");
  let container_id = e!(
    docker
      .create_container(
        Some(CreateContainerOptions {
          name: "redis-cli-tmp".to_owned(),
          ..Default::default()
        }),
        redis_container_config,
      )
      .await
  )?
  .id;
  debug!("Starting test cli container...");
  e!(
    docker
      .start_container(&container_id, None::<StartContainerOptions<String>>)
      .await
  )?;

  let test_network = read_network_name(docker).await?;
  debug!("Connecting container to the test network...");
  e!(
    docker
      .connect_network(
        &test_network,
        ConnectNetworkOptions {
          container: container_id.clone(),
          ..Default::default()
        }
      )
      .await
  )?;

  debug!("Running command: {:?}", command);
  let exec = e!(
    docker
      .create_exec(
        &container_id,
        CreateExecOptions {
          attach_stdout: Some(true),
          attach_stderr: Some(true),
          cmd: Some(command),
          ..Default::default()
        }
      )
      .await
  )?
  .id;
  let exec_state = e!(docker.start_exec(&exec, None).await)?;

  let mut out = Vec::with_capacity(1024);
  if let StartExecResults::Attached { mut output, .. } = exec_state {
    while let Some(Ok(msg)) = output.next().await {
      match msg {
        LogOutput::StdOut { message } => out.extend(&message),
        LogOutput::StdErr { message } => {
          warn!("stderr from cli container: {}", String::from_utf8_lossy(&message));
        },
        _ => {},
      };
    }
  } else {
    return Err(RedisError::new(RedisErrorKind::Unknown, "Missing start exec result"));
  }

  debug!("Cleaning up cli container...");
  let result = e!(
    docker
      .remove_container(
        &container_id,
        Some(RemoveContainerOptions {
          force: true,
          ..Default::default()
        }),
      )
      .await
  );
  if let Err(e) = result {
    error!("Failed to remove cli container: {:?}", e);
  }

  Ok(out)
}

/// Read the cluster state via CLUSTER SLOTS.
// This tries to run:
//
// docker run -it --name redis-cli-tmp --rm --network compose_fred-tests bitnami/redis:7.0.9
// redis-cli -h redis-cluster-1 -p 30001 -a bar --raw CLUSTER SLOTS
pub async fn inspect_cluster(tls: bool) -> Result<ClusterRouting, RedisError> {
  let docker = e!(Docker::connect_with_http("", 10, API_DEFAULT_VERSION))?;

  debug!("Connected to docker");
  let password = env::try_read("REDIS_PASSWORD")?;

  let cluster_slots: Vec<String> = if tls {
    let (host, port) = (
      env::try_read(env::CLUSTER_TLS_HOST)?,
      env::try_read(env::CLUSTER_TLS_PORT)?,
    );

    // TODO add ca/cert/key argv
    format!(
      "redis-cli -h {} -p {} -a {} --raw --tls CLUSTER SLOTS",
      host, port, password
    )
    .split(' ')
    .map(|s| s.to_owned())
    .collect()
  } else {
    let (host, port) = (env::try_read(env::CLUSTER_HOST)?, env::try_read(env::CLUSTER_PORT)?);

    format!("redis-cli -h {} -p {} -a {} --raw CLUSTER SLOTS", host, port, password)
      .split(' ')
      .map(|s| s.to_owned())
      .collect()
  };

  let result = run_in_redis_container(&docker, cluster_slots).await?;
  debug!("CLUSTER SLOTS response: {}", String::from_utf8_lossy(&result));
  let parsed: RedisValue = match resp2_decode(&Bytes::from(result))? {
    Some((frame, _)) => frame.into_resp3().try_into()?,
    None => {
      return Err(RedisError::new(
        RedisErrorKind::Unknown,
        "Failed to read cluster slots.",
      ))
    },
  };

  ClusterRouting::from_cluster_slots(parsed, "")
}
