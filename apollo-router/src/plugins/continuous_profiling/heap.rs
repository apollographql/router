//! Heap profiles from the router's own jemalloc.
//!
//! The router's global allocator is jemalloc, built with profiling support and started with
//! `prof:true,prof_active:false` (see `allocator.rs`). Activating `prof.active` makes jemalloc
//! sample allocations (by default about one per 512KiB allocated) and record their call stacks.
//! A dump of those samples is an in-use heap profile, which we symbolize in-process and encode as
//! pprof. Nothing outside the router binary is needed, so this works the same in any base image.

pub(super) use imp::*;

#[cfg(all(
    feature = "global-allocator",
    not(feature = "dhat-heap"),
    target_os = "linux"
))]
mod imp {
    use tower::BoxError;

    /// pprof sample type for in-use heap. The name matches what Datadog's native profiler
    /// (ddprof) reports for live heap, so profiles land in the same place in Datadog's UI.
    const SAMPLE_TYPE: (&str, &str) = ("inuse-space", "bytes");

    fn prof_ctl() -> Result<
        &'static std::sync::Arc<tokio::sync::Mutex<jemalloc_pprof::JemallocProfCtl>>,
        BoxError,
    > {
        jemalloc_pprof::PROF_CTL.as_ref().ok_or_else(|| {
            "jemalloc was started with profiling disabled (opt.prof is false)".into()
        })
    }

    pub(crate) fn available() -> Result<(), BoxError> {
        prof_ctl().map(|_| ())
    }

    pub(crate) fn is_active() -> Result<bool, BoxError> {
        // SAFETY: "prof.active" is documented as readable as a bool:
        // http://jemalloc.net/jemalloc.3.html#prof.active
        Ok(unsafe { tikv_jemalloc_ctl::raw::read(b"prof.active\0") }?)
    }

    pub(crate) fn set_active(active: bool) -> Result<(), BoxError> {
        // SAFETY: "prof.active" is documented as writable as a bool:
        // http://jemalloc.net/jemalloc.3.html#prof.active
        Ok(unsafe { tikv_jemalloc_ctl::raw::write(b"prof.active\0", active) }?)
    }

    /// Dump the in-use heap and encode it as gzipped pprof.
    ///
    /// Dumping and symbolizing are synchronous and can take a while on a large heap, so this runs
    /// on the blocking pool.
    pub(crate) async fn collect_pprof() -> Result<Vec<u8>, BoxError> {
        let ctl = prof_ctl()?.clone();
        tokio::task::spawn_blocking(move || {
            let started = std::time::Instant::now();
            let profile = ctl.blocking_lock().dump_profile()?;
            let pprof = profile.to_pprof(SAMPLE_TYPE, SAMPLE_TYPE, None);
            // Symbolizing parses the router binary's symbol table, and `backtrace` would otherwise
            // keep that cache (~30MiB for a release build) for the life of the process. Re-parsing
            // once per interval is cheaper than holding it.
            backtrace::clear_symbol_cache();
            tracing::debug!(duration = ?started.elapsed(), "collected heap profile");
            Ok(pprof)
        })
        .await?
    }
}

#[cfg(not(all(
    feature = "global-allocator",
    not(feature = "dhat-heap"),
    target_os = "linux"
)))]
mod imp {
    use tower::BoxError;

    const UNSUPPORTED: &str =
        "heap profiling requires Linux and the router's jemalloc global allocator";

    pub(crate) fn available() -> Result<(), BoxError> {
        Err(UNSUPPORTED.into())
    }

    pub(crate) fn is_active() -> Result<bool, BoxError> {
        Err(UNSUPPORTED.into())
    }

    pub(crate) fn set_active(_active: bool) -> Result<(), BoxError> {
        Err(UNSUPPORTED.into())
    }

    pub(crate) async fn collect_pprof() -> Result<Vec<u8>, BoxError> {
        Err(UNSUPPORTED.into())
    }
}
