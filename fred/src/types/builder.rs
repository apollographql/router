use crate::{
  clients::{Client, Pool},
  error::{Error, ErrorKind},
  prelude::ReconnectPolicy,
  types::config::{Config, ConnectionConfig, PerformanceConfig, ServerConfig},
};

#[cfg(not(feature = "glommio"))]
use crate::clients::ExclusivePool;
#[cfg(feature = "subscriber-client")]
use crate::clients::SubscriberClient;
#[cfg(feature = "dynamic-pool")]
use crate::{clients::DynamicPool, types::config::DynamicPoolConfig};
#[cfg(feature = "sentinel-client")]
use crate::{clients::SentinelClient, types::config::SentinelConfig};

/// A client and pool builder interface.
///
/// ```rust
/// # use std::time::Duration;
/// # use redis_protocol::resp3::types::RespVersion;
/// # use fred::prelude::*;
/// fn example() -> Result<(), Error> {
///   // use default values
///   let client = Builder::default_centralized().build()?;
///
///   // or initialize from a URL or config
///   let config = Config::from_url("redis://localhost:6379/1")?;
///   let mut builder = Builder::from_config(config);
///   // or modify values in place (creating defaults if needed)
///   builder
///     .with_config(|config| {
///       config.version = RespVersion::RESP3;
///       config.fail_fast = true;
///     })
///     .with_connection_config(|config| {
///       config.tcp = TcpConfig {
///         nodelay: Some(true),
///         ..Default::default()
///       };
///       config.internal_command_timeout = Duration::from_secs(10);
///     });
///   // or overwrite configuration structs in place
///   builder.set_policy(ReconnectPolicy::new_exponential(0, 100, 30_000, 2));
///   builder.set_performance_config(PerformanceConfig::default());
///
///   // reuse the builder as needed to create any kind of client
///   let client = builder.build()?;
///   let pool = builder.build_pool(3)?;
///   let subscriber = builder.build_subscriber_client()?;
///
///   // ...
///
///   Ok(())
/// }
/// ```
#[derive(Clone, Debug)]
pub struct Builder {
  config:      Option<Config>,
  performance: PerformanceConfig,
  connection:  ConnectionConfig,
  policy:      Option<ReconnectPolicy>,
  #[cfg(feature = "sentinel-client")]
  sentinel:    Option<SentinelConfig>,
  #[cfg(feature = "dynamic-pool")]
  pool_config: Option<DynamicPoolConfig>,
}

impl Default for Builder {
  fn default() -> Self {
    Builder {
      config:                                       None,
      performance:                                  PerformanceConfig::default(),
      connection:                                   ConnectionConfig::default(),
      policy:                                       None,
      #[cfg(feature = "sentinel-client")]
      sentinel:                                     None,
      #[cfg(feature = "dynamic-pool")]
      pool_config:                                  None,
    }
  }
}

impl Builder {
  /// Create a new builder instance with default config values for a centralized deployment.
  pub fn default_centralized() -> Self {
    Builder {
      config: Some(Config {
        server: ServerConfig::default_centralized(),
        ..Default::default()
      }),
      ..Default::default()
    }
  }

  /// Create a new builder instance with default config values for a clustered deployment.
  pub fn default_clustered() -> Self {
    Builder {
      config: Some(Config {
        server: ServerConfig::default_clustered(),
        ..Default::default()
      }),
      ..Default::default()
    }
  }

  /// Create a new builder instance from the provided client config.
  pub fn from_config(config: Config) -> Self {
    Builder {
      config: Some(config),
      ..Default::default()
    }
  }

  /// Read the client config.
  pub fn get_config(&self) -> Option<&Config> {
    self.config.as_ref()
  }

  /// Read the reconnection policy.
  pub fn get_policy(&self) -> Option<&ReconnectPolicy> {
    self.policy.as_ref()
  }

  /// Read the performance config.
  pub fn get_performance_config(&self) -> &PerformanceConfig {
    &self.performance
  }

  /// Read the connection config.
  pub fn get_connection_config(&self) -> &ConnectionConfig {
    &self.connection
  }

  /// Read the sentinel client config.
  #[cfg(feature = "sentinel-client")]
  #[cfg_attr(docsrs, doc(cfg(feature = "sentinel-client")))]
  pub fn get_sentinel_config(&self) -> Option<&Config> {
    self.config.as_ref()
  }

  /// Read the dynamic pool config.
  #[cfg(feature = "dynamic-pool")]
  #[cfg_attr(docsrs, doc(cfg(feature = "dynamic-pool")))]
  pub fn get_pool_config(&self) -> Option<&DynamicPoolConfig> {
    self.pool_config.as_ref()
  }

  /// Overwrite the client config on the builder.
  pub fn set_config(&mut self, config: Config) -> &mut Self {
    self.config = Some(config);
    self
  }

  /// Overwrite the reconnection policy on the builder.
  pub fn set_policy(&mut self, policy: ReconnectPolicy) -> &mut Self {
    self.policy = Some(policy);
    self
  }

  /// Overwrite the performance config on the builder.
  pub fn set_performance_config(&mut self, config: PerformanceConfig) -> &mut Self {
    self.performance = config;
    self
  }

  /// Overwrite the connection config on the builder.
  pub fn set_connection_config(&mut self, config: ConnectionConfig) -> &mut Self {
    self.connection = config;
    self
  }

  /// Overwrite the sentinel config on the builder.
  #[cfg(feature = "sentinel-client")]
  #[cfg_attr(docsrs, doc(cfg(feature = "sentinel-client")))]
  pub fn set_sentinel_config(&mut self, config: SentinelConfig) -> &mut Self {
    self.sentinel = Some(config);
    self
  }

