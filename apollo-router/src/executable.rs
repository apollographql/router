//! Main entry point for CLI command to start server.

use std::env;
use std::ffi::OsStr;
use std::fmt;
use std::fmt::Debug;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::anyhow;
use anyhow::Result;
use clap::ArgAction;
use clap::Args;
use clap::CommandFactory;
use clap::Parser;
use clap::Subcommand;
use directories::ProjectDirs;
use url::ParseError;
use url::Url;

use crate::configuration;
use crate::configuration::generate_config_schema;
use crate::configuration::generate_upgrade;
use crate::configuration::ConfigurationError;
use crate::plugins::telemetry::reload::init_telemetry;
use crate::router::ConfigurationSource;
use crate::router::RouterHttpServer;
use crate::router::SchemaSource;
use crate::router::ShutdownSource;

// Note: the dhat-heap and dhat-ad-hoc features should not be both enabled. We name our functions
// and variables identically to prevent this from happening.

#[cfg(feature = "dhat-heap")]
#[global_allocator]
pub(crate) static ALLOC: dhat::Alloc = dhat::Alloc;

#[cfg(feature = "dhat-heap")]
pub(crate) static mut DHAT_HEAP_PROFILER: OnceCell<dhat::Profiler> = OnceCell::new();

#[cfg(feature = "dhat-ad-hoc")]
pub(crate) static mut DHAT_AD_HOC_PROFILER: OnceCell<dhat::Profiler> = OnceCell::new();

pub(crate) const APOLLO_ROUTER_DEV_ENV: &str = "APOLLO_ROUTER_DEV";

// Note: Constructor/Destructor functions may not play nicely with tracing, since they run after
// main completes, so don't use tracing, use println!() and eprintln!()..
#[cfg(feature = "dhat-heap")]
fn create_heap_profiler() {
    unsafe {
        match DHAT_HEAP_PROFILER.set(dhat::Profiler::new_heap()) {
            Ok(p) => {
                println!("heap profiler installed: {:?}", p);
                libc::atexit(drop_heap_profiler);
            }
            Err(e) => eprintln!("heap profiler install failed: {:?}", e),
        }
    }
}

#[cfg(feature = "dhat-heap")]
#[no_mangle]
extern "C" fn drop_heap_profiler() {
    unsafe {
        if let Some(p) = DHAT_HEAP_PROFILER.take() {
            drop(p);
        }
    }
}

#[cfg(feature = "dhat-ad-hoc")]
fn create_ad_hoc_profiler() {
    unsafe {
        match DHAT_AD_HOC_PROFILER.set(dhat::Profiler::new_ad_hoc()) {
            Ok(p) => {
                println!("ad-hoc profiler installed: {:?}", p);
                libc::atexit(drop_ad_hoc_profiler);
            }
            Err(e) => eprintln!("ad-hoc profiler install failed: {:?}", e),
        }
    }
}

#[cfg(feature = "dhat-ad-hoc")]
#[no_mangle]
extern "C" fn drop_ad_hoc_profiler() {
    unsafe {
        if let Some(p) = DHAT_AD_HOC_PROFILER.take() {
            drop(p);
        }
    }
}

/// Subcommands
#[derive(Subcommand, Debug)]
enum Commands {
    /// Configuration subcommands.
    Config(ConfigSubcommandArgs),
}

#[derive(Args, Debug)]
struct ConfigSubcommandArgs {
    /// Subcommands
    #[clap(subcommand)]
    command: ConfigSubcommand,
}

#[derive(Subcommand, Debug)]
enum ConfigSubcommand {
    /// Print the json configuration schema.
    Schema,

    /// Print upgraded configuration.
    Upgrade {
        /// The location of the config to upgrade.
        #[clap(value_parser, env = "APOLLO_ROUTER_CONFIG_PATH")]
        config_path: PathBuf,

        /// Print a diff.
        #[clap(action = ArgAction::SetTrue, long)]
        diff: bool,
    },
    /// List all the available experimental configurations with related GitHub discussion
    Experimental,
}

