//! Persistent native transport for the query-plan-checker oracle.
//!
//! Same shape as [`crate::lean_oracle`]: a native binary built from the Lean model (see
//! `scripts/build-plan-oracle.sh`), kept alive across cases, one line in and one line out.

use std::ffi::OsString;
use std::fs;
use std::io::BufRead;
use std::io::BufReader;
use std::io::Write;
use std::path::Path;
use std::process::Child;
use std::process::ChildStdin;
use std::process::ChildStdout;
use std::process::Command;
use std::process::Stdio;

pub const PLAN_ORACLE_ENV: &str = "QUERY_PLAN_LEAN_ORACLE";
pub const ALLOW_STALE_ORACLE_ENV: &str = "QUERY_PLAN_ALLOW_STALE_LEAN_ORACLE";

/// The `apollo-graphql-lean` revision this harness is aligned with. The build script records what
/// it built from beside the binary, and the runner refuses a mismatch unless the override is set.
pub const PLAN_MODEL_COMMIT: &str = "26c8a96102715cde336ba5d90f39b371805c7b27";

pub struct PlanOracle {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
}

impl PlanOracle {
    pub fn from_env() -> Option<Self> {
        let executable = std::env::var_os(PLAN_ORACLE_ENV)?;
        Some(Self::open(executable))
    }

    pub fn open(executable: impl AsRef<Path>) -> Self {
        validate_model_commit(executable.as_ref());
        let mut child = Command::new(executable.as_ref())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap_or_else(|error| {
                panic!(
                    "start plan oracle {}: {error}",
                    executable.as_ref().display()
                )
            });
        PlanOracle {
            stdin: child.stdin.take().expect("open plan oracle stdin"),
            stdout: BufReader::new(child.stdout.take().expect("open plan oracle stdout")),
            child,
        }
    }

    fn request(&mut self, payload: &str) -> String {
        writeln!(self.stdin, "{payload}").expect("write plan oracle request");
        self.stdin.flush().expect("flush plan oracle request");
        let mut line = String::new();
        self.stdout
            .read_line(&mut line)
            .expect("read plan oracle response");
        assert!(!line.is_empty(), "plan oracle exited before responding");
        let (id, result) = line
            .trim_end()
            .split_once('=')
            .unwrap_or_else(|| panic!("unexpected plan oracle output: {line}"));
        assert_eq!(id, "ok", "plan oracle reported: {result}");
        result.to_string()
    }

    /// The oracle's own rendering of the shared fixture, compared against
    /// [`crate::plan_model::schema_digest`] before any case runs. The two sides hold independent
    /// copies, and a silent divergence there would make every verdict meaningless.
    pub fn schema_digest(&mut self) -> String {
        self.request("schema")
    }

    /// `checkQueryPlan` on one byte string, or `None` when the grammar discarded it — which both
    /// sides must do for the same bytes.
    pub fn check(&mut self, bytes: &[u8]) -> Option<bool> {
        let hex: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
        match self.request(&format!("check {hex}")).as_str() {
            "1" => Some(true),
            "0" => Some(false),
            "skip" => None,
            other => panic!("unexpected plan oracle flag: {other}"),
        }
    }
}

impl PlanOracle {
    /// The two halves of `checkQueryPlan` apart: `(complete, sound)`.
    pub fn halves(&mut self, bytes: &[u8]) -> Option<(bool, bool)> {
        let hex: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
        let response = self.request(&format!("halves {hex}"));
        if response == "skip" {
            return None;
        }
        let (complete, sound) = response.split_once(',')?;
        Some((complete == "1", sound == "1"))
    }

    /// Both operands of the soundness inclusion test for the entity fetch's first case and first
    /// key: what it has already fetched, and what that key demands of it.
    pub fn requirement(&mut self, bytes: &[u8]) -> Option<(String, String)> {
        let hex: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
        let response = self.request(&format!("requirement {hex}"));
        let (left, right) = response.split_once(" ||| ")?;
        Some((left.trim().to_string(), right.trim().to_string()))
    }

    /// Both operands of the completeness test, as the oracle builds them: `(left, right)`.
    ///
    /// A disagreement in that half is really a disagreement about these two operations, and
    /// taking them from the oracle rather than rebuilding them here is what makes the pair
    /// evidence rather than a guess.
    pub fn operands(&mut self, bytes: &[u8]) -> Option<(String, String)> {
        let hex: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
        let response = self.request(&format!("operands {hex}"));
        if response == "skip" {
            return None;
        }
        let (left, right) = response.split_once(" ||| ")?;
        Some((left.trim().to_string(), right.trim().to_string()))
    }
}

fn validate_model_commit(executable: &Path) {
    let mut sidecar: OsString = executable.as_os_str().to_owned();
    sidecar.push(".model-commit");
    let actual = fs::read_to_string(Path::new(&sidecar))
        .unwrap_or_else(|error| panic!("read plan oracle model commit sidecar: {error}"));
    let actual = actual.trim();
    if actual == PLAN_MODEL_COMMIT {
        return;
    }
    if std::env::var_os(ALLOW_STALE_ORACLE_ENV).is_some() {
        eprintln!("warning: plan oracle is commit {actual}, expected {PLAN_MODEL_COMMIT}");
        return;
    }
    panic!(
        "plan oracle is commit {actual}, expected {PLAN_MODEL_COMMIT}; \
         set {ALLOW_STALE_ORACLE_ENV}=1 to override deliberately"
    );
}

impl Drop for PlanOracle {
    fn drop(&mut self) {
        let _ = writeln!(self.stdin, "quit");
        let _ = self.stdin.flush();
        let _ = self.child.wait();
    }
}
