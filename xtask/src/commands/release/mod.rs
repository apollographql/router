mod common;
mod new;
mod pre_verify;
mod prepare;
mod reconcile;
mod state;
mod status;

use anyhow::Result;
pub(crate) use new::New;
pub(crate) use pre_verify::PreVerify;
pub(crate) use prepare::Prepare;
pub(crate) use reconcile::Reconcile;
pub(crate) use status::Status;

#[derive(Debug, clap::Subcommand)]
pub enum Command {
    /// Cut a fresh release branch and open the draft release PR.
    New(New),

    /// Prepare a new release
    Prepare(Prepare),

    /// Verify that a release is ready to be published
    PreVerify,

    /// Open (or resume) the reconcile-main-back-to-dev PR for a released version.
    Reconcile(Reconcile),

    /// Show the current state of release work across all release lines.
    Status(Status),
}

impl Command {
    pub fn run(&self) -> Result<()> {
        match self {
            Command::New(command) => command.run(),
            Command::Prepare(command) => command.run(),
            Command::PreVerify => PreVerify::run(),
            Command::Reconcile(command) => command.run(),
            Command::Status(command) => command.run(),
        }
    }
}