/// Options for the router
#[derive(Parser, Debug)]
#[clap(name = "router", about = "Apollo federation router")]
#[command(disable_version_flag(true))]
pub(crate) struct Opt {
    /// Log level (off|error|warn|info|debug|trace).
    #[clap(
        long = "log",
        default_value = "info",
        alias = "log-level",
        env = "APOLLO_ROUTER_LOG"
    )]
    log_level: String,

    /// Reload configuration and schema files automatically.
    #[clap(
        alias = "hr",
        long = "hot-reload",
        env = "APOLLO_ROUTER_HOT_RELOAD",
        action(ArgAction::SetTrue)
    )]
    hot_reload: bool,

    /// Configuration location relative to the project directory.
    #[clap(
        short,
        long = "config",
        value_parser,
        env = "APOLLO_ROUTER_CONFIG_PATH"
    )]
    config_path: Option<PathBuf>,

    /// Enable development mode.
    #[clap(
        env = APOLLO_ROUTER_DEV_ENV,
        long = "dev",
        hide(true),
        action(ArgAction::SetTrue)
    )]
    dev: bool,

    /// Schema location relative to the project directory.
    #[clap(
        short,
        long = "supergraph",
        value_parser,
        env = "APOLLO_ROUTER_SUPERGRAPH_PATH"
    )]
    supergraph_path: Option<PathBuf>,

    /// Prints the configuration schema.
    #[clap(long, action(ArgAction::SetTrue), hide(true))]
    schema: bool,

    /// Subcommands
    #[clap(subcommand)]
    command: Option<Commands>,

    /// Your Apollo key.
    #[clap(skip = std::env::var("APOLLO_KEY").ok())]
    apollo_key: Option<String>,

    /// Your Apollo graph reference.
    #[clap(skip = std::env::var("APOLLO_GRAPH_REF").ok())]
    apollo_graph_ref: Option<String>,

    /// The endpoints (comma separated) polled to fetch the latest supergraph schema.
    #[clap(long, env, action = ArgAction::Append)]
    // Should be a Vec<Url> when https://github.com/clap-rs/clap/discussions/3796 is solved
    apollo_uplink_endpoints: Option<String>,

    /// The time between polls to Apollo uplink. Minimum 10s.
    #[clap(long, default_value = "10s", value_parser = humantime::parse_duration, env)]
    apollo_uplink_poll_interval: Duration,

    /// Disable sending anonymous usage information to Apollo.
    #[clap(long, env = "APOLLO_TELEMETRY_DISABLED")]
    anonymous_telemetry_disabled: bool,

    /// The timeout for an http call to Apollo uplink. Defaults to 30s.
    #[clap(long, default_value = "30s", value_parser = humantime::parse_duration, env)]
    apollo_uplink_timeout: Duration,

    /// Display version and exit.
    #[clap(action = ArgAction::SetTrue, long, short = 'V')]
    pub(crate) version: bool,
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

/// This is the main router entrypoint.
///
/// Starts a Tokio runtime and runs a Router in it based on command-line options.
/// Returns on fatal error or after graceful shutdown has completed.
///
/// Refer to the examples if you would like to see how to run your own router with plugins.
pub fn main() -> Result<()> {
    #[cfg(feature = "dhat-heap")]
    create_heap_profiler();

    #[cfg(feature = "dhat-ad-hoc")]
    create_ad_hoc_profiler();

    let mut builder = tokio::runtime::Builder::new_multi_thread();
    builder.enable_all();
    if let Some(nb) = std::env::var("APOLLO_ROUTER_NUM_CORES")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
    {
        builder.worker_threads(nb);
    }
    let runtime = builder.build()?;
    runtime.block_on(Executable::builder().start())
}

/// Entry point into creating a router executable with more customization than [`main`].
#[non_exhaustive]
pub struct Executable {}

