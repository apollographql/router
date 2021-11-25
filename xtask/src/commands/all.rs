use structopt::StructOpt;

use super::{Compliance, Lint, Test};

#[derive(Debug, StructOpt)]
pub struct All {
    #[structopt(flatten)]
    pub compliance: Compliance,
    #[structopt(flatten)]
    pub lint: Lint,
    #[structopt(flatten)]
    pub test: Test,
}
