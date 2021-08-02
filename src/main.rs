//! Main entry point for CLI command to start server.

use log::LevelFilter;

use server::{ConfigurationKind, FederatedServerError, SchemaKind, ShutdownKind};
use server::{FederatedServer, State};

use directories::ProjectDirs;
use futures::prelude::*;
use std::ffi::OsStr;
use std::fmt::{Display, Formatter};
use std::path::PathBuf;
use structopt::StructOpt;

/// Options for the router
#[derive(StructOpt, Debug)]
#[structopt(name = "router", about = "Apollo federation router")]
struct Opt {
    /// Log level (off|error|warn|info|debug|trace).
    #[structopt(short, long, default_value = "info")]
    log_level: LevelFilter,

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
        short,
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

impl Display for ProjectDir {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
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
async fn main() -> Result<(), FederatedServerError> {
    let opt = Opt::from_args();

    let _ = env_logger::builder().filter_level(opt.log_level).try_init();

    let base_directory = match opt.project_dir.path {
        Some(project_dir) => project_dir,
        None => {
            log::error!("Unable to determine project directory. It must be explicitly set by passing in the '-p' flag");
            return Err(FederatedServerError::StartupError);
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
                    log::info!("Starting Apollo Federation")
                }
                State::Running(port) => {
                    log::info!("Listening on http://{} 🚀", port)
                }
                State::Stopped => {
                    log::info!("Stopped")
                }
                State::Errored => {
                    log::info!("Stopped with error")
                }
            }
            future::ready(())
        })
        .await;

    if let Err(err) = server_handle.await {
        log::error!("{}", err);
        return Err(err);
    }
    Ok(())
}