#[buildstructor::buildstructor]
impl Executable {
    /// Returns a builder that can parse command-line options and run a Router
    /// in an existing Tokio runtime.
    ///
    /// Builder methods:
    ///
    /// * `.config(impl Into<`[`ConfigurationSource`]`>)`
    ///   Optional.
    ///   Specifies where to find the Router configuration.
    ///   The default is the file specified by the `--config` or `-c` CLI option.
    ///
    /// * `.schema(impl Into<`[`SchemaSource`]`>)`
    ///   Optional.
    ///   Specifies when to find the supergraph schema.
    ///   The default is the file specified by the `--supergraph` or `-s` CLI option.
    ///
    /// * `.shutdown(impl Into<`[`ShutdownSource`]`>)`
    ///   Optional.
    ///   Specifies when the Router should shut down gracefully.
    ///   The default is on CTRL+C (`SIGINT`).
    ///
    /// * `.start()`
    ///   Returns a future that resolves to [`anyhow::Result`]`<()>`
    ///   on fatal error or after graceful shutdown has completed.
    ///   Must be called (and the future awaited) in the context of an existing Tokio runtime.
    ///
    /// ```no_run
    /// use apollo_router::{Executable, ShutdownSource};
    /// # #[tokio::main]
    /// # async fn main() -> anyhow::Result<()> {
    /// # use futures::StreamExt;
    /// # let schemas = futures::stream::empty().boxed();
    /// # let configs = futures::stream::empty().boxed();
    /// use apollo_router::{ConfigurationSource, SchemaSource};
    /// Executable::builder()
    ///   .shutdown(ShutdownSource::None)
    ///   .schema(SchemaSource::Stream(schemas))
    ///   .config(ConfigurationSource::Stream(configs))
    ///   .start()
    ///   .await
    /// # }
    /// ```
    #[builder(entry = "builder", exit = "start", visibility = "pub")]
    async fn start(
        shutdown: Option<ShutdownSource>,
        schema: Option<SchemaSource>,
        config: Option<ConfigurationSource>,
    ) -> Result<()> {
        let opt = Opt::parse();

        if opt.version {
            println!("{}", std::env!("CARGO_PKG_VERSION"));
            return Ok(());
        }

        copy_args_to_env();
        init_telemetry(&opt.log_level)?;
        setup_panic_handler();

        if opt.schema {
            eprintln!("`router --schema` is deprecated. Use `router config schema`");
            let schema = generate_config_schema();
            println!("{}", serde_json::to_string_pretty(&schema)?);
            return Ok(());
        }

        let result = match opt.command.as_ref() {
            Some(Commands::Config(ConfigSubcommandArgs {
                command: ConfigSubcommand::Schema,
            })) => {
                let schema = generate_config_schema();
                println!("{}", serde_json::to_string_pretty(&schema)?);
                Ok(())
            }
            Some(Commands::Config(ConfigSubcommandArgs {
                command: ConfigSubcommand::Upgrade { config_path, diff },
            })) => {
                let config_string = std::fs::read_to_string(config_path)?;
                let output = generate_upgrade(&config_string, *diff)?;
                println!("{output}");
                Ok(())
            }
            Some(Commands::Config(ConfigSubcommandArgs {
                command: ConfigSubcommand::Experimental,
            })) => {
                configuration::print_all_experimental_conf();
                Ok(())
            }
            None => Self::inner_start(shutdown, schema, config, opt).await,
        };

        //We should be good to shutdown the tracer provider now as the router should have finished everything.
        opentelemetry::global::shutdown_tracer_provider();
        result
    }

