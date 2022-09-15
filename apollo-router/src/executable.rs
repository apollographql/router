//! Main entry point for CLI command to start server.

use std::env;
use std::ffi::OsStr;
use std::ffi::OsString;
use std::fmt;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::anyhow;
use anyhow::Context;
use anyhow::Result;
use clap::AppSettings;
use clap::CommandFactory;
use clap::Parser;
use directories::ProjectDirs;
use once_cell::sync::OnceCell;
use tracing::dispatcher::with_default;
use tracing::dispatcher::Dispatch;
use tracing::instrument::WithSubscriber;
use tracing_subscriber::EnvFilter;
use url::ParseError;
use url::Url;

use crate::configuration::generate_config_schema;
use crate::configuration::Configuration;
use crate::configuration::ConfigurationError;
use crate::router::ConfigurationSource;
use crate::router::RouterHttpServer;
use crate::router::SchemaSource;
use crate::router::ShutdownSource;

pub(crate) static GLOBAL_ENV_FILTER: OnceCell<String> = OnceCell::new();

/// Options for the router
#[derive(Parser, Debug)]
#[clap(
    name = "router",
    about = "Apollo federation router",
    global_setting(AppSettings::NoAutoVersion)
)]
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
    #[clap(alias = "hr", long = "hot-reload", env = "APOLLO_ROUTER_HOT_RELOAD")]
    hot_reload: bool,

    /// Configuration location relative to the project directory.
    #[clap(
        short,
        long = "config",
        parse(from_os_str),
        env = "APOLLO_ROUTER_CONFIG_PATH"
    )]
    config_path: Option<PathBuf>,

    /// Enable development mode.
    #[clap(env = "APOLLO_ROUTER_DEV", long = "dev", hide(true))]
    dev: bool,

    /// Schema location relative to the project directory.
    #[clap(
        short,
        long = "supergraph",
        parse(from_os_str),
        env = "APOLLO_ROUTER_SUPERGRAPH_PATH"
    )]
    supergraph_path: Option<PathBuf>,

    /// Prints the configuration schema.
    #[clap(long)]
    schema: bool,

    /// Your Apollo key.
    #[clap(skip = std::env::var("APOLLO_KEY").ok())]
    apollo_key: Option<String>,

    /// Your Apollo graph reference.
    #[clap(skip = std::env::var("APOLLO_GRAPH_REF").ok())]
    apollo_graph_ref: Option<String>,

    /// The endpoints (comma separated) polled to fetch the latest supergraph schema.
    #[clap(long, env, multiple_occurrences(true))]
    // Should be a Vec<Url> when https://github.com/clap-rs/clap/discussions/3796 is solved
    apollo_uplink_endpoints: Option<String>,

    /// The time between polls to Apollo uplink. Minimum 10s.
    #[clap(long, default_value = "10s", parse(try_from_str = humantime::parse_duration), env)]
    apollo_uplink_poll_interval: Duration,

    /// Display version and exit.
    #[clap(parse(from_flag), long, short = 'V')]
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
/// Refer to the examples if you would like to see how to run your own router with plugins.
pub fn main() -> Result<()> {
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

/// Entry point into creating a router executable.
#[non_exhaustive]
pub struct Executable {}

#[buildstructor::buildstructor]
impl Executable {
    /// Build an executable that will parse commandline options and set up logging,
    /// then start an HTTP server.
    ///
    /// You may optionally specify when the server should gracefully shut down, the schema source and the configuration source.
    /// The default is on CTRL+C on the terminal (or a `SIGINT` signal).
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
    /// Note that if you do not specify a runtime you must be in the context of an existing tokio runtime.
    ///
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

        if opt.schema {
            let schema = generate_config_schema();
            println!("{}", serde_json::to_string_pretty(&schema)?);
            return Ok(());
        }

        let builder = tracing_subscriber::fmt::fmt().with_env_filter(
            EnvFilter::try_new(&opt.log_level).context("could not parse log configuration")?,
        );

        let dispatcher = if atty::is(atty::Stream::Stdout) {
            Dispatch::new(
                builder
                    .with_target(!opt.log_level.eq_ignore_ascii_case("info"))
                    .finish(),
            )
        } else {
            Dispatch::new(builder.json().finish())
        };

        GLOBAL_ENV_FILTER.set(opt.log_level.clone()).expect(
            "failed setting the global env filter. THe start() function should only be called once",
        );

        // The dispatcher we created is passed explicitely here to make sure we display the logs
        // in the initialization pahse and in the state machine code, before a global subscriber
        // is set using the configuration file
        Self::inner_start(shutdown, schema, config, opt, dispatcher.clone())
            .with_subscriber(dispatcher)
            .await
    }

    async fn inner_start(
        shutdown: Option<ShutdownSource>,
        schema: Option<SchemaSource>,
        config: Option<ConfigurationSource>,
        mut opt: Opt,
        dispatcher: Dispatch,
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
                    dev: opt.dev,
                }
            }) {
                Some(configuration) => configuration,
                None => Configuration::builder()
                    .dev(opt.dev)
                    .build()
                    .map(std::convert::Into::into)?,
            },
        };

        let is_telemetry_disabled = std::env::var("APOLLO_TELEMETRY_DISABLED").ok().is_some();
        let apollo_telemetry_msg = if is_telemetry_disabled {
            "Anonymous usage data was disabled via APOLLO_TELEMETRY_DISABLED=1.".to_string()
        } else {
            "Anonymous usage data is gathered to inform Apollo product development.  See https://go.apollo.dev/o/privacy for more info.".to_string()
        };

        let apollo_router_msg = format!("Apollo Router v{} // (c) Apollo Graph, Inc. // Licensed as ELv2 (https://go.apollo.dev/elv2)\n{}", std::env!("CARGO_PKG_VERSION"), apollo_telemetry_msg);
        let schema = match (schema, opt.supergraph_path, opt.apollo_key) {
            (Some(_), Some(_), _) => {
                return Err(anyhow!(
                    "--supergraph and APOLLO_ROUTER_SUPERGRAPH_PATH cannot be used when a custom schema source is in use"
                ))
            }
            (Some(source), None, _) => source,
            (_, Some(supergraph_path), _) => {
                tracing::info!("{apollo_router_msg}");
                setup_panic_handler(dispatcher.clone());

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

fn setup_panic_handler(dispatcher: Dispatch) {
    // Redirect panics to the logs.
    let backtrace_env = std::env::var("RUST_BACKTRACE");
    let show_backtraces =
        backtrace_env.as_deref() == Ok("1") || backtrace_env.as_deref() == Ok("full");
    if show_backtraces {
        tracing::warn!("RUST_BACKTRACE={} detected. This use useful for diagnostics but will have a performance impact and may leak sensitive information", backtrace_env.as_ref().unwrap());
    }
    std::panic::set_hook(Box::new(move |e| {
        with_default(&dispatcher, || {
            if show_backtraces {
                let backtrace = backtrace::Backtrace::new();
                tracing::error!("{}\n{:?}", e, backtrace)
            } else {
                tracing::error!("{}", e)
            }
            // Once we've panic'ed the behaviour of the router is non-deterministic
            // We've logged out the panic details. Terminate with an error code
            std::process::exit(1);
        });
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
            if a.is_allow_invalid_utf8_set() {
                if let Some(value) = matches.get_one::<OsString>(a.get_id()) {
                    env::set_var(env, value);
                }
            } else if let Ok(Some(value)) = matches.try_get_one::<PathBuf>(a.get_id()) {
                env::set_var(env, value);
            } else if let Ok(Some(value)) = matches.try_get_one::<String>(a.get_id()) {
                env::set_var(env, value);
            }
        }
    });
}
