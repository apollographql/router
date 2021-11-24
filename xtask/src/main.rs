mod commands;

use ansi_term::Colour::Green;
use anyhow::Result;
use structopt::StructOpt;

fn main() -> Result<()> {
    let app = Xtask::from_args();
    app.run()
}

#[derive(Debug, StructOpt)]
#[structopt(
    name = "xtask",
    about = "Workflows used locally and in CI for developing Router"
)]
struct Xtask {
    #[structopt(subcommand)]
    pub command: Command,
}

#[derive(Debug, StructOpt)]
pub enum Command {
    /// Locally run all the checks the CI will perform.
    All(All),
    /// Check the code for licence and security compliance.
    CheckCompliance(commands::Compliance),

    /// Build Router's binaries for distribution.
    Dist(commands::Dist),

    /// Run linters for Router.
    Lint(commands::Lint),

    /// Run tests for Router.
    Test(commands::Test),

    /// Package build.
    Package(commands::Package),
}

#[derive(Debug, StructOpt)]
pub struct All {
    #[structopt(flatten)]
    compliance: commands::Compliance,
    #[structopt(flatten)]
    lint: commands::Lint,
    #[structopt(flatten)]
    test: commands::Test,
}

impl Xtask {
    pub fn run(&self) -> Result<()> {
        match &self.command {
            Command::All(All {
                compliance,
                lint,
                test,
            }) => {
                eprintln!("Checking format and clippy...");
                lint.run()?;
                eprintln!("Checking licenses...");
                compliance.run()?;
                eprintln!("Running tests");
                test.run()
            }
            Command::CheckCompliance(command) => command.run(),
            Command::Dist(command) => command.run(),
            Command::Lint(command) => command.run(),
            Command::Test(command) => command.run(),
            Command::Package(command) => command.run(),
        }?;
        eprintln!("{}", Green.bold().paint("Success!"));
        Ok(())
    }
}