    async fn inner_start(
        shutdown: Option<ShutdownSource>,
        schema: Option<SchemaSource>,
        config: Option<ConfigurationSource>,
        mut opt: Opt,
    ) -> Result<()> {
        let current_directory = std::env::current_dir()?;
        // Enable hot reload when dev mode is enabled
        opt.hot_reload = opt.hot_reload || opt.dev;

        let configuration = match (config, opt.config_path.as_ref()) {
            (Some(_), Some(_)) => {
                return Err(anyhow!(
                    "--config and APOLLO_ROUTER_CONFIG_PATH cannot be used when a custom configuration source is in use"
                ));
            }
            (Some(config), None) => config,
            _ => match opt.config_path.as_ref().map(|path| {
                let path = if path.is_relative() {
                    current_directory.join(path)
                } else {
                    path.to_path_buf()
                };

                ConfigurationSource::File {
                    path,
                    watch: opt.hot_reload,
                    delay: None,
                }
            }) {
                Some(configuration) => configuration,
                None => Default::default(),
            },
        };

        let apollo_telemetry_msg = if opt.anonymous_telemetry_disabled {
            "Anonymous usage data collection is disabled.".to_string()
        } else {
            "Anonymous usage data is gathered to inform Apollo product development.  See https://go.apollo.dev/o/privacy for details.".to_string()
        };

        let apollo_router_msg = format!("Apollo Router v{} // (c) Apollo Graph, Inc. // Licensed as ELv2 (https://go.apollo.dev/elv2)", std::env!("CARGO_PKG_VERSION"));
        let schema = match (schema, opt.supergraph_path, opt.apollo_key) {
            (Some(_), Some(_), _) => {
                return Err(anyhow!(
                    "--supergraph and APOLLO_ROUTER_SUPERGRAPH_PATH cannot be used when a custom schema source is in use"
                ))
            }
            (Some(source), None, _) => source,
            (_, Some(supergraph_path), _) => {
                tracing::info!("{apollo_router_msg}");
                tracing::info!("{apollo_telemetry_msg}");

                let supergraph_path = if supergraph_path.is_relative() {
                    current_directory.join(supergraph_path)
                } else {
                    supergraph_path
                };
                SchemaSource::File {
                    path: supergraph_path,
                    watch: opt.hot_reload,
                    delay: None,
                }
            }
            (_, None, Some(apollo_key)) => {
                tracing::info!("{apollo_router_msg}");
                tracing::info!("{apollo_telemetry_msg}");

                let apollo_graph_ref = opt.apollo_graph_ref.ok_or_else(||anyhow!("cannot fetch the supergraph from Apollo Studio without setting the APOLLO_GRAPH_REF environment variable"))?;
                if opt.apollo_uplink_poll_interval < Duration::from_secs(10) {
                    return Err(anyhow!("Apollo poll interval must be at least 10s"));
                }
                let uplink_endpoints: Option<Vec<Url>> = opt
                    .apollo_uplink_endpoints
                    .map(|e| {
                        e.split(',')
                            .map(|endpoint| Url::parse(endpoint.trim()))
                            .collect::<Result<Vec<Url>, ParseError>>()
                    })
                    .transpose()
                    .map_err(|err| ConfigurationError::InvalidConfiguration {
                        message: "bad value for apollo_uplink_endpoints, cannot parse to an url",
                        error: err.to_string(),
                    })?;

                SchemaSource::Registry {
                    apollo_key,
                    apollo_graph_ref,
                    urls: uplink_endpoints,
                    poll_interval: opt.apollo_uplink_poll_interval,
                    timeout: opt.apollo_uplink_timeout
                }
            }
            _ => {
                return Err(anyhow!(
                    r#"{apollo_router_msg}

⚠️  The Apollo Router requires a composed supergraph schema at startup. ⚠️

👉 DO ONE:

  * Pass a local schema file with the '--supergraph' option:

      $ ./router --supergraph <file_path>

  * Fetch a registered schema from Apollo Studio by setting
    these environment variables:

      $ APOLLO_KEY="..." APOLLO_GRAPH_REF="..." ./router

      For details, see the Apollo docs:
      https://www.apollographql.com/docs/router/managed-federation/setup

🔬 TESTING THINGS OUT?

  1. Download an example supergraph schema with Apollo-hosted subgraphs:

    $ curl -L https://supergraph.demo.starstuff.dev/ > starstuff.graphql

  2. Run the Apollo Router in development mode with the supergraph schema:

    $ ./router --dev --supergraph starstuff.graphql

    "#
                ));
            }
        };

        let router = RouterHttpServer::builder()
            .configuration(configuration)
            .schema(schema)
            .shutdown(shutdown.unwrap_or(ShutdownSource::CtrlC))
            .start();
        if let Err(err) = router.await {
            tracing::error!("{}", err);
            return Err(err.into());
        }
        Ok(())
    }
}

fn setup_panic_handler() {
    // Redirect panics to the logs.
    let backtrace_env = std::env::var("RUST_BACKTRACE");
    let show_backtraces =
        backtrace_env.as_deref() == Ok("1") || backtrace_env.as_deref() == Ok("full");
    if show_backtraces {
        tracing::warn!("RUST_BACKTRACE={} detected. This is useful for diagnostics but will have a performance impact and may leak sensitive information", backtrace_env.as_ref().unwrap());
    }
    std::panic::set_hook(Box::new(move |e| {
        if show_backtraces {
            let backtrace = std::backtrace::Backtrace::capture();
            tracing::error!("{}\n{:?}", e, backtrace)
        } else {
            tracing::error!("{}", e)
        }
        // Once we've panic'ed the behaviour of the router is non-deterministic
        // We've logged out the panic details. Terminate with an error code
        std::process::exit(1);
    }));
}

fn copy_args_to_env() {
    // Copy all the args to env.
    // This way, Clap is still responsible for the definitive view of what the current options are.
    // But if we have code that relies on env variable then it will still work.
    // Env variables should disappear over time as we move to plugins.
    let matches = Opt::command().get_matches();
    Opt::command().get_arguments().for_each(|a| {
        if let Some(env) = a.get_env() {
            if let Some(raw) = matches
                .get_raw(a.get_id().as_str())
                .unwrap_or_default()
                .next()
            {
                env::set_var(env, raw);
            }
        }
    });
}
