//! Main entry point for CLI command to start server.

use std::cell::Cell;
use std::env;
use std::ffi::OsStr;
use std::fmt;
use std::fmt::Debug;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use std::time::Duration;

use anyhow::anyhow;
use anyhow::Result;
use clap::builder::FalseyValueParser;
use clap::ArgAction;
use clap::Args;
use clap::CommandFactory;
use clap::Parser;
use clap::Subcommand;
use directories::ProjectDirs;
#[cfg(any(feature = "dhat-heap", feature = "dhat-ad-hoc"))]
use once_cell::sync::OnceCell;
use regex::Captures;
use regex::Regex;
use url::ParseError;
use url::Url;

use crate::configuration::generate_config_schema;
use crate::configuration::generate_upgrade;
use crate::configuration::Discussed;
use crate::metrics::meter_provider;
use crate::plugin::plugins;
use crate::plugins::telemetry::reload::init_telemetry;
use crate::router::ConfigurationSource;
use crate::router::RouterHttpServer;
use crate::router::SchemaSource;
use crate::router::ShutdownSource;
use crate::uplink::Endpoints;
use crate::uplink::UplinkConfig;
use crate::LicenseSource;

#[cfg(all(
    feature = "global-allocator",
    not(feature = "dhat-heap"),
    target_os = "linux"
))]
#[global_allocator]
static ALLOC: tikv_jemallocator::Jemalloc = tikv_jemallocator::Jemalloc;

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
    /// List all the available preview configurations with related GitHub discussion
    Preview,
}

/// Options for the router
#[derive(Parser, Debug)]
#[clap(name = "router", about = "Apollo federation router")]
#[command(disable_version_flag(true))]
pub struct Opt {
    /// Log level (off|error|warn|info|debug|trace).
    #[clap(
        long = "log",
        default_value = "info",
        alias = "log-level",
        value_parser = add_log_filter,
        env = "APOLLO_ROUTER_LOG"
    )]
    // FIXME: when upgrading to router 2.0 we should put this value in an Option
    log_level: String,

    /// Reload locally provided configuration and supergraph files automatically.  This only affects watching of local files and does not affect supergraphs and configuration provided by GraphOS through Uplink, which is always reloaded immediately.
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

    /// Locations (comma separated) to fetch the supergraph from. These will be queried in order.
    #[clap(env = "APOLLO_ROUTER_SUPERGRAPH_URLS", value_delimiter = ',')]
    supergraph_urls: Option<Vec<Url>>,

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

    /// Your Apollo Router license.
    #[clap(skip = std::env::var("APOLLO_ROUTER_LICENSE").ok())]
    apollo_router_license: Option<String>,

    /// License location relative to the current directory.
    #[clap(long = "license", env = "APOLLO_ROUTER_LICENSE_PATH")]
    apollo_router_license_path: Option<PathBuf>,

    /// The endpoints (comma separated) polled to fetch the latest supergraph schema.
    #[clap(long, env, action = ArgAction::Append)]
    // Should be a Vec<Url> when https://github.com/clap-rs/clap/discussions/3796 is solved
    apollo_uplink_endpoints: Option<String>,

    /// The time between polls to Apollo uplink. Minimum 10s.
    #[clap(long, default_value = "10s", value_parser = humantime::parse_duration, env)]
    apollo_uplink_poll_interval: Duration,

    /// Disable sending anonymous usage information to Apollo.
    #[clap(long, env = "APOLLO_TELEMETRY_DISABLED", value_parser = FalseyValueParser::new())]
    anonymous_telemetry_disabled: bool,

    /// The timeout for an http call to Apollo uplink. Defaults to 30s.
    #[clap(long, default_value = "30s", value_parser = humantime::parse_duration, env)]
    apollo_uplink_timeout: Duration,

    /// The listen address for the router. Overrides `supergraph.listen` in router.yaml.
    #[clap(long = "listen", env = "APOLLO_ROUTER_LISTEN_ADDRESS")]
    listen_address: Option<SocketAddr>,

    /// Display version and exit.
    #[clap(action = ArgAction::SetTrue, long, short = 'V')]
    pub(crate) version: bool,
}