  /// Overwrite the pool config on the builder.
  #[cfg(feature = "dynamic-pool")]
  #[cfg_attr(docsrs, doc(cfg(feature = "dynamic-pool")))]
  pub fn set_pool_config(&mut self, config: DynamicPoolConfig) -> &mut Self {
    self.pool_config = Some(config);
    self
  }

  /// Modify the client config in place, creating a new one with default centralized values first if needed.
  pub fn with_config<F>(&mut self, func: F) -> &mut Self
  where
    F: FnOnce(&mut Config),
  {
    if let Some(config) = self.config.as_mut() {
      func(config);
    } else {
      let mut config = Config::default();
      func(&mut config);
      self.config = Some(config);
    }

    self
  }

  /// Modify the performance config in place, creating a new one with default values first if needed.
  pub fn with_performance_config<F>(&mut self, func: F) -> &mut Self
  where
    F: FnOnce(&mut PerformanceConfig),
  {
    func(&mut self.performance);
    self
  }

  /// Modify the connection config in place, creating a new one with default values first if needed.
  pub fn with_connection_config<F>(&mut self, func: F) -> &mut Self
  where
    F: FnOnce(&mut ConnectionConfig),
  {
    func(&mut self.connection);
    self
  }

  /// Modify the sentinel config in place, creating a new one with default values first if needed.
  #[cfg(feature = "sentinel-client")]
  #[cfg_attr(docsrs, doc(cfg(feature = "sentinel-client")))]
  pub fn with_sentinel_config<F>(&mut self, func: F) -> &mut Self
  where
    F: FnOnce(&mut SentinelConfig),
  {
    if let Some(config) = self.sentinel.as_mut() {
      func(config);
    } else {
      let mut config = SentinelConfig::default();
      func(&mut config);
      self.sentinel = Some(config);
    }

    self
  }

  /// Modify the pool config in place, creating a new one with default values first if needed.
  #[cfg(feature = "dynamic-pool")]
  #[cfg_attr(docsrs, doc(cfg(feature = "dynamic-pool")))]
  pub fn with_pool_config<F>(&mut self, func: F) -> &mut Self
  where
    F: FnOnce(&mut DynamicPoolConfig),
  {
    if let Some(config) = self.pool_config.as_mut() {
      func(config);
    } else {
      let mut config = DynamicPoolConfig::default();
      func(&mut config);
      self.pool_config = Some(config);
    }

    self
  }

  /// Create a new client.
  pub fn build(&self) -> Result<Client, Error> {
    if let Some(config) = self.config.as_ref() {
      Ok(Client::new(
        config.clone(),
        Some(self.performance.clone()),
        Some(self.connection.clone()),
        self.policy.clone(),
      ))
    } else {
      Err(Error::new(ErrorKind::Config, "Missing client configuration."))
    }
  }

  /// Create a new client pool.
  pub fn build_pool(&self, size: usize) -> Result<Pool, Error> {
    if let Some(config) = self.config.as_ref() {
      Pool::new(
        config.clone(),
        Some(self.performance.clone()),
        Some(self.connection.clone()),
        self.policy.clone(),
        size,
      )
    } else {
      Err(Error::new(ErrorKind::Config, "Missing client configuration."))
    }
  }

  /// Create a new exclusive client pool.
  #[cfg(not(feature = "glommio"))]
  pub fn build_exclusive_pool(&self, size: usize) -> Result<ExclusivePool, Error> {
    if let Some(config) = self.config.as_ref() {
      ExclusivePool::new(
        config.clone(),
        Some(self.performance.clone()),
        Some(self.connection.clone()),
        self.policy.clone(),
        size,
      )
    } else {
      Err(Error::new(ErrorKind::Config, "Missing client configuration."))
    }
  }

  /// Crete a new dynamic client pool.
  #[cfg(feature = "dynamic-pool")]
  #[cfg_attr(docsrs, doc(cfg(feature = "dynamic-pool")))]
  pub fn build_dynamic_pool(&self) -> Result<DynamicPool, Error> {
    let config = match self.config.as_ref() {
      Some(config) => config.clone(),
      None => return Err(Error::new(ErrorKind::Config, "Missing client configuration.")),
    };
    let pool_config = self.pool_config.as_ref().cloned().unwrap_or_default();

    DynamicPool::new(
      config,
      Some(self.performance.clone()),
      Some(self.connection.clone()),
      self.policy.clone(),
      pool_config,
    )
  }

  /// Create a new subscriber client.
  #[cfg(feature = "subscriber-client")]
  #[cfg_attr(docsrs, doc(cfg(feature = "subscriber-client")))]
  pub fn build_subscriber_client(&self) -> Result<SubscriberClient, Error> {
    if let Some(config) = self.config.as_ref() {
      Ok(SubscriberClient::new(
        config.clone(),
        Some(self.performance.clone()),
        Some(self.connection.clone()),
        self.policy.clone(),
      ))
    } else {
      Err(Error::new(ErrorKind::Config, "Missing client configuration."))
    }
  }

  /// Create a new sentinel client.
  ///
  /// This is only necessary if callers need to communicate directly with sentinel nodes. Use a
  /// `ServerConfig::Sentinel` to interact with Redis servers behind a sentinel layer.
  #[cfg(feature = "sentinel-client")]
  #[cfg_attr(docsrs, doc(cfg(feature = "sentinel-client")))]
  pub fn build_sentinel_client(&self) -> Result<SentinelClient, Error> {
    if let Some(config) = self.sentinel.as_ref() {
      Ok(SentinelClient::new(
        config.clone(),
        Some(self.performance.clone()),
        Some(self.connection.clone()),
        self.policy.clone(),
      ))
    } else {
      Err(Error::new(ErrorKind::Config, "Missing sentinel client configuration."))
    }
  }
}
