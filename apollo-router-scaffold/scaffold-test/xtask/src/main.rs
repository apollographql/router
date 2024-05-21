use anyhow::Result;
use apollo_router_scaffold::RouterAction;
use clap::Parser;
use clap::Subcommand;

#[derive(Parser, Debug)]
struct Args {
    #[clap(subcommand)]
    action: Action,
}

impl Args {
    fn execute(&self) -> Result<()> {
        self.action.execute()
    }
}

#[derive(Subcommand, Debug)]
enum Action {
    /// Forward to router action
    Router {
        #[clap(subcommand)]
        action: RouterAction,
    },
}

impl Action {
    fn execute(&self) -> Result<()> {
        match self {
            Action::Router { action } => action.execute(),
        }
    }
}

fn main() -> Result<()> {
    let args = Args::parse();
    args.execute()
}