// Add a filter to global log level settings so that the level only applies to the router.
//
// If you want to set a complex logging filter which isn't modified in this way, use RUST_LOG.
fn add_log_filter(raw: &str) -> Result<String, String> {
    match std::env::var("RUST_LOG") {
        Ok(filter) => Ok(filter),
        Err(_e) => {
            // Directives are case-insensitive. Convert to lowercase before processing.
            let lowered = raw.to_lowercase();
            // Find "global" directives and limit them to apollo_router
            let rgx =
                Regex::new(r"(^|,)(off|error|warn|info|debug|trace)").expect("regex must be valid");
            let res = rgx.replace_all(&lowered, |caps: &Captures| {
                // The default level is info, then other ones can override the default one
                // If the pattern matches, we must have caps 1 and 2
                format!("{}apollo_router={}", &caps[1], &caps[2])
            });
            Ok(format!("info,{res}"))
        }
    }
}

impl Opt {
    pub(crate) fn uplink_config(&self) -> Result<UplinkConfig, anyhow::Error> {
        Ok(UplinkConfig {
            apollo_key: self
                .apollo_key
                .clone()
                .ok_or(Self::err_require_opt("APOLLO_KEY"))?,
            apollo_graph_ref: self
                .apollo_graph_ref
                .clone()
                .ok_or(Self::err_require_opt("APOLLO_GRAPH_REF"))?,
            endpoints: self
                .apollo_uplink_endpoints
                .as_ref()
                .map(|endpoints| Self::parse_endpoints(endpoints))
                .transpose()?,
            poll_interval: self.apollo_uplink_poll_interval,
            timeout: self.apollo_uplink_timeout,
        })
    }

    pub(crate) fn is_telemetry_disabled(&self) -> bool {
        self.anonymous_telemetry_disabled
    }

    fn parse_endpoints(endpoints: &str) -> std::result::Result<Endpoints, anyhow::Error> {
        Ok(Endpoints::fallback(
            endpoints
                .split(',')
                .map(|endpoint| Url::parse(endpoint.trim()))
                .collect::<Result<Vec<Url>, ParseError>>()
                .map_err(|err| anyhow!("invalid Apollo Uplink endpoint, {}", err))?,
        ))
    }

    fn err_require_opt(env_var: &str) -> anyhow::Error {
        anyhow!("Use of Apollo Graph OS requires setting the {env_var} environment variable")
    }
}

