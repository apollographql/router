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
use tracing_subscriber::EnvFilter;
use url::ParseError;
use url::Url;

use crate::configuration::generate_config_schema;
use crate::configuration::Configuration;
use crate::configuration::ConfigurationError;
use crate::router::ApolloRouter;
use crate::router::ConfigurationKind;
use crate::router::SchemaKind;
use crate::router::ShutdownKind;
use crate::subscriber::set_global_subscriber;
use crate::subscriber::RouterSubscriber;

static GLOBAL_ENV_FILTER: OnceCell<String> = OnceCell::new();

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
/// Refer to the examples if you would like how to run your own router with plugins.
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
pub struct Executable {}

#[buildstructor::buildstructor]
impl Executable {
    /// Build an executable that will parse commandline options and set up logging.
    /// You may optionally supply a `router_builder_fn` to override building of the router.
    ///
    /// ```no_run
    /// use apollo_router::{ApolloRouter, Executable, ShutdownKind};
    /// # use anyhow::Result;
    /// # #[tokio::main]
    /// # async fn main()->Result<()> {
    /// Executable::builder()
    ///   .router_builder_fn(|configuration, schema| ApolloRouter::builder()
    ///                 .configuration(configuration)
    ///                 .schema(schema)
    ///                 .shutdown(ShutdownKind::CtrlC)
    ///                 .build())
    ///   .start().await
    /// # }
    /// ```
    /// Note that if you do not specify a runtime you must be in the context of an existing tokio runtime.
    ///
    #[builder(entry = "builder", exit = "start")]
    pub async fn start(
        router_builder_fn: Option<fn(ConfigurationKind, SchemaKind) -> ApolloRouter>,
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

        // This is more complex than I'd like it to be. Really, we just want to pass
        // a FmtSubscriber to set_global_subscriber(), but we can't because of the
        // generic nature of FmtSubscriber. See: https://github.com/tokio-rs/tracing/issues/380
        // for more details.
        let builder = tracing_subscriber::fmt::fmt().with_env_filter(
            EnvFilter::try_new(&opt.log_level).context("could not parse log configuration")?,
        );

        let subscriber: RouterSubscriber = if atty::is(atty::Stream::Stdout) {
            RouterSubscriber::TextSubscriber(builder.finish())
        } else {
            RouterSubscriber::JsonSubscriber(builder.json().finish())
        };

        set_global_subscriber(subscriber)?;

        GLOBAL_ENV_FILTER.set(opt.log_level).unwrap();

        let current_directory = std::env::current_dir()?;

        let configuration = opt
            .config_path
            .as_ref()
            .map(|path| {
                let path = if path.is_relative() {
                    current_directory.join(path)
                } else {
                    path.to_path_buf()
                };

                ConfigurationKind::File {
                    path,
                    watch: opt.hot_reload,
                    delay: None,
                }
            })
            .unwrap_or_else(|| Configuration::builder().build().into());
        let apollo_router_msg = format!("Apollo Router v{} // (c) Apollo Graph, Inc. // Licensed as ELv2 (https://go.apollo.dev/elv2)", std::env!("CARGO_PKG_VERSION"));
        let schema = match (opt.supergraph_path, opt.apollo_key) {
            (Some(supergraph_path), _) => {
                tracing::info!("{apollo_router_msg}");
                setup_panic_handler();

                let supergraph_path = if supergraph_path.is_relative() {
                    current_directory.join(supergraph_path)
                } else {
                    supergraph_path
                };
                SchemaKind::File {
                    path: supergraph_path,
                    watch: opt.hot_reload,
                    delay: None,
                }
            }
            (None, Some(apollo_key)) => {
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

                SchemaKind::Registry {
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

  2. Run the Apollo Router with the supergraph schema:

    $ ./router --supergraph starstuff.graphql

    "#
                ));
            }
        };

        let router = router_builder_fn.unwrap_or(|configuration, schema| {
            ApolloRouter::builder()
                .configuration(configuration)
                .schema(schema)
                .shutdown(ShutdownKind::CtrlC)
                .build()
        })(configuration, schema);
        if let Err(err) = router.serve().await {
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
        tracing::warn!("RUST_BACKTRACE={} detected. This use useful for diagnostics but will have a performance impact and may leak sensitive information", backtrace_env.as_ref().unwrap());
    }
    std::panic::set_hook(Box::new(move |e| {
        if show_backtraces {
            let backtrace = backtrace::Backtrace::new();
            tracing::error!("{}\n{:?}", e, backtrace)
        } else {
            tracing::error!("{}", e)
        }
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
