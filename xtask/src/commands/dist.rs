use anyhow::Result;
use xtask::*;

const RELEASE_DIST_DIR: &str = "release-dist";

#[derive(Debug, clap::Parser)]
pub struct Dist {
    #[clap(long)]
    target: Option<String>,

    /// Pass --features to cargo test
    #[clap(long)]
    features: Option<String>,
}

impl Dist {
    pub fn run(&self) -> Result<()> {
        let mut args = vec!["build", "--profile", "release-dist"];
        if let Some(features) = &self.features {
            args.push("--features");
            args.push(features);
        }

        match &self.target {
            Some(target) => {
                args.push("--target");
                args.push(target);
                cargo!(args);

                let bin_path = TARGET_DIR
                    .join(target)
                    .join(RELEASE_DIST_DIR)
                    .join(RELEASE_BIN);

                eprintln!("successfully compiled to: {}", &bin_path);
            }
            None => {
                cargo!(args);

                let bin_path = TARGET_DIR.join(RELEASE_DIST_DIR).join(RELEASE_BIN);

                eprintln!("successfully compiled to: {}", &bin_path);
            }
        }

        Ok(())
    }
}
