use anyhow::anyhow;
use camino::Utf8Path;

use std::{collections::HashMap, fmt, str::FromStr};

use crate::Result;

const TARGET_MUSL_LINUX: &str = "x86_64-unknown-linux-musl";
const TARGET_GNU_LINUX: &str = "x86_64-unknown-linux-gnu";
const TARGET_WINDOWS: &str = "x86_64-pc-windows-msvc";
const TARGET_MACOS: &str = "x86_64-apple-darwin";
const BREW_OPT: &[&str] = &["/usr/local/opt", "/opt/homebrew/Cellar"];

pub(crate) const POSSIBLE_TARGETS: [&str; 4] = [
    TARGET_MUSL_LINUX,
    TARGET_GNU_LINUX,
    TARGET_WINDOWS,
    TARGET_MACOS,
];

#[derive(Debug, PartialEq, Clone)]
pub(crate) enum Target {
    MuslLinux,
    GnuLinux,
    Windows,
    MacOS,
    Other,
}

impl Target {
    pub(crate) fn get_args(&self) -> Vec<String> {
        let mut args = vec![];

        if let Self::MuslLinux | Self::GnuLinux | Self::Windows | Self::MacOS = self {
            args.push("--target".to_string());
            args.push(self.to_string());
        }
        if !self.composition_js() {
            args.push("--no-default-features".to_string());
        }
        args
    }

    pub(crate) fn is_other(&self) -> bool {
        Self::Other == *self
    }

    pub(crate) fn get_env(&self) -> Result<Option<HashMap<String, String>>> {
        let mut env = HashMap::new();
        Ok(match self {
            Target::GnuLinux | Target::MuslLinux => {
                env.insert("OPENSSL_STATIC".to_string(), "1".to_string());
                Some(env)
            }
            Target::MacOS => {
                let openssl_path = BREW_OPT
                    .iter()
                    .map(|x| Utf8Path::new(x).join("openssl@1.1"))
                    .find(|x| x.exists())
                    .ok_or_else(|| {
                        anyhow!(
                            "OpenSSL v1.1 is not installed. Please install with `brew install \
                        openssl@1.1`"
                        )
                    })?;

                env.insert("OPENSSL_ROOT_DIR".to_string(), openssl_path.to_string());
                env.insert("OPENSSL_STATIC".to_string(), "1".to_string());

                Some(env)
            }
            Target::Windows => {
                env.insert(
                    "RUSTFLAGS".to_string(),
                    "-Ctarget-feature=+crt-static".to_string(),
                );
                Some(env)
            }
            _ => None,
        })
    }

    fn composition_js(&self) -> bool {
        !matches!(self, Target::MuslLinux)
    }
}

impl Default for Target {
    fn default() -> Self {
        if cfg!(target_os = "windows") {
            Target::Windows
        } else if cfg!(target_os = "linux") {
            if cfg!(target_env = "gnu") {
                Target::GnuLinux
            } else if cfg!(target_env = "musl") {
                Target::MuslLinux
            } else {
                Target::Other
            }
        } else if cfg!(target_os = "macos") {
            Target::MacOS
        } else {
            Target::Other
        }
    }
}

impl FromStr for Target {
    type Err = anyhow::Error;

    fn from_str(input: &str) -> Result<Self, Self::Err> {
        match input {
            TARGET_MUSL_LINUX => Ok(Self::MuslLinux),
            TARGET_GNU_LINUX => Ok(Self::GnuLinux),
            TARGET_WINDOWS => Ok(Self::Windows),
            TARGET_MACOS => Ok(Self::MacOS),
            _ => Ok(Self::Other),
        }
    }
}

impl fmt::Display for Target {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let msg = match &self {
            Target::MuslLinux => TARGET_MUSL_LINUX,
            Target::GnuLinux => TARGET_GNU_LINUX,
            Target::Windows => TARGET_WINDOWS,
            Target::MacOS => TARGET_MACOS,
            Target::Other => "unknown-target",
        };
        write!(f, "{}", msg)
    }
}