/// Wrapper so that clap can display the default config path in the help message.
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
        license: Option<LicenseSource>,
        config: Option<ConfigurationSource>,
        cli_args: Option<Opt>,
    ) -> Result<()> {
        let opt = cli_args.unwrap_or_else(Opt::parse);

        if opt.version {
            println!("{}", std::env!("CARGO_PKG_VERSION"));
            return Ok(());
        }

        copy_args_to_env();

        let apollo_telemetry_initialized = if graph_os() {
            init_telemetry(&opt.log_level)?;
            true
        } else {
            // Best effort init telemetry
            init_telemetry(&opt.log_level).is_ok()
        };

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
                Discussed::new().print_experimental();
                Ok(())
            }
            Some(Commands::Config(ConfigSubcommandArgs {
                command: ConfigSubcommand::Preview,
            })) => {
                Discussed::new().print_preview();
                Ok(())
            }
            None => Self::inner_start(shutdown, schema, config, license, opt).await,
        };

        if apollo_telemetry_initialized {
            // We should be good to shutdown OpenTelemetry now as the router should have finished everything.
            tokio::task::spawn_blocking(move || {
                opentelemetry::global::shutdown_tracer_provider();
                meter_provider().shutdown();
            })
            .await?;
        }
        result
    }

    async fn inner_start(
        shutdown: Option<ShutdownSource>,
        schema: Option<SchemaSource>,
        config: Option<ConfigurationSource>,
        license: Option<LicenseSource>,
        mut opt: Opt,
    ) -> Result<()> {
        if opt.apollo_uplink_poll_interval < Duration::from_secs(10) {
            return Err(anyhow!("apollo-uplink-poll-interval must be at least 10s"));
        }
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
            #[allow(clippy::blocks_in_conditions)]
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

        // Schema source will be in order of precedence:
        // 1. Cli --supergraph
        // 2. Env APOLLO_ROUTER_SUPERGRAPH_PATH
        // 3. Env APOLLO_ROUTER_SUPERGRAPH_URLS
        // 4. Env APOLLO_KEY and APOLLO_GRAPH_REF
        let schema_source = match (schema, &opt.supergraph_path, &opt.supergraph_urls, &opt.apollo_key) {
            (Some(_), Some(_), _, _) | (Some(_), _, Some(_), _) => {
                return Err(anyhow!(
                    "--supergraph and APOLLO_ROUTER_SUPERGRAPH_PATH cannot be used when a custom schema source is in use"
                ))
            }
            (Some(source), None, None,_) => source,
            (_, Some(supergraph_path), _, _) => {
                tracing::info!("{apollo_router_msg}");
                tracing::info!("{apollo_telemetry_msg}");

                let supergraph_path = if supergraph_path.is_relative() {
                    current_directory.join(supergraph_path)
                } else {
                    supergraph_path.clone()
                };
                SchemaSource::File {
                    path: supergraph_path,
                    watch: opt.hot_reload,
                    delay: None,
                }
            }
            (_, _, Some(supergraph_urls), _) => {
                tracing::info!("{apollo_router_msg}");
                tracing::info!("{apollo_telemetry_msg}");

                SchemaSource::URLs {
                    urls: supergraph_urls.clone(),
                    watch: opt.hot_reload,
                    period: opt.apollo_uplink_poll_interval
                }
            }
            (_, None, None, Some(_apollo_key)) => {
                tracing::info!("{apollo_router_msg}");
                tracing::info!("{apollo_telemetry_msg}");
                SchemaSource::Registry(opt.uplink_config()?)
            }
            _ => {
                return Err(anyhow!(
                    r#"{apollo_router_msg}

⚠️  The Apollo Router requires a composed supergraph schema at startup. ⚠️

👉 DO ONE:

  * Pass a local schema file with the '--supergraph' option:

      $ ./router --supergraph <file_path>

  * Fetch a registered schema from GraphOS by setting
    these environment variables:

      $ APOLLO_KEY="..." APOLLO_GRAPH_REF="..." ./router

      For details, see the Apollo docs:
      https://www.apollographql.com/docs/federation/managed-federation/setup

🔬 TESTING THINGS OUT?

  1. Download an example supergraph schema with Apollo-hosted subgraphs:

    $ curl -L https://supergraph.demo.starstuff.dev/ > starstuff.graphql

  2. Run the Apollo Router in development mode with the supergraph schema:

    $ ./router --dev --supergraph starstuff.graphql

    "#
                ));
            }
        };

        // Order of precedence:
        // 1. explicit path from cli
        // 2. env APOLLO_ROUTER_LICENSE
        // 3. uplink

        let license = if let Some(license) = license {
            license
        } else {
            match (
                &opt.apollo_router_license,
                &opt.apollo_router_license_path,
                &opt.apollo_key,
                &opt.apollo_graph_ref,
            ) {
                (_, Some(license_path), _, _) => {
                    let license_path = if license_path.is_relative() {
                        current_directory.join(license_path)
                    } else {
                        license_path.clone()
                    };
                    LicenseSource::File {
                        path: license_path,
                        watch: opt.hot_reload,
                    }
                }
                (Some(_license), _, _, _) => LicenseSource::Env,
                (_, _, Some(_apollo_key), Some(_apollo_graph_ref)) => {
                    LicenseSource::Registry(opt.uplink_config()?)
                }

                _ => LicenseSource::default(),
            }
        };

        // If there are custom plugins then if RUST_LOG hasn't been set and APOLLO_ROUTER_LOG contains one of the defaults.
        let user_plugins_present = plugins().filter(|p| !p.is_apollo()).count() > 0;
        let rust_log_set = std::env::var("RUST_LOG").is_ok();
        let apollo_router_log = std::env::var("APOLLO_ROUTER_LOG").unwrap_or_default();
        if user_plugins_present
            && !rust_log_set
            && ["trace", "debug", "warn", "error", "info"].contains(&apollo_router_log.as_str())
        {
            tracing::info!("Custom plugins are present. To see log messages from your plugins you must configure `RUST_LOG` or `APOLLO_ROUTER_LOG` environment variables. See the Router logging documentation for more details");
        }

        let uplink_config = opt.uplink_config().ok();
        if uplink_config
            .clone()
            .unwrap_or_default()
            .endpoints
            .unwrap_or_default()
            .url_count()
            == 1
        {
            tracing::warn!("Only a single uplink endpoint is configured. We recommend specifying at least two endpoints so that a fallback exists.");
        }

        let router = RouterHttpServer::builder()
            .is_telemetry_disabled(opt.is_telemetry_disabled())
            .configuration(configuration)
            .and_uplink(uplink_config)
            .schema(schema_source)
            .license(license)
            .shutdown(shutdown.unwrap_or(ShutdownSource::CtrlC))
            .start();

        if let Err(err) = router.await {
            tracing::error!("{}", err);
            return Err(err.into());
        }
        Ok(())
    }
}

