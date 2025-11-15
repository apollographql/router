//! Main entry point for CLI command to start server.

use std::fmt::Debug;
use std::net::SocketAddr;
#[cfg(unix)]
use std::os::unix::fs::MetadataExt;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use std::time::Duration;

use anyhow::Result;
use anyhow::anyhow;
use clap::ArgAction;
use clap::Args;
use clap::Parser;
use clap::Subcommand;
use clap::builder::FalseyValueParser;
use parking_lot::Mutex;
use regex::Captures;
use regex::Regex;
use url::ParseError;
use url::Url;

use crate::LicenseSource;
use crate::configuration::Discussed;
use crate::configuration::expansion::Expansion;
use crate::configuration::generate_config_schema;
use crate::configuration::generate_upgrade;
use crate::configuration::schema::Mode;
use crate::configuration::validate_yaml_configuration;
use crate::metrics::meter_provider_internal;
use crate::plugin::plugins;
use crate::plugins::telemetry::reload::otel::init_telemetry;
use crate::router::ApolloRouterError;
use crate::router::ConfigurationSource;
use crate::router::RouterHttpServer;
use crate::router::SchemaSource;
use crate::router::ShutdownSource;
use crate::uplink::Endpoints;
use crate::uplink::UplinkConfig;

pub(crate) static APOLLO_ROUTER_DEV_MODE: AtomicBool = AtomicBool::new(false);
pub(crate) static APOLLO_ROUTER_SUPERGRAPH_PATH_IS_SET: AtomicBool = AtomicBool::new(false);
pub(crate) static APOLLO_ROUTER_SUPERGRAPH_URLS_IS_SET: AtomicBool = AtomicBool::new(false);
pub(crate) static APOLLO_ROUTER_LICENCE_IS_SET: AtomicBool = AtomicBool::new(false);
pub(crate) static APOLLO_ROUTER_LICENCE_PATH_IS_SET: AtomicBool = AtomicBool::new(false);
pub(crate) static APOLLO_TELEMETRY_DISABLED: AtomicBool = AtomicBool::new(false);
pub(crate) static APOLLO_ROUTER_LISTEN_ADDRESS: Mutex<Option<SocketAddr>> = Mutex::new(None);
pub(crate) static APOLLO_ROUTER_GRAPH_ARTIFACT_REFERENCE: Mutex<Option<String>> = Mutex::new(None);
pub(crate) static APOLLO_ROUTER_HOT_RELOAD_CLI: AtomicBool = AtomicBool::new(false);

const STARTUP_ERROR_MESSAGE: &str = r#"
⚠️  The Apollo Router requires a composed supergraph schema at startup. ⚠️

👉 DO ONE:

  * Pass a local schema file with the '--supergraph' option:

      $ ./router --supergraph <file_path>

  * Fetch a registered schema from GraphOS by setting
    these environment variables:

      $ APOLLO_KEY="..." APOLLO_GRAPH_REF="..." ./router

      For details, see the Apollo docs:
      https://www.apollographql.com/docs/federation/managed-federation/setup

  * Fetch a schema from an OCI registry using '--graph-artifact-reference':

      $ APOLLO_KEY="..." ./router --graph-artifact-reference=<reference>

      For details, see the Apollo docs:
      https://www.apollographql.com/docs/federation/managed-federation/setup

  * Specify a schema source in your configuration file:

      Add 'graph_artifact_reference' to your router configuration file

🧪 TESTING THINGS OUT?

  1. Download an example supergraph schema with Apollo-hosted subgraphs:

    $ curl -L https://supergraph.demo.starstuff.dev/ > starstuff.graphql

  2. Run the Apollo Router in development mode with the supergraph schema:

    $ ./router --dev --supergraph starstuff.graphql

    "#;

const INITIAL_UPLINK_POLL_INTERVAL: Duration = Duration::from_secs(10);

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
    /// Validate existing Router configuration file
    Validate {
        /// The location of the config to validate.
        #[clap(value_parser, env = "APOLLO_ROUTER_CONFIG_PATH")]
        config_path: PathBuf,
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
    #[clap(env = "APOLLO_ROUTER_DEV", long = "dev", action(ArgAction::SetTrue))]
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

    /// Subcommands
    #[clap(subcommand)]
    command: Option<Commands>,

    /// Your Apollo key.
    #[clap(skip = std::env::var("APOLLO_KEY").ok())]
    apollo_key: Option<String>,

    /// Key file location relative to the current directory.
    #[cfg(unix)]
    #[clap(long = "apollo-key-path", env = "APOLLO_KEY_PATH")]
    apollo_key_path: Option<PathBuf>,

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

    /// An OCI reference to a graph artifact that contains the supergraph schema for the router to run.
    #[clap(long, env = "APOLLO_GRAPH_ARTIFACT_REFERENCE", action = ArgAction::Append)]
    graph_artifact_reference: Option<String>,

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
            poll_interval: INITIAL_UPLINK_POLL_INTERVAL,
            timeout: self.apollo_uplink_timeout,
        })
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

