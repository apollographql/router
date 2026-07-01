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

/// Pull a component from an OCI registry.
///
/// TODO(wasm-components): wire this to `crate::registry` (the same machinery the router uses to pull
/// supergraph artifacts from OCI). The registry's current entry points are schema-oriented, so
/// pulling an arbitrary wasm layer needs a small dedicated path; tracked as a follow-up. Until then
/// operators use `path:` with an artifact they have fetched.
async fn load_from_oci(reference: &str) -> Result<Vec<u8>, BoxError> {
    Err(format!(
        "loading wasm components from OCI (`{reference}`) is not yet implemented; \
         use `source.path` with a locally-fetched artifact for now"
    )
    .into())
}
