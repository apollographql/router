//! `connect-migrate` — CLI for moving Apollo Connectors customers
//! between `connect/v0.X` specs.
//!
//! Built behind the `connect-migrate` feature of the `apollo-federation`
//! crate so the dependency on `clap` doesn't enter the default build
//! graph for library consumers:
//!
//!     cargo build --release --bin connect-migrate --features connect-migrate
//!
//! The library helpers the binary calls into live in the sibling
//! `mod.rs` and are part of the `apollo_federation::connectors::migration`
//! public surface.

use std::fs::File;
use std::io::BufWriter;
use std::io::{self};
use std::path::PathBuf;
use std::process::ExitCode;

use apollo_federation::connectors::migration::AGENT_GUIDE;
use apollo_federation::connectors::migration::analyze;
use clap::Parser;
use clap::Subcommand;
use clap::ValueEnum;

/// Binary version reported by `connect-migrate --version`.
///
/// Decoupled from `apollo-federation`'s package version because
/// `connect-migrate` ships from a separate release repo with its own
/// cadence. The release CI overrides this via the
/// `CONNECT_MIGRATE_VERSION` env var at compile time (set from the
/// pushed git tag); local dev builds get the `-dev` suffix.
const VERSION: &str = match option_env!("CONNECT_MIGRATE_VERSION") {
    Some(v) => v,
    None => "0.0.0-dev",
};

#[derive(Parser, Debug)]
#[command(
    name = "connect-migrate",
    about = "Help upgrade Apollo Connectors schemas across connect/v0.X spec versions",
    long_about = None,
    version = VERSION,
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Print the developer-facing migration guide that an agent can
    /// follow when walking a customer through a v0.3 → v0.4 upgrade.
    ///
    /// The guide is embedded in the binary at compile time. Print it
    /// and pipe to your agent of choice, or read it manually.
    AgentGuide,

    /// Scan a project for `@connect(selection: …)` sites that change
    /// meaning between `connect/v0.3` and `connect/v0.4`.
    ///
    /// Walks the given paths (defaults to `.`), parses every
    /// `.graphql` file, and emits a migration manifest (or JSONL via
    /// `--format=json`) sorting each divergent site into rewrites to
    /// apply, no-ops, and questions for the developer.
    Analyze {
        /// Paths to scan. May be files or directories. Directories
        /// are walked recursively for `.graphql` files. Defaults to
        /// the current directory.
        #[arg(default_value = ".")]
        paths: Vec<PathBuf>,

        /// Output path. Defaults to stdout, so the typical idiom is
        /// `connect-migrate analyze subgraphs > connect-migrate-manifest-$(date -u +%Y-%m-%dT%H-%M-%SZ).md`.
        /// Use `-` to be explicit about stdout when scripting.
        #[arg(long, short)]
        output: Option<PathBuf>,

        /// Output format.
        #[arg(long, value_enum, default_value_t = OutputFormat::Markdown)]
        format: OutputFormat,
    },
}

#[derive(Copy, Clone, Debug, ValueEnum)]
enum OutputFormat {
    /// Human- and agent-readable migration manifest with structured
    /// `site v2` identity comments the agent applies edits from. Default.
    Markdown,
    /// One JSON record per site, separated by newlines. Useful for
    /// tool-consuming agents that prefer typed records over markdown.
    Json,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match cli.command {
        Command::AgentGuide => {
            print!("{}", AGENT_GUIDE);
            ExitCode::SUCCESS
        }
        Command::Analyze {
            paths,
            output,
            format,
        } => match run_analyze(paths, output, format) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("connect-migrate analyze: {e}");
                ExitCode::FAILURE
            }
        },
    }
}

fn run_analyze(
    paths: Vec<PathBuf>,
    output: Option<PathBuf>,
    format: OutputFormat,
) -> io::Result<()> {
    // When exactly one path argument is a directory, treat it as the
    // project root so emitted `file:` paths are relative to that dir
    // rather than to the developer's cwd. The site locator (line:col)
    // is unaffected; this is purely cosmetic.
    let project_root = match paths.as_slice() {
        [only] if only.is_dir() => only.clone(),
        _ => std::env::current_dir()?,
    };
    let report = analyze::analyze(&paths, &project_root);

    // Default to stdout; `-o <path>` (or `-o -` for explicit stdout)
    // overrides. Keeps `connect-migrate analyze subgraphs > manifest.md`
    // as the obvious idiom and lets scripts pipe to other consumers
    // without an intermediate file.
    let stdout = io::stdout();
    let writing_to_path = matches!(&output, Some(p) if p.as_os_str() != "-");
    let mut sink: Box<dyn std::io::Write> = match output.as_deref() {
        Some(p) if p.as_os_str() != "-" => Box::new(BufWriter::new(File::create(p)?)),
        _ => Box::new(BufWriter::new(stdout.lock())),
    };

    match format {
        OutputFormat::Markdown => {
            analyze::write_markdown(&mut sink, &report, &project_root, VERSION)?;
        }
        OutputFormat::Json => {
            analyze::write_jsonl(&mut sink, &report.sites)?;
        }
    }

    let total = report.sites.len();
    let kept = report
        .sites
        .iter()
        .filter(|s| matches!(s.recommendation, analyze::Recommendation::KeepV03))
        .count();
    let embraced = report
        .sites
        .iter()
        .filter(|s| matches!(s.recommendation, analyze::Recommendation::EmbraceV04))
        .count();
    let ambiguous = report
        .sites
        .iter()
        .filter(|s| matches!(s.recommendation, analyze::Recommendation::Ambiguous))
        .count();
    eprintln!(
        "scanned {files} file(s); analyzed {analyzed} `@connect` directive(s); {total} site(s) need attention ({kept} keep-v0.3 · {embraced} embrace-v0.4 · {ambiguous} ambiguous) — {kind}",
        files = report.files_scanned,
        analyzed = report.directives_analyzed,
        kind = report.result_kind().as_str(),
    );
    if writing_to_path && let Some(path) = output.as_deref() {
        eprintln!("wrote: {}", path.display());
    }

    Ok(())
}
