use std::path::Path;
use std::process::Command;

use crate::integration::IntegrationTest;

/// `router config validate` reports a file's automatic upgrades in its own output, with nothing
/// logged about them at any level.
#[test]
fn validate_reports_automatic_upgrades_instead_of_logging_them() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/integration/fixtures/legacy_settings.yaml");

    let output = Command::new(IntegrationTest::router_location())
        .args(["config", "validate"])
        .arg(&path)
        .output()
        .expect("router config validate runs");

    let stdout = String::from_utf8(output.stdout).expect("stdout is UTF-8");
    let stderr = String::from_utf8(output.stderr).expect("stderr is UTF-8");
    assert!(output.status.success(), "{stdout}{stderr}");
    let path = path.display();
    assert_eq!(
        stdout,
        format!(
            "Configuration at path {path} is valid.\n\
             It relies on automatic upgrades that the router applies at startup:\n  \
               - telemetry.apollo.otlp_tracing_sampler has been removed. Router 3 uses OTLP exclusively for traces. Remove this key from your config.\n  \
               - The `expose_query_plan` plugin has been promoted out of experimental in Router\n    \
                 3.0. Its configuration moves to a top-level key.\n\
             Run `router config upgrade {path}` to update the file.\n"
        )
    );
    assert_eq!(stderr, "");
}
