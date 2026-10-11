//! Heap profiles from the router's own jemalloc, shared by the diagnostics plugin and continuous
//! profiling.
//!
//! The router's global allocator is jemalloc, built with profiling support and started with
//! `prof:true,prof_active:false` (see `allocator.rs`). While sampling is on, jemalloc samples
//! allocations (by default about one per 512KiB allocated) and records their call stacks. A dump
//! of those samples is an in-use heap profile, which we symbolize in-process and encode as gzipped
//! pprof, so it opens in standard tools without the router binary.
//!
//! Sampling is process-wide, so it is owned here rather than by any one user: each user holds a
//! [`Sampling`] guard, and sampling stays on while any guard is held.

pub(crate) use imp::*;

/// Held by tests that start sampling or depend on whether it's on, so that they don't race under
/// plain `cargo test`, which runs a binary's tests on threads of one process. (nextest runs each
/// test in its own process, so it doesn't need this.)
#[cfg(all(test, feature = "global-allocator", not(feature = "dhat-heap"), unix))]
pub(crate) static TEST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

#[cfg(all(feature = "global-allocator", not(feature = "dhat-heap"), unix))]
mod imp {
    use parking_lot::Mutex;
    use tower::BoxError;

    /// pprof sample type for in-use heap. The name matches what Datadog's native profiler
    /// (ddprof) reports for live heap, so profiles land in the same place in Datadog's UI.
    const SAMPLE_TYPE: (&str, &str) = ("inuse-space", "bytes");

    struct SamplingState {
        /// Number of live [`Sampling`] guards.
        holders: usize,
        /// Whether sampling was already on (e.g. via `MALLOC_CONF`) when the first guard was
        /// taken, in which case we leave it on when the last one is dropped.
        was_on: bool,
    }

    static SAMPLING: Mutex<SamplingState> = Mutex::new(SamplingState {
        holders: 0,
        was_on: false,
    });

    /// Keeps jemalloc's allocation sampling on while held.
    pub(crate) struct Sampling(());

    impl Drop for Sampling {
        fn drop(&mut self) {
            let mut state = SAMPLING.lock();
            state.holders -= 1;
            if state.holders == 0
                && !state.was_on
                && let Err(err) = set_active(false)
            {
                tracing::warn!(error = %err, "failed to switch jemalloc heap profiling off");
            }
        }
    }

    fn prof_ctl() -> Result<
        &'static std::sync::Arc<tokio::sync::Mutex<jemalloc_pprof::JemallocProfCtl>>,
        BoxError,
    > {
        jemalloc_pprof::PROF_CTL.as_ref().ok_or_else(|| {
            "jemalloc was started with profiling disabled (opt.prof is false)".into()
        })
    }

    /// Whether heap profiling works in this process.
    pub(crate) fn available() -> Result<(), BoxError> {
        prof_ctl().map(|_| ())
    }

    /// Switch sampling on, if it isn't already, until the returned guard is dropped.
    pub(crate) fn start_sampling() -> Result<Sampling, BoxError> {
        available()?;
        let mut state = SAMPLING.lock();
        if state.holders == 0 {
            state.was_on = is_active()?;
            if !state.was_on {
                set_active(true)?;
            }
        }
        state.holders += 1;
        Ok(Sampling(()))
    }

    /// Whether jemalloc is sampling allocations, whoever switched it on.
    pub(crate) fn is_sampling() -> Result<bool, BoxError> {
        available()?;
        is_active()
    }

    fn is_active() -> Result<bool, BoxError> {
        // SAFETY: "prof.active" is documented as readable as a bool:
        // http://jemalloc.net/jemalloc.3.html#prof.active
        Ok(unsafe { tikv_jemalloc_ctl::raw::read(b"prof.active\0") }?)
    }

    fn set_active(active: bool) -> Result<(), BoxError> {
        // SAFETY: "prof.active" is documented as writable as a bool:
        // http://jemalloc.net/jemalloc.3.html#prof.active
        Ok(unsafe { tikv_jemalloc_ctl::raw::write(b"prof.active\0", active) }?)
    }

    /// Dump the in-use heap and encode it as symbolized, gzipped pprof.
    ///
    /// Dumping and symbolizing are synchronous and can take a while on a large heap, so this runs
    /// on the blocking pool.
    pub(crate) async fn collect_pprof() -> Result<Vec<u8>, BoxError> {
        let ctl = prof_ctl()?.clone();
        tokio::task::spawn_blocking(move || {
            let started = std::time::Instant::now();
            let profile = dump_profile(&mut ctl.blocking_lock())?;
            let pprof = profile.to_pprof(SAMPLE_TYPE, SAMPLE_TYPE, None);
            // Symbolizing parses the router binary's symbol table, and `backtrace` would otherwise
            // keep that cache (~30MiB for a release build) for the life of the process. Re-parsing
            // per profile is cheaper than holding it.
            backtrace::clear_symbol_cache();
            tracing::debug!(duration = ?started.elapsed(), "collected heap profile");
            Ok(pprof)
        })
        .await?
    }

