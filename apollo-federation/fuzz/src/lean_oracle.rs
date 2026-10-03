//! Persistent native Lean-oracle transport.
//!
//! The oracle is a native binary built from the Lean model (see `scripts/build-oracle.sh`), kept
//! alive across cases so that neither process startup nor Lean initialization is paid per test.
//! The protocol is one line in, one line out: `<id>=<result>`.
//!
//! Adapted from the same transport in `duckki/graphql-static-analysis-rs`.

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

pub const LEAN_ORACLE_ENV: &str = "QUERY_INCLUSION_LEAN_ORACLE";
pub const ALLOW_STALE_ORACLE_ENV: &str = "QUERY_INCLUSION_ALLOW_STALE_LEAN_ORACLE";

/// The Lean revision this harness is aligned with. The build script records the revision it built
/// from beside the binary, and the runner refuses a mismatch unless the override is set.
pub const LEAN_MODEL_COMMIT: &str = "06a5d04d6c00b875d7da9c1c4f1c148b32191f0d";

pub struct LeanOracle {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
}

/// One oracle verdict for an ordered pair of operations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OracleVerdict {
    /// `includesBool left right`, the executable checker the Rust port mirrors.
    pub includes: bool,
    /// `includesBoolReference left right`, the exhaustive enumeration in the same model.
    pub reference: bool,
}

impl LeanOracle {
    pub fn from_env() -> Option<Self> {
        let executable = std::env::var_os(LEAN_ORACLE_ENV)?;
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
                    "start Lean oracle {}: {error}",
                    executable.as_ref().display()
                )
            });
        LeanOracle {
            stdin: child.stdin.take().expect("open Lean oracle stdin"),
            stdout: BufReader::new(child.stdout.take().expect("open Lean oracle stdout")),
            child,
        }
    }

    fn request(&mut self, payload: &str) -> String {
        writeln!(self.stdin, "{payload}").expect("write Lean oracle request");
        self.stdin.flush().expect("flush Lean oracle request");
        let mut line = String::new();
        self.stdout
            .read_line(&mut line)
            .expect("read Lean oracle response");
        assert!(!line.is_empty(), "Lean oracle exited before responding");
        let (id, result) = line
            .trim_end()
            .split_once('=')
            .unwrap_or_else(|| panic!("unexpected Lean oracle output: {line}"));
        assert_eq!(id, "ok", "Lean oracle reported: {result}");
        result.to_string()
    }

    /// The oracle's own rendering of the shared schema tables. Compared against
    /// [`crate::model::schema_digest`] before any case runs, because the two sides hold
    /// independent copies of the schema and a silent divergence there would make every
    /// subsequent comparison meaningless.
    pub fn schema_digest(&mut self) -> String {
        self.request("schema")
    }

    /// Times `includesBool` on one case *inside* the oracle process, returning nanoseconds for
    /// `iterations` runs. Timing through `verdicts` instead would measure the line protocol.
    pub fn bench(&mut self, bytes: &[u8], iterations: usize) -> u64 {
        let hex: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
        let response = self.request(&format!("bench {iterations} {hex}"));
        let (nanos, _accepted) = response
            .split_once(',')
            .unwrap_or_else(|| panic!("unexpected Lean bench response: {response}"));
        nanos.parse().expect("nanoseconds")
    }

    /// Both directions for one byte string. Asking for the reverse direction too doubles the
    /// observations per case at almost no cost, and inclusion is not symmetric, so the two
    /// answers are genuinely independent.
    pub fn verdicts(&mut self, bytes: &[u8]) -> (OracleVerdict, OracleVerdict) {
        let hex: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
        let response = self.request(&format!("includes {hex}"));
        let flags: Vec<bool> = response
            .split(',')
            .map(|flag| match flag {
                "1" => true,
                "0" => false,
                other => panic!("unexpected Lean oracle flag: {other}"),
            })
            .collect();
        assert_eq!(flags.len(), 4, "Lean oracle returned {response}");
        (
            OracleVerdict {
                includes: flags[0],
                reference: flags[1],
            },
            OracleVerdict {
                includes: flags[2],
                reference: flags[3],
            },
        )
    }
}

fn validate_model_commit(executable: &Path) {
    let mut sidecar: OsString = executable.as_os_str().to_owned();
    sidecar.push(".model-commit");
    let actual = fs::read_to_string(Path::new(&sidecar))
        .unwrap_or_else(|error| panic!("read Lean oracle model commit sidecar: {error}"));
    let actual = actual.trim();
    if actual == LEAN_MODEL_COMMIT {
        return;
    }
    if std::env::var_os(ALLOW_STALE_ORACLE_ENV).is_some() {
        eprintln!("warning: Lean oracle is commit {actual}, expected {LEAN_MODEL_COMMIT}");
        return;
    }
    panic!(
        "Lean oracle is commit {actual}, expected {LEAN_MODEL_COMMIT}; \
         set {ALLOW_STALE_ORACLE_ENV}=1 to override deliberately"
    );
}

impl Drop for LeanOracle {
    fn drop(&mut self) {
        let _ = writeln!(self.stdin, "quit");
        let _ = self.stdin.flush();
        let _ = self.child.wait();
    }
}