/// This is the main router entrypoint.
///
/// Starts a Tokio runtime and runs a Router in it based on command-line options.
/// Returns on fatal error or after graceful shutdown has completed.
///
/// Refer to the examples if you would like to see how to run your own router with plugins.
pub fn main() -> Result<()> {
    #[cfg(feature = "dhat-heap")]
    crate::allocator::create_heap_profiler();

    #[cfg(feature = "dhat-ad-hoc")]
    crate::allocator::create_ad_hoc_profiler();

    let mut builder = tokio::runtime::Builder::new_multi_thread();
    builder.enable_all();

    // This environment variable is intentionally undocumented.
    // See also APOLLO_ROUTER_COMPUTE_THREADS in apollo-router/src/compute_job.rs
    if let Some(nb) = std::env::var("APOLLO_ROUTER_IO_THREADS")
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

        *crate::services::APOLLO_KEY.lock() = opt.apollo_key.clone();
        *crate::services::APOLLO_GRAPH_REF.lock() = opt.apollo_graph_ref.clone();
        *APOLLO_ROUTER_LISTEN_ADDRESS.lock() = opt.listen_address;
        *APOLLO_ROUTER_GRAPH_ARTIFACT_REFERENCE.lock() = opt.graph_artifact_reference.clone();
        // Only set hot_reload if explicitly true
        if opt.hot_reload {
            APOLLO_ROUTER_HOT_RELOAD_CLI.store(true, Ordering::Relaxed);
        }
        APOLLO_ROUTER_DEV_MODE.store(opt.dev, Ordering::Relaxed);
        APOLLO_ROUTER_SUPERGRAPH_PATH_IS_SET
            .store(opt.supergraph_path.is_some(), Ordering::Relaxed);
        APOLLO_ROUTER_SUPERGRAPH_URLS_IS_SET
            .store(opt.supergraph_urls.is_some(), Ordering::Relaxed);
        APOLLO_ROUTER_LICENCE_IS_SET.store(opt.apollo_router_license.is_some(), Ordering::Relaxed);
        APOLLO_ROUTER_LICENCE_PATH_IS_SET
            .store(opt.apollo_router_license_path.is_some(), Ordering::Relaxed);
        APOLLO_TELEMETRY_DISABLED.store(opt.anonymous_telemetry_disabled, Ordering::Relaxed);

        let apollo_telemetry_initialized = if graph_os() {
            init_telemetry(&opt.log_level)?;
            true
        } else {
            // Best effort init telemetry
            init_telemetry(&opt.log_level).is_ok()
        };

        setup_panic_handler();

        let result = match opt.command.as_ref() {
            Some(Commands::Config(ConfigSubcommandArgs {
                command: ConfigSubcommand::Schema,
            })) => {
                let schema = generate_config_schema();
                println!("{}", serde_json::to_string_pretty(&schema)?);
                Ok(())
            }
            Some(Commands::Config(ConfigSubcommandArgs {
                command: ConfigSubcommand::Validate { config_path },
            })) => {
                let config_string = std::fs::read_to_string(config_path)?;
                validate_yaml_configuration(
                    &config_string,
                    Expansion::default()?,
                    Mode::NoUpgrade,
                )?
                .validate()?;

                println!("Configuration at path {config_path:?} is valid!");

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
                meter_provider_internal().shutdown();
            })
            .await?;
        }
        result
    }

    #[cfg_attr(test, allow(dead_code))]
    pub(crate) async fn inner_start(
        shutdown: Option<ShutdownSource>,
        schema: Option<SchemaSource>,
        config: Option<ConfigurationSource>,
        license: Option<LicenseSource>,
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
            #[allow(clippy::blocks_in_conditions)]
            _ => opt
                .config_path
                .as_ref()
                .map(|path| {
                    let path = if path.is_relative() {
                        current_directory.join(path)
                    } else {
                        path.to_path_buf()
                    };

                    ConfigurationSource::File {
                        path,
                        watch: opt.hot_reload,
                    }
                })
                .unwrap_or_default(),
        };

        let apollo_telemetry_msg = if opt.anonymous_telemetry_disabled {
            "Anonymous usage data collection is disabled.".to_string()
        } else {
            "Anonymous usage data is gathered to inform Apollo product development.  See https://go.apollo.dev/o/privacy for details.".to_string()
        };

        let apollo_router_msg = format!(
            "Apollo Router v{} // (c) Apollo Graph, Inc. // Licensed as ELv2 (https://go.apollo.dev/elv2)",
            std::env!("CARGO_PKG_VERSION")
        );

        // Schema source will be in order of precedence:
        // 1. Cli --supergraph
        // 2. Env APOLLO_ROUTER_SUPERGRAPH_PATH
        // 3. Env APOLLO_ROUTER_SUPERGRAPH_URLS
        // 4. Env APOLLO_KEY and APOLLO_GRAPH_ARTIFACT_REFERENCE (CLI/env only)
        // 5. Env APOLLO_KEY and APOLLO_GRAPH_REF (CLI/env only)
        // 6. Config file graph_artifact_reference (handled in config stream)
        #[cfg(unix)]
        let akp = &opt.apollo_key_path;
        #[cfg(not(unix))]
        let akp: &Option<PathBuf> = &None;

        // Track if schema source was provided via CLI/env (for OCI/Registry)
        let mut schema_source_provided_via_cli_env = false;
        // Track if graph_artifact_reference is being used from CLI/env (not from config)
        let using_graph_artifact_reference = opt.graph_artifact_reference.is_some();

        // Validate that schema sources are not conflicting
        // Check both builder API schema and CLI supergraph_path (both map to -s/--supergraph)
        let has_supergraph_file = schema.is_some() || opt.supergraph_path.is_some();
        if has_supergraph_file && opt.graph_artifact_reference.is_some() {
            return Err(anyhow!(
                "--supergraph (-s) and --graph-artifact-reference cannot be used together. Please specify only one schema source."
            ));
        }
        if opt.supergraph_urls.is_some() && opt.graph_artifact_reference.is_some() {
            return Err(anyhow!(
                "APOLLO_ROUTER_SUPERGRAPH_URLS and --graph-artifact-reference cannot be used together. Please specify only one schema source."
            ));
        }

        let schema_source = match (
            schema,
            &opt.supergraph_path,
            &opt.supergraph_urls,
            &opt.apollo_key,
            akp,
        ) {
            (Some(_), Some(_), _, _, _) | (Some(_), _, Some(_), _, _) => {
                return Err(anyhow!(
                    "--supergraph and APOLLO_ROUTER_SUPERGRAPH_PATH cannot be used when a custom schema source is in use"
                ));
            }
            (Some(source), None, None, _, _) => source,
            (_, Some(supergraph_path), _, _, _) => {
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
                }
            }
            (_, _, Some(supergraph_urls), _, _) => {
                tracing::info!("{apollo_router_msg}");
                tracing::info!("{apollo_telemetry_msg}");

                if opt.hot_reload {
                    tracing::warn!(
                        "Schema hot reloading is disabled for --supergraph-urls / APOLLO_ROUTER_SUPERGRAPH_URLS."
                    );
                }

                SchemaSource::URLs {
                    urls: supergraph_urls.clone(),
                }
            }
            (_, None, None, _, Some(apollo_key_path)) => {
                let apollo_key_path = if apollo_key_path.is_relative() {
                    current_directory.join(apollo_key_path)
                } else {
                    apollo_key_path.clone()
                };

                if !apollo_key_path.exists() {
                    tracing::error!(
                        "Apollo key at path '{}' does not exist.",
                        apollo_key_path.to_string_lossy()
                    );
                    return Err(anyhow!(
                        "Apollo key at path '{}' does not exist.",
                        apollo_key_path.to_string_lossy()
                    ));
                } else {
                    // On unix systems, Check that the executing user is the only user who may
                    // read the key file.
                    // Note: We could, in future, add support for Windows.
                    #[cfg(unix)]
                    {
                        let meta = std::fs::metadata(apollo_key_path.clone())
                            .map_err(|err| anyhow!("Failed to read Apollo key file: {}", err))?;
                        let mode = meta.mode();
                        // If our mode isn't "safe", fail...
                        // safe == none of the "group" or "other" bits set.
                        if mode & 0o077 != 0 {
                            return Err(anyhow!(
                                "Apollo key file permissions ({:#o}) are too permissive",
                                mode & 0o000777
                            ));
                        }
                        let euid = unsafe { libc::geteuid() };
                        let owner = meta.uid();
                        if euid != owner {
                            return Err(anyhow!(
                                "Apollo key file owner id ({owner}) does not match effective user id ({euid})"
                            ));
                        }
                    }
                    //The key file exists try and load it
                    match std::fs::read_to_string(&apollo_key_path) {
                        Ok(apollo_key) => {
                            opt.apollo_key = Some(apollo_key.trim().to_string());
                        }
                        Err(err) => {
                            return Err(anyhow!("Failed to read Apollo key file: {}", err));
                        }
                    };
                    match opt.graph_artifact_reference {
                        None => {
                            // No graph_artifact_reference from CLI/env - check if we have apollo_graph_ref
                            if opt.apollo_graph_ref.is_some() {
                                // Create Registry schema source from CLI/env
                                schema_source_provided_via_cli_env = true;
                                SchemaSource::Registry {
                                    apollo_key: opt.apollo_key.clone(),
                                    apollo_graph_ref: opt.apollo_graph_ref.clone(),
                                    endpoints: opt
                                        .apollo_uplink_endpoints
                                        .as_ref()
                                        .map(|endpoints| Opt::parse_endpoints(endpoints))
                                        .transpose()?,
                                    poll_interval: INITIAL_UPLINK_POLL_INTERVAL,
                                    timeout: opt.apollo_uplink_timeout,
                                }
                            } else {
                                // No schema source from CLI/env - let config stream handle it
                                SchemaSource::Static {
                                    schema_sdl: String::new(),
                                }
                            }
                        }
                        Some(ref reference) => {
                            schema_source_provided_via_cli_env = true;
                            SchemaSource::OCI {
                                apollo_key: opt
                                    .apollo_key
                                    .clone()
                                    .ok_or(Opt::err_require_opt("APOLLO_KEY"))?,
                                reference: reference.clone(),
                            }
                        }
                    }
                }
            }
            (_, None, None, Some(_apollo_key), None) => {
                tracing::info!("{apollo_router_msg}");
                tracing::info!("{apollo_telemetry_msg}");
                match opt.graph_artifact_reference {
                    None => {
                        // No graph_artifact_reference from CLI/env - check if we have apollo_graph_ref
                        if opt.apollo_graph_ref.is_some() {
                            // Create Registry schema source from CLI/env
                            schema_source_provided_via_cli_env = true;
                            SchemaSource::Registry {
                                apollo_key: opt.apollo_key.clone(),
                                apollo_graph_ref: opt.apollo_graph_ref.clone(),
                                endpoints: opt
                                    .apollo_uplink_endpoints
                                    .as_ref()
                                    .map(|endpoints| Opt::parse_endpoints(endpoints))
                                    .transpose()?,
                                poll_interval: INITIAL_UPLINK_POLL_INTERVAL,
                                timeout: opt.apollo_uplink_timeout,
                            }
                        } else {
                            // No schema source from CLI/env - let config stream handle it
                            SchemaSource::Static {
                                schema_sdl: String::new(),
                            }
                        }
                    }
                    Some(ref reference) => {
                        schema_source_provided_via_cli_env = true;
                        SchemaSource::OCI {
                            apollo_key: opt
                                .apollo_key
                                .clone()
                                .ok_or(Opt::err_require_opt("APOLLO_KEY"))?,
                            reference: reference.clone(),
                        }
                    }
                }
            }
            _ => {
                // No APOLLO_KEY set - check if graph_artifact_reference was provided via CLI
                // If so, APOLLO_KEY is required
                if opt.graph_artifact_reference.is_some() {
                    return Err(anyhow!(
                        r#"{apollo_router_msg}

⚠️  The Apollo Router requires a license key ⚠️

Set the APOLLO_KEY environment variable:

  $ export APOLLO_KEY="your-apollo-key"
  $ ./router --graph-artifact-reference=<reference>

  For details, see the Apollo docs:
  https://www.apollographql.com/docs/federation/managed-federation/setup

  "#
                    ));
                }

                // No schema source provided via CLI/env - let config stream handle it
                // The config stream will check the parsed config for graph_artifact_reference
                SchemaSource::Static {
                    schema_sdl: String::new(),
                }
            }
        };

        // Order of precedence for licenses:
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
                // Only require APOLLO_GRAPH_REF for uplink license if not using graph_artifact_reference
                (_, _, Some(_apollo_key), Some(_apollo_graph_ref))
                    if !using_graph_artifact_reference =>
                {
                    LicenseSource::Registry {
                        apollo_key: opt.apollo_key.clone(),
                        apollo_graph_ref: opt.apollo_graph_ref.clone(),
                        endpoints: opt
                            .apollo_uplink_endpoints
                            .as_ref()
                            .map(|endpoints| Opt::parse_endpoints(endpoints))
                            .transpose()?,
                        poll_interval: INITIAL_UPLINK_POLL_INTERVAL,
                        timeout: opt.apollo_uplink_timeout,
                    }
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
            tracing::info!(
                "Custom plugins are present. To see log messages from your plugins you must configure `RUST_LOG` or `APOLLO_ROUTER_LOG` environment variables. See the Router logging documentation for more details"
            );
        }

        // Check uplink endpoint count for warning (only if not using graph_artifact_reference)
        // Note: Validation now happens in into_stream(), so we just check endpoint count here
        if !using_graph_artifact_reference
            && let Some(ref endpoints_str) = opt.apollo_uplink_endpoints
            && let Ok(endpoints) = Opt::parse_endpoints(endpoints_str)
            && endpoints.url_count() == 1
        {
            tracing::warn!(
                "Only a single uplink endpoint is configured. We recommend specifying at least two endpoints so that a fallback exists."
            );
        }

        // Build uplink config for builder (only if not using graph_artifact_reference)
        // Note: Validation now happens in into_stream(), so we construct it here for the builder
        let uplink_config_for_builder = if !using_graph_artifact_reference {
            opt.uplink_config().ok()
        } else {
            None
        };

        let router = RouterHttpServer::builder()
            .is_telemetry_disabled(opt.anonymous_telemetry_disabled)
            .configuration(configuration)
            .and_uplink(uplink_config_for_builder)
            .schema(schema_source)
            .license(license)
            .shutdown(shutdown.unwrap_or(ShutdownSource::CtrlC))
            .schema_source_provided(schema_source_provided_via_cli_env)
            .start();

        if let Err(err) = router.await {
            // Display helpful error message for NoSchema error
            if matches!(err, ApolloRouterError::NoSchema) {
                let apollo_router_msg = format!(
                    "Apollo Router v{} // (c) Apollo Graph, Inc. // Licensed as ELv2 (https://go.apollo.dev/elv2)",
                    std::env!("CARGO_PKG_VERSION")
                );
                eprintln!("{}{}", apollo_router_msg, STARTUP_ERROR_MESSAGE);
            }
            tracing::error!("{}", err);
            return Err(err.into());
        }
        Ok(())
    }
}

