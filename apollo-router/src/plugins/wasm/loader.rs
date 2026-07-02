//! Acquire a component's `.wasm` bytes from its configured source (local path or OCI).

use tower::BoxError;

use super::Source;

/// Load the component bytes for `source`. Exactly one of `path` / `oci` must be set.
pub(super) async fn load(source: &Source) -> Result<Vec<u8>, BoxError> {
    match (&source.path, &source.oci) {
        (Some(path), None) => tokio::fs::read(path)
            .await
            .map_err(|e| format!("reading wasm component at `{}`: {e}", path.display()).into()),
        (None, Some(reference)) => load_from_oci(reference).await,
        (Some(_), Some(_)) => {
            Err("wasm data source `source` must set exactly one of `path` or `oci`, not both".into())
        }
        (None, None) => {
            Err("wasm data source `source` must set either `path` or `oci`".into())
        }
    }
}

/// Pull a component from an OCI registry, reusing the router's OCI machinery
/// (`crate::registry`) — the same client, auth, and telemetry used for supergraph artifacts.
async fn load_from_oci(reference: &str) -> Result<Vec<u8>, BoxError> {
    // Validate up front for a clear error on a malformed reference.
    crate::registry::validate_oci_reference(reference)
        .map_err(|e| format!("invalid OCI reference `{reference}`: {e}"))?;
    crate::registry::fetch_oci_component(reference)
        .await
        .map_err(|e| format!("pulling wasm component from `{reference}`: {e}").into())
}
