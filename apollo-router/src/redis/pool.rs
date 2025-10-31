use std::fmt::Debug;
use std::ops::Deref;
use std::str::FromStr;
use std::sync::Arc;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;
use std::time::Duration;

use itertools::Itertools;
use redis::AsyncCommands;
use redis::ConnectionInfo;
use redis::FromRedisValue;
use redis::Pipeline;
use redis::RedisError;
use redis::SetExpiry;
use redis::SetOptions;
use redis::ToRedisArgs;
use redis::aio::MultiplexedConnection;
use redis::cluster_async::ClusterConnection;
use tokio::sync::RwLock;
use tokio::task::JoinError;
use tokio::task::JoinSet;
use tokio::time::timeout;
use tokio_util::task::AbortOnDropHandle;
use url::Url;

use crate::redis::Config;

#[derive(Clone)]
enum Connection {
    Standard(MultiplexedConnection),
    Cluster(ClusterConnection),
}

// standard: standalone redis instance
// cluster: sharded master nodes, each with replicas
// sentinel: single master node with replicas
enum Client {
    Standard(redis::Client),
    Cluster(redis::cluster::ClusterClient),
    Sentinel((String, redis::sentinel::Sentinel)), // (service name, client)
}

impl Client {
    // NB: cluster connection should automatically route to replicas, so using this will not connect to
    // sentinel replicas. TODO investigate relative popularity of cluster vs sentinel
    async fn connection(&mut self) -> Result<Connection, RedisError> {
        match self {
            Client::Standard(client) => {
                // TODO: use one with timeout, fix params?
                let connection = client.get_multiplexed_tokio_connection().await?;
                Ok(Connection::Standard(connection))
            }
            Client::Cluster(client) => {
                // TODO: fix params etc
                let connection = client.get_async_connection().await?;
                Ok(Connection::Cluster(connection))
            }
            Client::Sentinel((service_name, sentinel)) => {
                // TODO: test this, fix params
                let client = sentinel.async_master_for(service_name, None).await?;
                let connection = client.get_multiplexed_tokio_connection().await?;
                Ok(Connection::Standard(connection))
            }
        }
    }
}

#[derive(Clone)]
enum ConnectionState {
    Connected(Connection),
    Disconnected,
}