    #[cfg(target_os = "linux")]
    fn dump_profile(
        ctl: &mut jemalloc_pprof::JemallocProfCtl,
    ) -> Result<jemalloc_pprof::StackProfile, BoxError> {
        Ok(ctl.dump_profile()?)
    }

    /// Like Linux, but without the shared object mappings (and their build IDs), which the
    /// `mappings` crate only reads on Linux and otherwise logs an error for. We symbolize
    /// in-process, so profiles don't need them.
    #[cfg(not(target_os = "linux"))]
    fn dump_profile(
        ctl: &mut jemalloc_pprof::JemallocProfCtl,
    ) -> Result<jemalloc_pprof::StackProfile, BoxError> {
        let dump = std::io::BufReader::new(ctl.dump()?);
        Ok(pprof_util::parse_jeheap(dump, None)?)
    }

    /// Decodes the profiles [`collect_pprof`] produces, for tests.
    #[cfg(test)]
    pub(crate) mod test_pprof {
        use std::io::Read;

        /// Just the parts of pprof's `Profile` message tests look at.
        #[derive(Clone, PartialEq, prost::Message)]
        pub(crate) struct Profile {
            #[prost(message, repeated, tag = "1")]
            sample_type: Vec<ValueType>,
            #[prost(message, repeated, tag = "2")]
            sample: Vec<Sample>,
            #[prost(string, repeated, tag = "6")]
            string_table: Vec<String>,
        }

        #[derive(Clone, PartialEq, prost::Message)]
        struct ValueType {
            #[prost(int64, tag = "1")]
            r#type: i64,
            #[prost(int64, tag = "2")]
            unit: i64,
        }

        #[derive(Clone, PartialEq, prost::Message)]
        struct Sample {
            #[prost(int64, repeated, tag = "2")]
            value: Vec<i64>,
        }

        impl Profile {
            pub(crate) fn decode_gzipped(gzipped: &[u8]) -> Self {
                let mut pprof = Vec::new();
                flate2::read::GzDecoder::new(gzipped)
                    .read_to_end(&mut pprof)
                    .expect("gzipped pprof");
                <Self as prost::Message>::decode(pprof.as_slice()).expect("pprof")
            }

            /// The (type, unit) of the first sample type
            pub(crate) fn sample_type(&self) -> (&str, &str) {
                let sample_type = &self.sample_type[0];
                (
                    &self.string_table[sample_type.r#type as usize],
                    &self.string_table[sample_type.unit as usize],
                )
            }

            /// Total of the first value across samples
            pub(crate) fn total(&self) -> i64 {
                self.sample.iter().map(|s| s.value[0]).sum()
            }

            /// Whether a symbolized function name ends with `suffix`
            pub(crate) fn has_function(&self, suffix: &str) -> bool {
                self.string_table.iter().any(|s| s.ends_with(suffix))
            }
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use crate::heap_profiling::TEST_LOCK;

        #[tokio::test]
        async fn sampling_stays_on_while_any_guard_is_held() {
            let _lock = TEST_LOCK.lock().await;
            assert!(!is_sampling().unwrap(), "sampling starts off");

            let first = start_sampling().unwrap();
            let second = start_sampling().unwrap();
            assert!(is_sampling().unwrap());
            drop(first);
            assert!(is_sampling().unwrap(), "the second guard keeps sampling on");
            drop(second);
            assert!(
                !is_sampling().unwrap(),
                "the last guard switches sampling off"
            );
        }

        #[tokio::test]
        async fn sampling_switched_on_elsewhere_is_left_on() {
            let _lock = TEST_LOCK.lock().await;
            set_active(true).unwrap();

            drop(start_sampling().unwrap());
            assert!(is_sampling().unwrap());

            set_active(false).unwrap();
        }
    }
}

#[cfg(not(all(feature = "global-allocator", not(feature = "dhat-heap"), unix)))]
mod imp {
    use tower::BoxError;

    const UNSUPPORTED: &str =
        "heap profiling requires a Unix platform and the router's jemalloc global allocator";

    pub(crate) struct Sampling(());

    pub(crate) fn available() -> Result<(), BoxError> {
        Err(UNSUPPORTED.into())
    }

    pub(crate) fn start_sampling() -> Result<Sampling, BoxError> {
        Err(UNSUPPORTED.into())
    }

    pub(crate) fn is_sampling() -> Result<bool, BoxError> {
        Err(UNSUPPORTED.into())
    }

    pub(crate) async fn collect_pprof() -> Result<Vec<u8>, BoxError> {
        Err(UNSUPPORTED.into())
    }
}
