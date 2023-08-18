use std::pin::Pin;

use derivative::Derivative;
use derive_more::Display;
use futures::prelude::*;

use crate::router::Event;
use crate::router::Event::Shutdown;

type ShutdownFuture = Pin<Box<dyn Future<Output = ()> + Send>>;

/// Specifies when the Router’s HTTP server should gracefully shutdown
#[derive(Display, Derivative)]
#[derivative(Debug)]
#[non_exhaustive]
pub enum ShutdownSource {
    /// No graceful shutdown
    #[display(fmt = "None")]
    None,

    /// A custom shutdown future.
    #[display(fmt = "Custom")]
    Custom(#[derivative(Debug = "ignore")] ShutdownFuture),

    /// Watch for Ctl-C signal.
    #[display(fmt = "CtrlC")]
    CtrlC,
}

impl ShutdownSource {
    /// Convert this shutdown hook into a future. Allows for unified handling later.
    pub(crate) fn into_stream(self) -> impl Stream<Item = Event> {
        match self {
            ShutdownSource::None => stream::pending::<Event>().boxed(),
            ShutdownSource::Custom(future) => future.map(|_| Shutdown).into_stream().boxed(),
            ShutdownSource::CtrlC => {
                #[cfg(not(unix))]
                {
                    async {
                        tokio::signal::ctrl_c()
                            .await
                            .expect("Failed to install CTRL+C signal handler");
                    }
                    .map(|_| Shutdown)
                    .into_stream()
                    .boxed()
                }

                #[cfg(unix)]
                future::select(
                    tokio::signal::ctrl_c().map(|s| s.ok()).boxed(),
                    async {
                        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                            .expect("Failed to install SIGTERM signal handler")
                            .recv()
                            .await
                    }
                    .boxed(),
                )
                .map(|_| Shutdown)
                .into_stream()
                .boxed()
            }
        }
    }
}