#[derive(thiserror::Error, Debug)]
pub(super) enum Error {
    #[error("{self:?}")]
    Redis(#[from] RedisError),
    #[error("{self:?}")]
    OutOfBounds,
    #[error("{self:?}")]
    Disconnected,
    #[error("{0}")]
    Config(#[from] ConfigError),
    #[error("{0}")]
    Join(#[from] JoinError),
}

#[derive(Clone)]
pub(super) struct Pool {
    index: Arc<AtomicUsize>,
    size: usize,
    clients: Vec<Arc<RwLock<Client>>>,
    connections: Vec<Arc<RwLock<ConnectionState>>>,
}

// TODO: need to figure out how to mark connection as disconnected

impl Pool {
    // TODO: should this try other clients if this one is down?
    async fn connection(&self) -> Result<Connection, Error> {
        let mut index = self.index.fetch_add(1, Ordering::Relaxed);
        index = index % self.size;

        for offset in 0..self.size {
            let index = (index + offset) % self.size;

            let possible_connection = self.connections.get(index).ok_or(Error::OutOfBounds)?;
            let connection_state = possible_connection.read().await;

            match connection_state.deref() {
                ConnectionState::Connected(connection) => return Ok(connection.clone()),
                ConnectionState::Disconnected => {
                    let pool = self.clone();
                    tokio::spawn(timeout(Duration::from_secs(10), async move {
                        // TODO: report error
                        pool.reconnect(index).await
                    }));
                }
            }
        }

        // No connection available
        Err(Error::Disconnected)
    }

    async fn reconnect(&self, index: usize) -> Result<(), Error> {
        // grabbing a write lock on the client means that only one reconnection per client
        // will happen at a time

        let client_ref = self.clients.get(index).ok_or(Error::OutOfBounds)?;
        let mut client = client_ref.write().await;

        let connection = client.connection().await?;

        let connection_ref = self.connections.get(index).ok_or(Error::OutOfBounds)?;
        let mut connection_ref_lock = connection_ref.write().await;
        *connection_ref_lock = ConnectionState::Connected(connection);

        Ok(())
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

impl Pool {
    // TODO: addtl configs?
    fn new_standard_clients(
        pool_size: usize,
        connection_info: ConnectionInfo,
    ) -> Result<Vec<Arc<RwLock<Client>>>, RedisError> {
        let mut clients = Vec::with_capacity(pool_size);
        for _ in 0..pool_size {
            let client = redis::Client::open(connection_info.clone())?;
            clients.push(Arc::new(RwLock::new(Client::Standard(client))));
        }

        Ok(clients)
    }

    // TODO: addtl configs?
    fn new_cluster_clients(
        pool_size: usize,
        connection_info: Vec<ConnectionInfo>,
    ) -> Result<Vec<Arc<RwLock<Client>>>, RedisError> {
        let mut clients = Vec::with_capacity(pool_size);
        for _ in 0..pool_size {
            let client = redis::cluster::ClusterClient::new(connection_info.clone())?;
            clients.push(Arc::new(RwLock::new(Client::Cluster(client))));
        }

        Ok(clients)
    }

    // TODO: addtl configs?
    fn new_sentinel_clients(
        pool_size: usize,
        connection_info: Vec<ConnectionInfo>,
    ) -> Result<Vec<Arc<RwLock<Client>>>, RedisError> {
        let mut clients = Vec::with_capacity(pool_size);
        for _ in 0..pool_size {
            let client = redis::sentinel::Sentinel::build(connection_info.clone())?;
            let service_name = String::from("service"); // TODO: where is this from
            clients.push(Arc::new(RwLock::new(Client::Sentinel((
                service_name,
                client,
            )))));
        }

        Ok(clients)
    }

    fn default_partial(pool_size: usize) -> Self {
        let connections = (0..pool_size)
            .map(|_| Arc::new(RwLock::new(ConnectionState::Disconnected)))
            .collect();
        Self {
            index: Arc::new(Default::default()),
            size: pool_size,
            clients: Vec::default(),
            connections,
        }
    }

    fn new_standard(config: Config, connection_info: Vec<ConnectionInfo>) -> Result<Self, Error> {
        // TODO: this doesn't actually connect to redis?
        // TODO: handle all config params...
        if connection_info.len() > 1 {
            todo!("return error");
        }

        let pool_size = config.pool_size as usize;
        let connection_info = connection_info.into_iter().next().expect("must have 1");

        Ok(Pool {
            clients: Self::new_standard_clients(pool_size, connection_info)?,
            ..Self::default_partial(pool_size)
        })
    }

    fn new_cluster(config: Config, connection_info: Vec<ConnectionInfo>) -> Result<Self, Error> {
        // TODO: this doesn't actually connect to redis?
        // TODO: handle all config params...
        let pool_size = config.pool_size as usize;
        Ok(Pool {
            clients: Self::new_cluster_clients(pool_size, connection_info)?,
            ..Self::default_partial(pool_size)
        })
    }

    fn new_sentinel(config: Config, connection_info: Vec<ConnectionInfo>) -> Result<Self, Error> {
        // TODO: this doesn't actually connect to redis?
        // TODO: handle all config params...
        let pool_size = config.pool_size as usize;
        Ok(Pool {
            clients: Self::new_sentinel_clients(pool_size, connection_info)?,
            ..Self::default_partial(pool_size)
        })
    }

    pub(super) async fn connect_all(&self) -> Result<(), Error> {
        let mut join_set = JoinSet::new();
        for i in 0..self.size {
            let pool = self.clone();
            join_set.spawn(async move { pool.reconnect(i).await });
        }

        for result in join_set.join_all().await {
            result?;
        }

        Ok(())
    }
}

// struct AbortOnDrop<T>(Vec<JoinHandle<T>>);
// impl<T> Drop for AbortOnDrop<T> {
//     fn drop(&mut self) {
//         for handle in self.0.iter() {
//             handle.abort();
//         }
//     }
// }
// impl<T> AbortOnDrop<T> {
//     fn with_capacity(capacity: usize) -> Self {
//         Self(Vec::with_capacity(capacity))
//     }
//     fn push(&mut self, handle: JoinHandle<T>) {
//         self.0.push(handle);
//     }
//     async fn join(&self) -> Vec<T> {
//         let mut results = Vec::with_capacity(self.0.len());
//         for result in self.0.iter() {
//             results.push(result.await);
//         }
//         results
//     }
// }

impl Pool {
    pub(super) async fn get<V: FromRedisValue + Send + 'static>(
        &self,
        key: String,
    ) -> Result<V, Error> {
        // TODO: timeout
        // send either an MGET or many gets, depending on the connection
        match self.connection().await? {
            Connection::Standard(mut conn) => Ok(conn.get(key).await?),
            Connection::Cluster(mut conn) => Ok(conn.get(key).await?),
        }
    }
    pub(super) async fn get_multiple<V: FromRedisValue + Send + 'static>(
        &self,
        keys: Vec<String>,
    ) -> Result<Vec<V>, Error> {
        // TODO: timeout
        // send either an MGET or many gets, depending on the connection
        match self.connection().await? {
            Connection::Standard(mut conn) => Ok(conn.mget(keys).await?),
            Connection::Cluster(conn) => {
                let key_len = keys.len();
                let mut tasks = Vec::with_capacity(key_len);
                for key in keys {
                    let mut conn = conn.clone();
                    tasks.push(AbortOnDropHandle::new(tokio::spawn(async move {
                        conn.get(key).await
                    })));
                }

                // TODO: support partial success in case one node is down

                let mut results = Vec::with_capacity(key_len);
                for task in tasks {
                    results.push(task.await??);
                }
                Ok(results)
            }
        }
    }

    pub(super) async fn insert_multiple<
        V: FromRedisValue + ToRedisArgs + Send + Sync + 'static + Debug,
    >(
        &self,
        data: Vec<(String, V)>,
        ttl: Option<Duration>,
    ) -> Result<(), Error> {
        // send either a pipeline of sets or many sets, depending on the connection
        let mut options = SetOptions::default().get(false);
        if let Some(ttl) = ttl {
            options = options.with_expiration(SetExpiry::EX(ttl.as_secs()));
        }
        match self.connection().await? {
            Connection::Standard(mut conn) => {
                let mut pipeline = Pipeline::with_capacity(data.len());
                for (key, value) in data {
                    pipeline.set_options(key, value, options);
                }
                Ok(pipeline.exec_async(&mut conn).await?)
            }
            Connection::Cluster(conn) => {
                let key_len = data.len();
                let mut tasks = Vec::with_capacity(key_len);
                for (key, value) in data {
                    let mut conn = conn.clone();
                    tasks.push(AbortOnDropHandle::new(tokio::spawn(async move {
                        conn.set_options(key, value, options).await
                    })));
                }

                for task in tasks {
                    let _: () = task.await??;
                }
                Ok(())
            }
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub(super) enum ConfigError {
    #[error("{self:?}")]
    NoUrls,
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