fn graph_os() -> bool {
    std::env::var("APOLLO_KEY").is_ok() && std::env::var("APOLLO_GRAPH_REF").is_ok()
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
            tracing::error!("{}\n{}", e, backtrace)
        } else {
            tracing::error!("{}", e)
        }
        if !USING_CATCH_UNWIND.get() {
            // Once we've panic'ed the behaviour of the router is non-deterministic
            // We've logged out the panic details. Terminate with an error code
            std::process::exit(1);
        }
    }));
}

// TODO: once the Rust query planner does not use `todo!()` anymore,
// remove this and the use of `catch_unwind` to call it.
thread_local! {
    pub(crate) static USING_CATCH_UNWIND: Cell<bool> = const { Cell::new(false) };
}

static COPIED: AtomicBool = AtomicBool::new(false);

fn copy_args_to_env() {
    if Ok(false) != COPIED.compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed) {
        panic!("`copy_args_to_env` was called twice: That means `Executable::start` was called twice in the same process, which should not happen");
    }
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

#[cfg(test)]
mod tests {
    use crate::executable::add_log_filter;

    #[test]
    fn simplest_logging_modifications() {
        for level in ["off", "error", "warn", "info", "debug", "trace"] {
            assert_eq!(
                add_log_filter(level).expect("conversion works"),
                format!("info,apollo_router={level}")
            );
        }
    }

    // It's hard to have comprehensive tests for this kind of functionality,
    // so this set is derived from the examples at:
    // https://docs.rs/env_logger/latest/env_logger/#filtering-results
    // which is a reasonably corpus of things to test.
    #[test]
    fn complex_logging_modifications() {
        assert_eq!(add_log_filter("hello").unwrap(), "info,hello");
        assert_eq!(add_log_filter("trace").unwrap(), "info,apollo_router=trace");
        assert_eq!(add_log_filter("TRACE").unwrap(), "info,apollo_router=trace");
        assert_eq!(add_log_filter("info").unwrap(), "info,apollo_router=info");
        assert_eq!(add_log_filter("INFO").unwrap(), "info,apollo_router=info");
        assert_eq!(add_log_filter("hello=debug").unwrap(), "info,hello=debug");
        assert_eq!(add_log_filter("hello=DEBUG").unwrap(), "info,hello=debug");
        assert_eq!(
            add_log_filter("hello,std::option").unwrap(),
            "info,hello,std::option"
        );
        assert_eq!(
            add_log_filter("error,hello=warn").unwrap(),
            "info,apollo_router=error,hello=warn"
        );
        assert_eq!(
            add_log_filter("error,hello=off").unwrap(),
            "info,apollo_router=error,hello=off"
        );
        assert_eq!(add_log_filter("off").unwrap(), "info,apollo_router=off");
        assert_eq!(add_log_filter("OFF").unwrap(), "info,apollo_router=off");
        assert_eq!(add_log_filter("hello/foo").unwrap(), "info,hello/foo");
        assert_eq!(add_log_filter("hello/f.o").unwrap(), "info,hello/f.o");
        assert_eq!(
            add_log_filter("hello=debug/foo*foo").unwrap(),
            "info,hello=debug/foo*foo"
        );
        assert_eq!(
            add_log_filter("error,hello=warn/[0-9]scopes").unwrap(),
            "info,apollo_router=error,hello=warn/[0-9]scopes"
        );
        // Add some hard ones
        assert_eq!(
            add_log_filter("hyper=debug,warn,regex=warn,h2=off").unwrap(),
            "info,hyper=debug,apollo_router=warn,regex=warn,h2=off"
        );
        assert_eq!(
            add_log_filter("hyper=debug,apollo_router=off,regex=info,h2=off").unwrap(),
            "info,hyper=debug,apollo_router=off,regex=info,h2=off"
        );
        assert_eq!(
            add_log_filter("apollo_router::plugins=debug").unwrap(),
            "info,apollo_router::plugins=debug"
        );
    }
}
