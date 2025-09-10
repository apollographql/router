#[cfg(not(any(feature = "glommio", feature = "smol", feature = "monoio")))]
mod _tokio;
#[cfg(not(any(feature = "glommio", feature = "smol", feature = "monoio")))]
pub use _tokio::*;

#[cfg(feature = "glommio")]
pub(crate) mod glommio;
#[cfg(feature = "glommio")]
pub use glommio::compat::*;

#[cfg(any(feature = "glommio", feature = "smol", feature = "monoio"))]
mod sync;
#[cfg(any(feature = "glommio", feature = "smol", feature = "monoio"))]
pub use sync::*;

#[cfg(not(feature = "glommio"))]
pub use _tokio::ClientLike;
#[cfg(feature = "glommio")]
pub use glommio::interfaces::ClientLike;

#[cfg(not(feature = "glommio"))]
pub(crate) use _tokio::spawn_event_listener;
#[cfg(feature = "glommio")]
#[doc(hidden)]
pub(crate) use glommio::interfaces::spawn_event_listener;
