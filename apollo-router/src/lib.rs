//! Starts a server that will handle http graphql requests.

extern crate core;

mod axum_http_server_factory;
pub mod configuration;
mod executable;
mod files;
mod http_server_factory;
pub mod plugins;
mod reload;
mod router;
mod router_factory;
mod state_machine;
pub mod subscriber;

pub use executable::{main, rt_main};
pub use router::*;
