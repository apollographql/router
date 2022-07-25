mod federation_demo;
mod jaeger;

use std::convert::TryFrom;
use std::env;
use std::process::Child;
use std::process::Command;
use std::str;

pub use anyhow;
use anyhow::Context;
use anyhow::Result;
use camino::Utf8PathBuf;
use cargo_metadata::MetadataCommand;
pub use federation_demo::*;
pub use jaeger::*;
use once_cell::sync::Lazy;

const MANIFEST_DIR: &str = env!("CARGO_MANIFEST_DIR");
#[cfg(not(windows))]
pub const RELEASE_BIN: &str = "router";
#[cfg(windows)]
pub const RELEASE_BIN: &str = "router.exe";
#[allow(dead_code)]
pub const PKG_PROJECT_NAME: &str = "router";

pub static PKG_VERSION: Lazy<String> = Lazy::new(|| {
    let metadata = MetadataCommand::new()
        .manifest_path(PKG_PROJECT_ROOT.join("Cargo.toml"))
        .exec()
        .expect("could not retrieve metadata");
    let router = metadata
        .packages
        .iter()
        .find(|x| x.name == "apollo-router")
        .expect("could not find crate apollo-router");

    router.version.to_string()
});

pub static PKG_PROJECT_ROOT: Lazy<Utf8PathBuf> = Lazy::new(|| {
    let manifest_dir =
        Utf8PathBuf::try_from(MANIFEST_DIR).expect("could not get the root directory.");
    let root_dir = manifest_dir
        .ancestors()
        .nth(1)
        .expect("could not find project root");

    root_dir.to_path_buf()
});

pub static TARGET_DIR: Lazy<Utf8PathBuf> = Lazy::new(|| {
    let metadata = MetadataCommand::new()
        .manifest_path(PKG_PROJECT_ROOT.join("Cargo.toml"))
        .exec()
        .expect("could not retrieve metadata");

    metadata.target_directory
});

#[macro_export]
macro_rules! cargo {
    (
        $args:expr
        $(
            , env = {
                $( $k: expr => $v: expr ),*
                $(,)?
            }
        )?
        $(,)?
    ) => {{
        let mut command = ::std::process::Command::new(which::which("cargo")?);

        command.args($args);
        $(
            $(
                command.env($k, $v);
            )*
        )?

        let status = command
            .current_dir(&*PKG_PROJECT_ROOT)
            .status()?;

        $crate::anyhow::ensure!(status.success(), "cargo command failed");
    }};
}

#[macro_export]
macro_rules! npm {
    ($current_dir:expr => $( $args:expr ),* $(,)?) => {{
        let mut command = ::std::process::Command::new(which::which("npm")?);

        $(
        command.args($args);
        )*

        let status = command
            .current_dir($current_dir)
            .status()?;

        $crate::anyhow::ensure!(status.success(), "npm command failed");
    }};
}

pub struct BackgroundTask {
    child: Child,
}

impl BackgroundTask {
    pub fn new(mut command: Command) -> Result<Self> {
        let child = command
            .spawn()
            .with_context(|| "Could not spawn child process")?;

        Ok(Self { child })
    }
}

impl Drop for BackgroundTask {
    fn drop(&mut self) {
        #[cfg(unix)]
        {
            // attempt to stop gracefully
            let pid = self.child.id();
            unsafe {
                libc::kill(libc::pid_t::from_ne_bytes(pid.to_ne_bytes()), libc::SIGTERM);
            }

            for _ in 0..10 {
                if self.child.try_wait().ok().flatten().is_some() {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_secs(1));
            }
        }

        if self.child.try_wait().ok().flatten().is_none() {
            // still alive? kill it with fire
            let _ = self.child.kill();
        }

        let _ = self.child.wait();
    }
}
