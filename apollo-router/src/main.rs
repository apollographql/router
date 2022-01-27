//! Main entry point for CLI command to start server.

use anyhow::{ensure, Context, Result};
use apollo_router::configuration::Configuration;
use apollo_router::GLOBAL_ENV_FILTER;
use apollo_router::{ApolloRouter, ConfigurationKind, SchemaKind, ShutdownKind, State};
use directories::ProjectDirs;
use futures::prelude::*;
use std::ffi::OsStr;
use std::fmt;
use std::path::PathBuf;
use structopt::StructOpt;
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

    /// Configuration location relative to the project directory.
    #[structopt(short, long = "config", parse(from_os_str), env)]
    configuration_path: Option<PathBuf>,

    /// Schema location relative to the project directory.
    #[structopt(short, long = "supergraph", parse(from_os_str), env)]
    supergraph_path: Option<PathBuf>,
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

fn main() -> Result<()> {
    let mut builder = tokio::runtime::Builder::new_multi_thread();
    builder.enable_all();
    if let Some(nb) = std::env::var("ROUTER_NUM_CORES")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
    {
        builder.worker_threads(nb);
    }
    let runtime = builder.build()?;
    runtime.block_on(rt_main())
}

async fn rt_main() -> Result<()> {
    let opt = Opt::from_args();

    let env_filter = std::env::var("RUST_LOG").ok().unwrap_or(opt.env_filter);

    let builder = tracing_subscriber::fmt::fmt()
        .with_env_filter(EnvFilter::try_new(&env_filter).context("could not parse log")?);
    if atty::is(atty::Stream::Stdout) {
        builder.init();
    } else {
        builder.json().init();
    }

    GLOBAL_ENV_FILTER.set(env_filter).unwrap();

    let current_directory = std::env::current_dir()?;

    let configuration = opt
        .configuration_path
        .as_ref()
        .map(|path| {
            let path = if path.is_relative() {
                current_directory.join(path)
            } else {
                path.to_path_buf()
            };

            ConfigurationKind::File {
                path,
                watch: opt.watch,
                delay: None,
            }
        })
        .unwrap_or_else(|| ConfigurationKind::Instance(Configuration::builder().build().boxed()));

    ensure!(
        opt.supergraph_path.is_some(),
        r#"
💫 Apollo Router requires a supergraph to be set using '--supergraph':

    $ ./router --supergraph <file>`
  
🪐 The supergraph can be built or downloaded from the Apollo Registry
   using the Rover CLI. To find out how, see:
    
    https://www.apollographql.com/docs/rover/supergraphs/.

🧪 If you're just experimenting, you can download and use an example
   supergraph with pre-deployed subgraphs:

    $ curl -L https://supergraph.demo.starstuff.dev/ > starstuff.graphql

   Then run the Apollo Router with that supergraph:

    $ ./router --supergraph starstuff.graphql

"#
    );

    let supergraph_path = opt.supergraph_path.unwrap();

    let supergraph_path = if supergraph_path.is_relative() {
        current_directory.join(supergraph_path)
    } else {
        supergraph_path
    };

    let schema = SchemaKind::File {
        path: supergraph_path,
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

    let server = ApolloRouter::builder()
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
                    tracing::info!(
                        r#"Starting Apollo Router
*******************************************************************
⚠️  Experimental software, not YET recommended for production use ⚠️
*******************************************************************"#
                    )
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
