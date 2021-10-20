//! Main entry point for CLI command to start server.

use anyhow::{Context, Result};
use apollo_router::GLOBAL_ENV_FILTER;
use apollo_router::{
    ConfigurationKind, FederatedServer, FederatedServerError, SchemaKind, ShutdownKind, State,
};
use directories::ProjectDirs;
use futures::prelude::*;
use std::ffi::OsStr;
use std::fmt;
use std::path::PathBuf;
use structopt::StructOpt;
use tracing_subscriber::prelude::*;
use tracing_subscriber::EnvFilter;

/// Options for the router
#[derive(StructOpt, Debug)]
#[structopt(name = "router", about = "Apollo federation router")]
struct Opt {
    /// Log level (off|error|warn|info|debug|trace).
    #[structopt(long = "log", default_value = "info", alias = "loglevel")]
    env_filter: String,

    /// Reload configuration and schema files automatically.
    #[structopt(short, long)]
    watch: bool,

    /// Directory where configuration files are located (OS dependent).
    #[structopt(short, long = "project_dir", parse(from_os_str), env, default_value)]
    project_dir: ProjectDir,

    /// Configuration location relative to the project directory.
    #[structopt(
        short,
        long = "config",
        parse(from_os_str),
        default_value = "configuration.yaml",
        env
    )]
    configuration_path: PathBuf,

    /// Schema location relative to the project directory.
    #[structopt(
        long = "schema",
        parse(from_os_str),
        default_value = "supergraph.graphql",
        env
    )]
    schema_path: PathBuf,
}

/// Wrapper so that structop can display the default config path in the help message.
/// Uses ProjectDirs to get the default location.
#[derive(Debug)]
struct ProjectDir {
    path: Option<PathBuf>,
}

impl Default for ProjectDir {
    fn default() -> Self {
        let dirs = ProjectDirs::from("com", "Apollo", "Federation");
        Self {
            path: dirs.map(|dirs| dirs.config_dir().to_path_buf()),
        }
    }
}

impl From<&OsStr> for ProjectDir {
    fn from(s: &OsStr) -> Self {
        Self {
            path: Some(PathBuf::from(s)),
        }
    }
}

impl fmt::Display for ProjectDir {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.path {
            None => {
                write!(f, "Unknown, -p option must be used.")
            }
            Some(path) => {
                write!(f, "{}", path.to_string_lossy())
            }
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let opt = Opt::from_args();

    tracing_subscriber::fmt::fmt()
        .with_env_filter(EnvFilter::try_new(&opt.env_filter).context("could not parse log")?)
        .finish()
        .init();

    GLOBAL_ENV_FILTER.set(opt.env_filter.clone()).unwrap();

    let base_directory = match opt.project_dir.path {
        Some(project_dir) => project_dir,
        None => {
            tracing::error!(
                "Unable to determine project directory. \
                It must be explicitly set by passing in the '-p' flag",
            );
            return Err(FederatedServerError::StartupError.into());
        }
    };

    let configuration = ConfigurationKind::File {
        path: base_directory.join(opt.configuration_path),
        watch: opt.watch,
        delay: None,
    };

    let schema = SchemaKind::File {
        path: base_directory.join(opt.schema_path),
        watch: opt.watch,
        delay: None,
    };

    // Create your text map propagator & assign it as the global propagator.
    //
    // This is required in order to create the header traceparent used in http_subgraph to
    // propagate the trace id to the subgraph services.
    //
    // /!\ If this is not called, there will be no warning and no header will be sent to the
    //     subgraphs!
    let propagator = opentelemetry::sdk::propagation::TraceContextPropagator::new();
    opentelemetry::global::set_text_map_propagator(propagator);

    let server = FederatedServer::builder()
        .configuration(configuration)
        .schema(schema)
        .shutdown(ShutdownKind::CtrlC)
        .build();
    let mut server_handle = server.serve();
    server_handle
        .state_receiver()
        .for_each(|state| {
            match state {
                State::Startup => {
                    tracing::info!("Starting Apollo Federation")
                }
                State::Running { address, .. } => {
                    tracing::info!("Listening on http://{} 🚀", address)
                }
                State::Stopped => {
                    tracing::info!("Stopped")
                }
                State::Errored => {
                    tracing::info!("Stopped with error")
                }
            }
            future::ready(())
        })
        .await;

    if let Err(err) = server_handle.await {
        tracing::error!("{}", err);
        return Err(err.into());
    }

    Ok(())
}
