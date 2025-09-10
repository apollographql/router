mod client;
mod options;
mod pipeline;
mod pool;

pub use client::Client;
pub use options::WithOptions;
pub use pipeline::Pipeline;
pub use pool::Pool;

#[cfg(not(feature = "glommio"))]
pub use pool::ExclusivePool;

#[cfg(feature = "sentinel-client")]
mod sentinel;
#[cfg(feature = "sentinel-client")]
#[cfg_attr(docsrs, doc(cfg(feature = "sentinel-client")))]
pub use sentinel::SentinelClient;

#[cfg(feature = "subscriber-client")]
mod pubsub;
#[cfg(feature = "subscriber-client")]
#[cfg_attr(docsrs, doc(cfg(feature = "subscriber-client")))]
pub use pubsub::SubscriberClient;

#[cfg(feature = "replicas")]
mod replica;
#[cfg(feature = "replicas")]
#[cfg_attr(docsrs, doc(cfg(feature = "replicas")))]
pub use replica::Replicas;

#[cfg(feature = "transactions")]
mod transaction;
#[cfg(feature = "transactions")]
#[cfg_attr(docsrs, doc(cfg(feature = "transactions")))]
pub use transaction::Transaction;

#[cfg(feature = "dynamic-pool")]
mod dynamic_pool;
#[cfg(feature = "dynamic-pool")]
#[cfg_attr(docsrs, doc(cfg(feature = "dynamic-pool")))]
pub use dynamic_pool::DynamicPool;
