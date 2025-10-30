use std::str::FromStr;

use itertools::Itertools;
use redis::ConnectionInfo;
use redis::FromRedisValue;
use redis::Pipeline;
use redis::RedisError;
use url::Url;

use super::Config;

#[derive(thiserror::Error, Debug)]
pub(super) enum Error {
    #[error("{0}")]
    Redis(#[from] RedisError),
    #[error("{0}")]
    Configuration(#[from] ConfigError),
    #[error("{0}")]
    BuildPool(#[from] deadpool::managed::BuildError),
    #[error("{self:?}")]
    UsePool(#[from] deadpool::managed::PoolError<RedisError>),
}

// TODO: this is a true pool - getting an item from it removes the ability for
//  other elements to access it.
//  We might not want this, but deadpool is an easy way to get started with this
//  new redis library, so we should consider what we want here
#[derive(Clone)]
pub(super) enum Pool {
    Standard(deadpool::managed::Pool<deadpool_redis::Manager, deadpool_redis::Connection>),
    Cluster(
        deadpool::managed::Pool<
            deadpool_redis::cluster::Manager,
            deadpool_redis::cluster::Connection,
        >,
    ),
    Sentinel(
        deadpool::managed::Pool<
            deadpool_redis::sentinel::Manager,
            deadpool_redis::sentinel::Connection,
        >,
    ),
}

impl Pool {
    fn new_standard_manager(
        _config: &Config,
        connection_info: ConnectionInfo,
    ) -> Result<deadpool_redis::Manager, Error> {
        // TODO: handle other config params (incl TLS)
        Ok(deadpool_redis::Manager::new(connection_info)?)
    }

    fn new_cluster_manager(
        config: &Config,
        connection_info: Vec<ConnectionInfo>,
    ) -> Result<deadpool_redis::cluster::Manager, Error> {
        // TODO: handle other config params (incl TLS)
        Ok(deadpool_redis::cluster::Manager::new(
            connection_info,
            config.read_from_replicas,
        )?)
    }

    fn new_sentinel_manager(
        _config: &Config,
        _connection_info: Vec<ConnectionInfo>,
    ) -> Result<deadpool_redis::sentinel::Manager, Error> {
        //! It's important to note that the sentinel nodes may have different usernames or passwords,
        //! so the authentication info for them must be entered separately.
        // TODO: need to update the merging to accommodate this..
        todo!();
    }

    fn new_pool<M: deadpool::managed::Manager, W: From<deadpool::managed::Object<M>>>(
        config: Config,
        manager: M,
    ) -> Result<deadpool::managed::Pool<M, W>, Error> {
        let builder = deadpool::managed::Pool::builder(manager)
            .runtime(deadpool::Runtime::Tokio1)
            .queue_mode(deadpool::managed::QueueMode::Fifo)
            .create_timeout(Some(config.timeout))
            .recycle_timeout(Some(config.timeout))
            .wait_timeout(Some(config.timeout))
            .max_size(config.pool_size as usize);
        Ok(builder.build()?)
    }

    fn new_standard(config: Config, connection_info: Vec<ConnectionInfo>) -> Result<Self, Error> {
        // TODO: this doesn't actually connect to redis?
        // TODO: handle all config params...
        if connection_info.len() > 1 {
            return Err(ConfigError::ExtraUrls)?;
        }

        let connection_info = connection_info
            .into_iter()
            .next()
            .ok_or(ConfigError::NoUrls)?;
        let manager = Self::new_standard_manager(&config, connection_info)?;
        let pool = Self::new_pool(config, manager)?;
        Ok(Self::Standard(pool))
    }
    fn new_cluster(config: Config, connection_info: Vec<ConnectionInfo>) -> Result<Self, Error> {
        // TODO: this doesn't actually connect to redis?
        // TODO: handle all config params...
        let manager = Self::new_cluster_manager(&config, connection_info)?;
        let pool = Self::new_pool(config, manager)?;
        Ok(Self::Cluster(pool))
    }

    fn new_sentinel(config: Config, connection_info: Vec<ConnectionInfo>) -> Result<Self, Error> {
        // TODO: this doesn't actually connect to redis?
        // TODO: handle all config params...
        let manager = Self::new_sentinel_manager(&config, connection_info)?;
        let pool = Self::new_pool(config, manager)?;
        Ok(Self::Sentinel(pool))
    }
}

impl Pool {
    pub(super) async fn query_async<V: FromRedisValue>(
        &self,
        pipeline: Pipeline,
    ) -> Result<V, Error> {
        match self {
            Pool::Standard(pool) => {
                let mut conn = pool.get().await?;
                Ok(pipeline.query_async(&mut conn).await?)
            }
            Pool::Cluster(pool) => {
                let mut conn = pool.get().await?;
                Ok(pipeline.query_async(&mut conn).await?)
            }
            Pool::Sentinel(pool) => {
                let mut conn = pool.get().await?;
                Ok(pipeline.query_async(&mut conn).await?)
            }
        }
    }
    pub(super) async fn exec_async(&self, pipeline: Pipeline) -> Result<(), Error> {
        match self {
            Pool::Standard(pool) => {
                let mut conn = pool.get().await?;
                Ok(pipeline.exec_async(&mut conn).await?)
            }
            Pool::Cluster(pool) => {
                let mut conn = pool.get().await?;
                Ok(pipeline.exec_async(&mut conn).await?)
            }
            Pool::Sentinel(pool) => {
                let mut conn = pool.get().await?;
                Ok(pipeline.exec_async(&mut conn).await?)
            }
        }
    }
}

impl TryFrom<Config> for Pool {
    type Error = Error;

    fn try_from(mut config: Config) -> Result<Self, Self::Error> {
        let (mode, urls) = update_schemes(config.urls)?;
        config.urls = urls;

        let connection_info = create_connection_info(&config)?;

        match mode {
            RedisMode::Standard => Self::new_standard(config, connection_info),
            RedisMode::Cluster => Self::new_cluster(config, connection_info),
            RedisMode::Sentinel => Self::new_sentinel(config, connection_info),
        }
    }
}

#[derive(Debug, thiserror::Error)]
enum ConfigError {
    #[error("{self:?}")]
    NoUrls,
    #[error("{self:?}")]
    ExtraUrls,
    #[error("{self:?}")]
    UnsupportedScheme,
    #[error("{self:?}")]
    MismatchedSchemes,
    #[error("{self:?}")]
    UnableToUpdateScheme,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
enum RedisMode {
    Standard,
    Cluster,
    Sentinel,
}

fn convert_scheme(scheme: &str) -> Option<(&'static str, RedisMode)> {
    match scheme {
        "redis" => Some(("redis", RedisMode::Standard)),
        "rediss" => Some(("rediss", RedisMode::Standard)),
        "redis-cluster" => Some(("redis", RedisMode::Cluster)),
        "rediss-cluster" => Some(("rediss", RedisMode::Cluster)),
        "redis-sentinel" => Some(("redis", RedisMode::Sentinel)),
        "rediss-sentinel" => Some(("rediss", RedisMode::Sentinel)),
        _ => None,
    }
}

fn update_schemes(mut urls: Vec<Url>) -> Result<(RedisMode, Vec<Url>), ConfigError> {
    if urls.is_empty() {
        return Err(ConfigError::NoUrls);
    }

    if urls.len() == 1 {
        let mut url = urls.remove(0);
        let (scheme, mode) = convert_scheme(url.scheme()).ok_or(ConfigError::UnsupportedScheme)?;

        url.set_scheme(scheme)
            .map_err(|_| ConfigError::UnableToUpdateScheme)?;
        return Ok((mode, vec![url]));
    }

    let schemes_and_modes: Vec<_> = urls
        .iter()
        .map(|url| convert_scheme(url.scheme()))
        .collect();
    if schemes_and_modes.iter().any(|s| s.is_none()) {
        return Err(ConfigError::MismatchedSchemes);
    }
    let (schemes, modes): (Vec<_>, Vec<_>) = schemes_and_modes.into_iter().flatten().unzip();

    let schemes: Vec<&'static str> = schemes.into_iter().unique().collect();
    let modes: Vec<RedisMode> = modes.into_iter().unique().collect();

    // make sure all have the same scheme: redis or rediss
    if schemes.len() != 1 {
        return Err(ConfigError::MismatchedSchemes);
    }

    let scheme = schemes[0];

    // work out what mode redis is in; sentinel cannot be paired with anything else
    if modes.contains(&RedisMode::Sentinel) && modes.len() != 1 {
        return Err(ConfigError::MismatchedSchemes);
    }

    let mode = if modes.contains(&RedisMode::Sentinel) {
        RedisMode::Sentinel
    } else {
        RedisMode::Cluster
    };

    for url in &mut urls {
        url.set_scheme(scheme)
            .map_err(|_| ConfigError::UnableToUpdateScheme)?;
    }

    Ok((mode, urls))
}

fn create_connection_info(config: &Config) -> Result<Vec<ConnectionInfo>, Error> {
    let mut connection_info = Vec::with_capacity(config.urls.len());
    for url in config.urls.iter() {
        let mut info = redis::ConnectionInfo::from_str(url.as_str())?;
        if let Some(username) = config.username.clone() {
            info.redis.username = Some(username);
        }
        if let Some(password) = config.password.clone() {
            info.redis.password = Some(password);
        }
        connection_info.push(info);
    }
    Ok(connection_info)
}