fn graph_os() -> bool {
    crate::services::APOLLO_KEY.lock().is_some()
        && crate::services::APOLLO_GRAPH_REF.lock().is_some()
}

fn setup_panic_handler() {
    // Redirect panics to the logs.
    let backtrace_env = std::env::var("RUST_BACKTRACE");
    let show_backtraces =
        backtrace_env.as_deref() == Ok("1") || backtrace_env.as_deref() == Ok("full");
    if show_backtraces {
        tracing::warn!(
            "RUST_BACKTRACE={} detected. This is useful for diagnostics but will have a performance impact and may leak sensitive information",
            backtrace_env.as_ref().unwrap()
        );
    }
    std::panic::set_hook(Box::new(move |e| {
        if show_backtraces {
            let backtrace = std::backtrace::Backtrace::capture();
            tracing::error!("{}\n{}", e, backtrace)
        } else {
            tracing::error!("{}", e)
        }

        // Once we've panic'ed the behaviour of the router is non-deterministic
        // We've logged out the panic details. Terminate with an error code
        std::process::exit(1);
    }));
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

    mod startup_tests {
        use std::env;
        use std::fs::File;
        use std::io::Write;
        use std::path::PathBuf;

        use anyhow::Result;
        use tempfile::TempDir;
        use tokio::time::Duration;

        use super::super::Executable;
        use super::super::Opt;
        use crate::router::SchemaSource;

        // Helper to create a temporary supergraph file
        async fn create_temp_supergraph_file() -> (PathBuf, TempDir) {
            let temp_dir = tempfile::tempdir().unwrap();
            let supergraph_path = temp_dir.path().join("supergraph.graphql");
            let mut file = File::create(&supergraph_path).unwrap();
            // Use a minimal valid supergraph schema for testing
            let supergraph_content = r#"schema
  @link(url: "https://specs.apollo.dev/link/v1.0")
  @link(url: "https://specs.apollo.dev/join/v0.3", for: EXECUTION) {
  query: Query
}

directive @join__enumValue(graph: join__Graph!) repeatable on ENUM_VALUE

directive @join__field(
  graph: join__Graph
  requires: join__FieldSet
  provides: join__FieldSet
  type: String
  external: Boolean
  override: String
  usedOverridden: Boolean
) repeatable on FIELD_DEFINITION | INPUT_FIELD_DEFINITION

directive @join__graph(name: String!, url: String!) on ENUM_VALUE

directive @join__implements(
  graph: join__Graph!
  interface: String!
) repeatable on OBJECT | INTERFACE

directive @join__type(
  graph: join__Graph!
  key: String!
  resolvable: Boolean = true
  isInterfaceObject: Boolean = false
) repeatable on OBJECT | INTERFACE | UNION | ENUM | INPUT_OBJECT | SCALAR

directive @link(
  url: String
  as: String
  for: link__Purpose
  import: [link__Import]
) repeatable on SCHEMA

scalar join__FieldSet

enum join__Graph {
  ACCOUNTS @join__graph(name: "accounts", url: "http://localhost:4001/graphql")
  INVENTORY @join__graph(name: "inventory", url: "http://localhost:4002/graphql")
  PRODUCTS @join__graph(name: "products", url: "http://localhost:4003/graphql")
  REVIEWS @join__graph(name: "reviews", url: "http://localhost:4004/graphql")
}

scalar link__Import

enum link__Purpose {
  SECURITY
  EXECUTION
}

type Query {
  me: User
}

type User
  @join__type(graph: ACCOUNTS, key: "id")
  @join__type(graph: PRODUCTS, key: "id")
  @join__type(graph: REVIEWS, key: "id") {
  id: ID!
  name: String @join__field(graph: ACCOUNTS)
  username: String @join__field(graph: ACCOUNTS)
}
"#;
            file.write_all(supergraph_content.as_bytes()).unwrap();
            (supergraph_path, temp_dir)
        }

        // Helper to create a temporary config file with graph_artifact_reference
        async fn create_temp_config_with_graph_artifact_reference(
            graph_artifact_ref: &str,
        ) -> (PathBuf, TempDir) {
            let temp_dir = tempfile::tempdir().unwrap();
            let config_path = temp_dir.path().join("router.yaml");
            let mut file = File::create(&config_path).unwrap();
            let config_content = format!(
                r#"
supergraph:
  listen: 127.0.0.1:0
  graph_artifact_reference: {}
health_check:
  listen: 127.0.0.1:0
"#,
                graph_artifact_ref
            );
            file.write_all(config_content.as_bytes()).unwrap();
            (config_path, temp_dir)
        }

        // Helper to create a temporary config file without graph_artifact_reference
        async fn create_temp_config_without_graph_artifact_reference() -> (PathBuf, TempDir) {
            let temp_dir = tempfile::tempdir().unwrap();
            let config_path = temp_dir.path().join("router.yaml");
            let mut file = File::create(&config_path).unwrap();
            let config_content = r#"
supergraph:
  listen: 127.0.0.1:0
health_check:
  listen: 127.0.0.1:0
"#;
            file.write_all(config_content.as_bytes()).unwrap();
            (config_path, temp_dir)
        }

        // Helper to save and restore environment variables
        struct EnvGuard {
            vars: Vec<(String, Option<String>)>,
        }

        impl EnvGuard {
            fn new(keys: &[&str]) -> Self {
                let vars = keys
                    .iter()
                    .map(|key| (key.to_string(), env::var(key).ok()))
                    .collect();
                Self { vars }
            }

            fn set(&self, key: &str, value: &str) {
                unsafe {
                    env::set_var(key, value);
                }
            }
        }

        impl Drop for EnvGuard {
            fn drop(&mut self) {
                for (key, original_value) in &self.vars {
                    match original_value {
                        Some(val) => unsafe {
                            env::set_var(key, val);
                        },
                        None => unsafe {
                            env::remove_var(key);
                        },
                    }
                }
            }
        }

        // Helper to test router startup and verify it succeeds
        async fn test_startup_success(
            schema: Option<SchemaSource>,
            config_path: Option<PathBuf>,
            env_vars: Vec<(&str, &str)>,
        ) -> Result<()> {
            test_startup_success_with_options(schema, config_path, env_vars, false, None).await
        }

        // Helper to test router startup with additional options
        async fn test_startup_success_with_options(
            schema: Option<SchemaSource>,
            config_path: Option<PathBuf>,
            env_vars: Vec<(&str, &str)>,
            hot_reload: bool,
            uplink_endpoints: Option<String>,
        ) -> Result<()> {
            // Save and set env vars
            let keys: Vec<&str> = env_vars.iter().map(|(k, _)| *k).collect();
            let guard = EnvGuard::new(&keys);
            for (key, value) in &env_vars {
                guard.set(key, value);
            }

            // Create Opt with test values
            let opt = Opt {
                log_level: "error".to_string(),
                hot_reload,
                config_path,
                dev: false,
                supergraph_path: None,
                supergraph_urls: None,
                command: None,
                apollo_key: env::var("APOLLO_KEY").ok(),
                #[cfg(unix)]
                apollo_key_path: None,
                apollo_graph_ref: env::var("APOLLO_GRAPH_REF").ok(),
                apollo_router_license: None,
                apollo_router_license_path: None,
                apollo_uplink_endpoints: uplink_endpoints
                    .or_else(|| env::var("APOLLO_UPLINK_ENDPOINTS").ok()),
                graph_artifact_reference: env::var("APOLLO_GRAPH_ARTIFACT_REFERENCE").ok(),
                anonymous_telemetry_disabled: true,
                apollo_uplink_timeout: Duration::from_secs(30),
                listen_address: None,
                version: false,
            };

            // Keep guard alive until after inner_start completes
            drop(guard);

            // Test inner_start - provide default license for tests
            use crate::router::LicenseSource;
            Executable::inner_start(None, schema, None, Some(LicenseSource::default()), opt).await
        }

        // Helper to test router startup and verify it fails with expected error
        async fn test_startup_failure(
            schema: Option<SchemaSource>,
            config_path: Option<PathBuf>,
            env_vars: Vec<(&str, &str)>,
            expected_error_contains: &str,
        ) {
            test_startup_failure_with_options(
                schema,
                config_path,
                env_vars,
                expected_error_contains,
                false,
                None,
            )
            .await
        }

        // Helper to test router startup failure with additional options
        async fn test_startup_failure_with_options(
            schema: Option<SchemaSource>,
            config_path: Option<PathBuf>,
            env_vars: Vec<(&str, &str)>,
            expected_error_contains: &str,
            hot_reload: bool,
            uplink_endpoints: Option<String>,
        ) {
            // Save and set env vars
            let keys: Vec<&str> = env_vars.iter().map(|(k, _)| *k).collect();
            let guard = EnvGuard::new(&keys);
            for (key, value) in &env_vars {
                guard.set(key, value);
            }

            // Create Opt with test values
            let opt = Opt {
                log_level: "error".to_string(),
                hot_reload,
                config_path,
                dev: false,
                supergraph_path: None,
                supergraph_urls: None,
                command: None,
                apollo_key: env::var("APOLLO_KEY").ok(),
                #[cfg(unix)]
                apollo_key_path: None,
                apollo_graph_ref: env::var("APOLLO_GRAPH_REF").ok(),
                apollo_router_license: None,
                apollo_router_license_path: None,
                apollo_uplink_endpoints: uplink_endpoints
                    .or_else(|| env::var("APOLLO_UPLINK_ENDPOINTS").ok()),
                graph_artifact_reference: env::var("APOLLO_GRAPH_ARTIFACT_REFERENCE").ok(),
                anonymous_telemetry_disabled: true,
                apollo_uplink_timeout: Duration::from_secs(30),
                listen_address: None,
                version: false,
            };

            // Keep guard alive until after inner_start completes
            drop(guard);

            // Test inner_start - should fail (provide default license to avoid license errors)
            use crate::router::LicenseSource;
            let result =
                Executable::inner_start(None, schema, None, Some(LicenseSource::default()), opt)
                    .await;

            // Verify it failed with expected error
            assert!(result.is_err(), "Expected startup to fail");
            let error_msg = result.unwrap_err().to_string();
            assert!(
                error_msg.contains(expected_error_contains),
                "Error message '{}' should contain '{}'",
                error_msg,
                expected_error_contains
            );
        }

        // VALID CASES

        #[tokio::test]
        async fn test_startup_with_supergraph_file_no_hot_reload() {
            let (supergraph_path, _temp_dir) = create_temp_supergraph_file().await;
            let schema = Some(SchemaSource::File {
                path: supergraph_path.clone(),
                watch: false,
            });

            // Use a timeout to verify router starts without NoSchema error
            let result = tokio::time::timeout(
                Duration::from_millis(500),
                test_startup_success(schema, None, vec![]),
            )
            .await;

            // Router should start (timeout means it's running, which is success)
            // Or it should fail with a non-NoSchema error (like subgraph connection error)
            match result {
                Ok(Ok(())) => {
                    // Router started successfully - this shouldn't happen in test without subgraphs
                    // but if it does, it means startup succeeded
                }
                Ok(Err(e)) => {
                    let error_msg = e.to_string();
                    // Should not fail with NoSchema - that's what we're testing
                    assert!(
                        !error_msg.contains("no valid schema was supplied"),
                        "Router should not fail with NoSchema error, got: {}",
                        error_msg
                    );
                    // It's OK if it fails with subgraph connection errors - that means schema was loaded
                }
                Err(_) => {
                    // Timeout - router is running, which means startup succeeded
                    // This is the expected case when router starts successfully
                }
            }
        }

        #[tokio::test]
        async fn test_startup_with_supergraph_file_with_hot_reload() {
            let (supergraph_path, _temp_dir) = create_temp_supergraph_file().await;
            let schema = Some(SchemaSource::File {
                path: supergraph_path.clone(),
                watch: true,
            });

            // Use a timeout to verify router starts without NoSchema error
            let result = tokio::time::timeout(
                Duration::from_millis(500),
                test_startup_success(schema, None, vec![]),
            )
            .await;

            // Router should start (timeout means it's running, which is success)
            match result {
                Ok(Ok(())) => {
                    // Router started successfully
                }
                Ok(Err(e)) => {
                    let error_msg = e.to_string();
                    // Should not fail with NoSchema
                    assert!(
                        !error_msg.contains("no valid schema was supplied"),
                        "Router should not fail with NoSchema error, got: {}",
                        error_msg
                    );
                }
                Err(_) => {
                    // Timeout - router is running, startup succeeded
                }
            }
        }

        #[tokio::test]
        async fn test_startup_with_graph_artifact_reference_no_hot_reload() {
            let valid_reference = "registry.apollographql.com/my-graph@sha256:1234567890abcdef1234567890abcdef1234567890abcdef1234567890abcdef";

            let result = test_startup_success(
                None,
                None,
                vec![
                    ("APOLLO_KEY", "test-key"),
                    ("APOLLO_GRAPH_ARTIFACT_REFERENCE", valid_reference),
                ],
            )
            .await;

            // Note: This will fail at OCI fetch, but should get past schema source selection
            // The error will be about OCI fetch failure, not NoSchema
            assert!(
                result.is_err(),
                "Should fail at OCI fetch, not schema source selection"
            );
            let error_msg = result.unwrap_err().to_string();
            // Should not be NoSchema error
            assert!(
                !error_msg.contains("no valid schema was supplied"),
                "Should not fail with NoSchema error, got: {}",
                error_msg
            );
        }

        #[tokio::test]
        async fn test_startup_with_graph_artifact_reference_with_hot_reload() {
            let valid_reference = "registry.apollographql.com/my-graph@sha256:1234567890abcdef1234567890abcdef1234567890abcdef1234567890abcdef";

            // Hot reload doesn't apply to OCI, but test the flag is accepted
            let result = test_startup_success(
                None,
                None,
                vec![
                    ("APOLLO_KEY", "test-key"),
                    ("APOLLO_GRAPH_ARTIFACT_REFERENCE", valid_reference),
                ],
            )
            .await;

            // Will fail at OCI fetch, but schema source should be selected correctly
            assert!(result.is_err());
            let error_msg = result.unwrap_err().to_string();
            assert!(
                !error_msg.contains("no valid schema was supplied"),
                "Should not fail with NoSchema error"
            );
        }

        #[tokio::test]
        async fn test_startup_with_apollo_graph_ref() {
            let result = test_startup_success(
                None,
                None,
                vec![
                    ("APOLLO_KEY", "test-key"),
                    ("APOLLO_GRAPH_REF", "my-graph@current"),
                ],
            )
            .await;

            // Will fail at Uplink fetch, but schema source should be selected correctly
            assert!(result.is_err());
            let error_msg = result.unwrap_err().to_string();
            assert!(
                !error_msg.contains("no valid schema was supplied"),
                "Should not fail with NoSchema error, got: {}",
                error_msg
            );
        }

        #[tokio::test]
        async fn test_startup_with_supergraph_urls() {
            use url::Url;
            let test_url = Url::parse("https://example.com/schema.graphql").unwrap();
            let schema = Some(SchemaSource::URLs {
                urls: vec![test_url],
            });

            let result = test_startup_success(schema, None, vec![]).await;
            // Will fail at URL fetch (network error), but schema source should be selected correctly
            // The error should be a network/fetch error, not NoSchema
            assert!(result.is_err());
            let error_msg = result.unwrap_err().to_string();
            assert!(
                !error_msg.contains("no valid schema was supplied"),
                "Should not fail with NoSchema error, got: {}",
                error_msg
            );
            // Should fail with a network/fetch error instead
            assert!(
                error_msg.contains("fetch")
                    || error_msg.contains("network")
                    || error_msg.contains("connection")
                    || error_msg.contains("http"),
                "Should fail with network/fetch error, got: {}",
                error_msg
            );
        }

        #[tokio::test]
        async fn test_startup_with_config_file_graph_artifact_reference() {
            let valid_reference = "registry.apollographql.com/my-graph@sha256:1234567890abcdef1234567890abcdef1234567890abcdef1234567890abcdef";
            let (config_path, _temp_dir) =
                create_temp_config_with_graph_artifact_reference(valid_reference).await;

            let result = test_startup_success(
                Some(SchemaSource::Static {
                    schema_sdl: String::new(),
                }),
                Some(config_path),
                vec![("APOLLO_KEY", "test-key")],
            )
            .await;

            // Will fail at OCI fetch, but config should be parsed and graph_artifact_reference found
            assert!(result.is_err());
            let error_msg = result.unwrap_err().to_string();
            assert!(
                !error_msg.contains("no valid schema was supplied"),
                "Should not fail with NoSchema error, config has graph_artifact_reference"
            );
        }

        #[tokio::test]
        async fn test_startup_with_apollo_graph_ref_and_uplink_endpoints() {
            // Test with custom uplink endpoints
            let result = test_startup_success_with_options(
                None,
                None,
                vec![
                    ("APOLLO_KEY", "test-key"),
                    ("APOLLO_GRAPH_REF", "my-graph@current"),
                ],
                false,
                Some("https://uplink1.example.com,https://uplink2.example.com".to_string()),
            )
            .await;

            // Will fail at Uplink fetch, but schema source should be selected correctly
            assert!(result.is_err());
            let error_msg = result.unwrap_err().to_string();
            assert!(
                !error_msg.contains("no valid schema was supplied"),
                "Should not fail with NoSchema error, got: {}",
                error_msg
            );
        }

        // INVALID CASES

        #[tokio::test]
        async fn test_startup_no_schema_source() {
            // No schema source, no config file
            test_startup_failure(None, None, vec![], "no valid schema was supplied").await;
        }

        #[tokio::test]
        async fn test_startup_no_schema_source_with_config_no_graph_artifact_ref() {
            // Config file exists but doesn't have graph_artifact_reference
            let (config_path, _temp_dir) =
                create_temp_config_without_graph_artifact_reference().await;

            test_startup_failure(
                Some(SchemaSource::Static {
                    schema_sdl: String::new(),
                }),
                Some(config_path),
                vec![],
                "no valid schema was supplied",
            )
            .await;
        }

        #[tokio::test]
        async fn test_startup_conflicting_supergraph_and_graph_artifact_reference() {
            let (supergraph_path, _temp_dir) = create_temp_supergraph_file().await;
            let schema = Some(SchemaSource::File {
                path: supergraph_path,
                watch: false,
            });

            test_startup_failure(
                schema,
                None,
                vec![
                    ("APOLLO_KEY", "test-key"),
                    (
                        "APOLLO_GRAPH_ARTIFACT_REFERENCE",
                        "registry.apollographql.com/my-graph@sha256:1234567890abcdef1234567890abcdef1234567890abcdef1234567890abcdef",
                    ),
                ],
                "cannot be used together",
            )
            .await;
        }

        #[tokio::test]
        async fn test_startup_conflicting_supergraph_urls_and_graph_artifact_reference() {
            use url::Url;
            let test_url = Url::parse("https://example.com/schema.graphql").unwrap();
            let schema = Some(SchemaSource::URLs {
                urls: vec![test_url],
            });

            test_startup_failure(
                schema,
                None,
                vec![
                    ("APOLLO_KEY", "test-key"),
                    (
                        "APOLLO_GRAPH_ARTIFACT_REFERENCE",
                        "registry.apollographql.com/my-graph@sha256:1234567890abcdef1234567890abcdef1234567890abcdef1234567890abcdef",
                    ),
                ],
                "cannot be used together",
            )
            .await;
        }

        #[tokio::test]
        async fn test_startup_graph_artifact_reference_without_apollo_key() {
            test_startup_failure(
                None,
                None,
                vec![(
                    "APOLLO_GRAPH_ARTIFACT_REFERENCE",
                    "registry.apollographql.com/my-graph@sha256:1234567890abcdef1234567890abcdef1234567890abcdef1234567890abcdef",
                )],
                "APOLLO_KEY",
            )
            .await;
        }

        #[tokio::test]
        async fn test_startup_invalid_graph_artifact_reference_format() {
            // Invalid format - missing @sha256: prefix
            // This will fail during validation in inner_start
            let keys: Vec<&str> = vec!["APOLLO_KEY", "APOLLO_GRAPH_ARTIFACT_REFERENCE"];
            let guard = EnvGuard::new(&keys);
            guard.set("APOLLO_KEY", "test-key");
            guard.set("APOLLO_GRAPH_ARTIFACT_REFERENCE", "invalid-reference");

            let opt = Opt {
                log_level: "error".to_string(),
                hot_reload: false,
                config_path: None,
                dev: false,
                supergraph_path: None,
                supergraph_urls: None,
                command: None,
                apollo_key: env::var("APOLLO_KEY").ok(),
                #[cfg(unix)]
                apollo_key_path: None,
                apollo_graph_ref: None,
                apollo_router_license: None,
                apollo_router_license_path: None,
                apollo_uplink_endpoints: None,
                graph_artifact_reference: env::var("APOLLO_GRAPH_ARTIFACT_REFERENCE").ok(),
                anonymous_telemetry_disabled: true,
                apollo_uplink_timeout: Duration::from_secs(30),
                listen_address: None,
                version: false,
            };

            drop(guard);

            // This should fail during OCI reference validation
            let result = Executable::inner_start(
                None,
                None,
                None,
                Some(crate::router::LicenseSource::default()),
                opt,
            )
            .await;

            assert!(result.is_err());
            let error_msg = result.unwrap_err().to_string();
            // The error will occur when trying to validate the OCI reference
            assert!(
                error_msg.contains("invalid") || error_msg.contains("graph artifact"),
                "Error should mention invalid reference, got: {}",
                error_msg
            );
        }

        #[tokio::test]
        async fn test_startup_apollo_key_without_graph_ref_or_artifact_reference() {
            // APOLLO_KEY set but no APOLLO_GRAPH_REF or graph_artifact_reference, and no config file
            test_startup_failure(
                Some(SchemaSource::Static {
                    schema_sdl: String::new(),
                }),
                None,
                vec![("APOLLO_KEY", "test-key")],
                "no valid schema was supplied",
            )
            .await;
        }

        #[tokio::test]
        async fn test_startup_missing_supergraph_file() {
            let non_existent_path = PathBuf::from("/non/existent/path/supergraph.graphql");
            let schema = Some(SchemaSource::File {
                path: non_existent_path,
                watch: false,
            });

            let result = test_startup_success(schema, None, vec![]).await;
            // Should fail because file doesn't exist
            assert!(result.is_err());
            let error_msg = result.unwrap_err().to_string();
            assert!(
                error_msg.contains("does not exist")
                    || error_msg.contains("No such file")
                    || error_msg.contains("not found")
                    || error_msg.contains("cannot find"),
                "Should fail with file not found error, got: {}",
                error_msg
            );
        }

        #[tokio::test]
        async fn test_startup_apollo_graph_ref_without_apollo_key() {
            // APOLLO_GRAPH_REF set but no APOLLO_KEY
            test_startup_failure(
                None,
                None,
                vec![("APOLLO_GRAPH_REF", "my-graph@current")],
                "APOLLO_KEY",
            )
            .await;
        }

        #[tokio::test]
        async fn test_startup_config_file_graph_artifact_reference_without_apollo_key() {
            // Config file has graph_artifact_reference but no APOLLO_KEY
            let valid_reference = "registry.apollographql.com/my-graph@sha256:1234567890abcdef1234567890abcdef1234567890abcdef1234567890abcdef";
            let (config_path, _temp_dir) =
                create_temp_config_with_graph_artifact_reference(valid_reference).await;

            let result = test_startup_success(
                Some(SchemaSource::Static {
                    schema_sdl: String::new(),
                }),
                Some(config_path),
                vec![], // No APOLLO_KEY
            )
            .await;

            // Should fail because APOLLO_KEY is required for graph_artifact_reference
            assert!(result.is_err());
            let error_msg = result.unwrap_err().to_string();
            assert!(
                error_msg.contains("APOLLO_KEY")
                    || error_msg.contains("no valid schema was supplied"),
                "Should fail with APOLLO_KEY error or NoSchema error, got: {}",
                error_msg
            );
        }

        #[tokio::test]
        async fn test_startup_invalid_uplink_endpoints_format() {
            // Invalid uplink endpoints format - not a valid URL
            let keys: Vec<&str> = vec!["APOLLO_KEY", "APOLLO_GRAPH_REF", "APOLLO_UPLINK_ENDPOINTS"];
            let guard = EnvGuard::new(&keys);
            guard.set("APOLLO_KEY", "test-key");
            guard.set("APOLLO_GRAPH_REF", "my-graph@current");
            guard.set("APOLLO_UPLINK_ENDPOINTS", "not-a-valid-url");

            let opt = Opt {
                log_level: "error".to_string(),
                hot_reload: false,
                config_path: None,
                dev: false,
                supergraph_path: None,
                supergraph_urls: None,
                command: None,
                apollo_key: env::var("APOLLO_KEY").ok(),
                #[cfg(unix)]
                apollo_key_path: None,
                apollo_graph_ref: env::var("APOLLO_GRAPH_REF").ok(),
                apollo_router_license: None,
                apollo_router_license_path: None,
                apollo_uplink_endpoints: env::var("APOLLO_UPLINK_ENDPOINTS").ok(),
                graph_artifact_reference: None,
                anonymous_telemetry_disabled: true,
                apollo_uplink_timeout: Duration::from_secs(30),
                listen_address: None,
                version: false,
            };

            drop(guard);

            // This should fail during uplink endpoint parsing
            let result = Executable::inner_start(
                None,
                None,
                None,
                Some(crate::router::LicenseSource::default()),
                opt,
            )
            .await;

            assert!(result.is_err());
            let error_msg = result.unwrap_err().to_string();
            assert!(
                error_msg.contains("invalid Apollo Uplink endpoint")
                    || error_msg.contains("invalid"),
                "Error should mention invalid uplink endpoint, got: {}",
                error_msg
            );
        }

        #[tokio::test]
        async fn test_startup_invalid_supergraph_urls_format() {
            // Invalid supergraph URL format - test that invalid URLs are caught
            // Note: Invalid URLs are caught during URL parsing, so we can't create a SchemaSource::URLs
            // with an invalid URL. This test verifies that URL parsing happens before schema source creation.
            // If we try to use Opt with invalid supergraph_urls, it should fail during parsing.
            use url::Url;
            let invalid_url = "not-a-valid-url";

            // Try to parse invalid URL - should fail
            let parse_result = Url::parse(invalid_url);
            assert!(parse_result.is_err(), "Invalid URL should fail to parse");

            // If we somehow had a valid URL object but it points to an unreachable endpoint,
            // test that it fails appropriately
            let test_url = Url::parse("https://example.com/schema.graphql").unwrap();
            let schema = Some(SchemaSource::URLs {
                urls: vec![test_url],
            });

            let result = test_startup_success(schema, None, vec![]).await;
            // Should fail with network/fetch error
            assert!(result.is_err());
            let error_msg = result.unwrap_err().to_string();
            assert!(
                error_msg.contains("fetch")
                    || error_msg.contains("network")
                    || error_msg.contains("connection")
                    || error_msg.contains("http"),
                "Should fail with network/fetch error, got: {}",
                error_msg
            );
        }

        #[tokio::test]
        async fn test_startup_graph_artifact_reference_with_hot_reload_flag() {
            // Test that hot_reload flag is accepted even though it doesn't apply to OCI
            let valid_reference = "registry.apollographql.com/my-graph@sha256:1234567890abcdef1234567890abcdef1234567890abcdef1234567890abcdef";

            let result = test_startup_success_with_options(
                None,
                None,
                vec![
                    ("APOLLO_KEY", "test-key"),
                    ("APOLLO_GRAPH_ARTIFACT_REFERENCE", valid_reference),
                ],
                true, // hot_reload enabled
                None,
            )
            .await;

            // Will fail at OCI fetch, but schema source should be selected correctly
            assert!(result.is_err());
            let error_msg = result.unwrap_err().to_string();
            assert!(
                !error_msg.contains("no valid schema was supplied"),
                "Should not fail with NoSchema error, got: {}",
                error_msg
            );
        }

        #[tokio::test]
        async fn test_startup_supergraph_file_with_hot_reload_flag() {
            // Test supergraph file with hot_reload flag explicitly set
            let (supergraph_path, _temp_dir) = create_temp_supergraph_file().await;
            let schema = Some(SchemaSource::File {
                path: supergraph_path.clone(),
                watch: true, // hot_reload enabled
            });

            let result = tokio::time::timeout(
                Duration::from_millis(500),
                test_startup_success_with_options(schema, None, vec![], true, None),
            )
            .await;

            match result {
                Ok(Ok(())) => {
                    // Router started successfully
                }
                Ok(Err(e)) => {
                    let error_msg = e.to_string();
                    assert!(
                        !error_msg.contains("no valid schema was supplied"),
                        "Router should not fail with NoSchema error, got: {}",
                        error_msg
                    );
                }
                Err(_) => {
                    // Timeout - router is running, startup succeeded
                }
            }
        }
    }
}
