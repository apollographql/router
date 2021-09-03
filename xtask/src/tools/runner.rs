use crate::utils;

use anyhow::{bail, Context, Result};
use camino::{Utf8Path, Utf8PathBuf};
use which::which;

use std::collections::HashMap;
use std::convert::TryInto;
use std::process::{Child, Command, Stdio};
use std::str;

pub(crate) struct Runner {
    pub(crate) verbose: bool,
    pub(crate) tool_name: String,
    pub(crate) tool_exe: Utf8PathBuf,
}

impl Runner {
    pub(crate) fn new(tool_name: &str, verbose: bool) -> Result<Self> {
        let tool_exe = which(tool_name).with_context(|| {
            format!(
                "You must have {} installed to run this command.",
                &tool_name
            )
        })?;
        Ok(Runner {
            verbose,
            tool_name: tool_name.to_string(),
            tool_exe: tool_exe.try_into()?,
        })
    }

    pub(crate) fn exec(
        &self,
        args: &[&str],
        directory: &Utf8Path,
        env: Option<&HashMap<String, String>>,
    ) -> Result<()> {
        let full_command = format!("`{} {}`", &self.tool_name, args.join(" "));
        utils::info(&format!("running {} in `{}`", &full_command, directory));
        if self.verbose {
            if let Some(env) = env {
                utils::info("env:");
                for (key, value) in env {
                    utils::info(&format!("  ${}={}", key, value));
                }
            }
        }

        let mut command = Command::new(&self.tool_exe);
        command.current_dir(directory).args(args);
        if let Some(env) = env {
            command.envs(env);
        }
        if !command
            .status()
            .with_context(|| "Could not spawn child process")?
            .success()
        {
            bail!("Command failed");
        }
        Ok(())
    }

    pub(crate) fn exec_background(
        &self,
        args: &[&str],
        directory: &Utf8Path,
        env: Option<&HashMap<String, String>>,
    ) -> Result<BackgroundTask> {
        let full_command = format!("`{} {}`", &self.tool_name, args.join(" "));
        utils::info(&format!("running {} in `{}`", &full_command, directory));
        if self.verbose {
            if let Some(env) = env {
                utils::info("env:");
                for (key, value) in env {
                    utils::info(&format!("  ${}={}", key, value));
                }
            }
        }

        let mut command = Command::new(&self.tool_exe);
        command
            .current_dir(directory)
            .args(args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if let Some(env) = env {
            command.envs(env);
        }
        let child = command
            .spawn()
            .with_context(|| "Could not spawn child process")?;
        Ok(BackgroundTask { child })
    }
}

pub(crate) struct BackgroundTask {
    child: Child,
}

impl Drop for BackgroundTask {
    fn drop(&mut self) {
        #[cfg(unix)]
        {
            use std::thread::sleep;
            use std::time::Duration;

            // attempt to stop gracefully
            let pid = self.child.id();
            unsafe {
                libc::kill(libc::pid_t::from_ne_bytes(pid.to_ne_bytes()), libc::SIGTERM);
            }

            for _ in 0..10 {
                if self.child.try_wait().ok().flatten().is_some() {
                    break;
                }
                sleep(Duration::from_secs(1));
            }
        }

        if self.child.try_wait().ok().flatten().is_none() {
            // still alive? kill it with fire
            let _ = self.child.kill();
        }

        let _ = self.child.wait();
    }
}
