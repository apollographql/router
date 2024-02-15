use anyhow::Result;
use xtask::*;

#[derive(Debug, clap::Parser)]
pub struct Dist {
    #[clap(long)]
    target: Option<String>,
}

impl Dist {
    pub fn run(&self) -> Result<()> {
        match &self.target {
            Some(target) => {
                cargo!(["build", "--release", "--target", target]);

                let bin_path = TARGET_DIR
                    .join(target.to_string())
                    .join("release")
                    .join(RELEASE_BIN);

                eprintln!("successfully compiled to: {}", &bin_path);
            }
            None => {
                cargo!(["build", "--release"]);

                let bin_path = TARGET_DIR.join("release").join(RELEASE_BIN);

                eprintln!("successfully compiled to: {}", &bin_path);
            }
        }

        Ok(())
    }
}
