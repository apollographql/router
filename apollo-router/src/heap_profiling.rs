//! Heap profiles from the router's own jemalloc, for the diagnostics plugin and anything else in
//! the router that profiles the heap.
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

/// An in-use heap profile.
pub(crate) struct HeapProfile {
    /// Symbolized, gzipped pprof
    pub(crate) pprof: Vec<u8>,
    /// Number of sampled call stacks. Zero means nothing allocated since sampling started is
    /// still in use, often because sampling only just started.
    pub(crate) samples: usize,
}

/// Held by tests that start sampling or depend on whether it's on, so that they don't race under
/// plain `cargo test`, which runs a binary's tests on threads of one process. (nextest runs each
/// test in its own process, so it doesn't need this.)
#[cfg(test)]
pub(crate) static TEST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

#[cfg(all(feature = "global-allocator", not(feature = "dhat-heap"), unix))]
mod imp {
    use parking_lot::Mutex;
    use tower::BoxError;

    /// pprof sample type for in-use heap: the name native heap profilers (e.g. ddprof) use for
    /// live heap, so profiling backends show these profiles alongside their own.
    const SAMPLE_TYPE: (&str, &str) = ("inuse-space", "bytes");

    struct SamplingState {
        /// Number of live [`Sampling`] guards.
        holders: usize,
        /// Whether sampling was already on (e.g. via `_RJEM_MALLOC_CONF`) when the first guard was
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
    pub(crate) async fn collect_pprof() -> Result<super::HeapProfile, BoxError> {
        let ctl = prof_ctl()?.clone();
        tokio::task::spawn_blocking(move || {
            let started = std::time::Instant::now();
            let (dump, dumped_to) = dump(&mut ctl.blocking_lock())?;
            let dump_bytes = dump.metadata()?.len();
            let profile = parse(dump)?;
            let pprof = profile.to_pprof(SAMPLE_TYPE, SAMPLE_TYPE, None);
            // Symbolizing parses the router binary's symbol table, and `backtrace` would otherwise
            // keep that cache (~30MiB for a release build) for the life of the process. Re-parsing
            // per profile is cheaper than holding it.
            backtrace::clear_symbol_cache();
            let samples = profile.stacks.len();
            tracing::debug!(
                duration = ?started.elapsed(),
                samples,
                dump_bytes,
                dumped_to,
                "collected heap profile"
            );
            Ok(super::HeapProfile { pprof, samples })
        })
        .await?
    }

    /// Have jemalloc write its sampled heap, returning the dump and where it went. jemalloc can only
    /// dump to a file, so on Linux that's an in-memory one: dumping then needs no writable
    /// filesystem (e.g. under a read-only root filesystem). Elsewhere, or if an in-memory file
    /// can't be opened, it's a temporary file in `TMPDIR`.
    fn dump(
        ctl: &mut jemalloc_pprof::JemallocProfCtl,
    ) -> Result<(std::fs::File, &'static str), BoxError> {
        #[cfg(target_os = "linux")]
        match memfd::open() {
            Ok((file, path)) => {
                // SAFETY: "prof.dump" is documented as writable and taking a C string:
                // http://jemalloc.net/jemalloc.3.html#prof.dump
                unsafe { tikv_jemalloc_ctl::raw::write(b"prof.dump\0", path.as_ptr()) }?;
                return Ok((file, "memory"));
            }
            Err(err) => {
                static WARNED: std::sync::Once = std::sync::Once::new();
                WARNED.call_once(|| {
                    tracing::warn!(
                        error = %err,
                        tmpdir = %std::env::temp_dir().display(),
                        "can't dump the heap to memory; dumping to a temporary file in TMPDIR instead"
                    );
                });
            }
        }
        Ok((ctl.dump()?, "tmpdir"))
    }

    /// Parse a jemalloc heap dump. On Linux, attach the shared object mappings (with their build
    /// IDs); the `mappings` crate only reads them there, and otherwise logs an error. We symbolize
    /// in-process, so profiles don't need them.
    fn parse(dump: std::fs::File) -> Result<jemalloc_pprof::StackProfile, BoxError> {
        #[cfg(target_os = "linux")]
        let mappings = mappings::MAPPINGS.as_deref();
        #[cfg(not(target_os = "linux"))]
        let mappings = None;
        Ok(pprof_util::parse_jeheap(
            std::io::BufReader::new(dump),
            mappings,
        )?)
    }

    /// Heap dumps into an anonymous in-memory file (`memfd_create`).
    #[cfg(target_os = "linux")]
    mod memfd {
        use std::ffi::CString;
        use std::fs::File;
        use std::io;
        use std::os::fd::AsRawFd;
        use std::os::fd::FromRawFd;

        use tower::BoxError;

        /// A new, empty in-memory file, and the path jemalloc can open it by.
        pub(super) fn open() -> Result<(File, CString), BoxError> {
            let file = create()?;
            // jemalloc opens the path it's given, so give it the file's path in /proc. Check it
            // first: jemalloc reports any failure to open it as a bare EFAULT.
            let path = format!("/proc/self/fd/{}", file.as_raw_fd());
            std::fs::metadata(&path).map_err(|err| format!("{path}: {err}"))?;
            Ok((file, CString::new(path)?))
        }

        fn create() -> io::Result<File> {
            let create = |flags: libc::c_uint| {
                // `syscall` reads its arguments as `long`s.
                // SAFETY: memfd_create takes a NUL-terminated name and flags, and returns a new
                // file descriptor or -1. Called through `syscall` so it doesn't need glibc 2.27.
                unsafe {
                    libc::syscall(
                        libc::SYS_memfd_create,
                        c"router-heap-profile".as_ptr(),
                        libc::c_ulong::from(flags),
                    )
                }
            };
            // Sealed against exec, which kernels can require (vm.memfd_noexec=2). Kernels before
            // 6.3 don't know the flag, and reject it with EINVAL.
            let mut fd = create(libc::MFD_CLOEXEC | libc::MFD_NOEXEC_SEAL);
            if fd < 0 && io::Error::last_os_error().raw_os_error() == Some(libc::EINVAL) {
                fd = create(libc::MFD_CLOEXEC);
            }
            if fd < 0 {
                return Err(io::Error::last_os_error());
            }
            // SAFETY: memfd_create returned a new file descriptor that nothing else owns.
            Ok(unsafe { File::from_raw_fd(fd as libc::c_int) })
        }
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

        /// Holds 64MiB allocated here, which should show up by name in a heap profile.
        #[inline(never)]
        pub(crate) fn allocate_for_profile() -> Vec<Vec<u8>> {
            (0..64)
                .map(|_| std::hint::black_box(vec![1u8; 1024 * 1024]))
                .collect()
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

            /// Whether a function is named `name`, with or without its module path. On macOS,
            /// `debug = "line-tables-only"` DWARF names functions without their module path, and
            /// symbolization prefers it over the symbol table.
            pub(crate) fn has_function(&self, name: &str) -> bool {
                let path_suffix = format!("::{name}");
                self.string_table
                    .iter()
                    .any(|s| s == name || s.ends_with(&path_suffix))
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

        /// On Linux, heaps are dumped to memory, so profiling works on a read-only filesystem.
        #[cfg(target_os = "linux")]
        #[tokio::test]
        async fn dumps_to_memory() {
            let _lock = TEST_LOCK.lock().await;
            let sampling = start_sampling().unwrap();
            let held = test_pprof::allocate_for_profile();

            let mut ctl = prof_ctl().unwrap().lock().await;
            let (dump, dumped_to) = dump(&mut ctl).unwrap();
            drop(ctl);
            assert_eq!(dumped_to, "memory");
            let profile = parse(dump).unwrap();
            drop(held);
            drop(sampling);

            assert!(!profile.stacks.is_empty());
            let pprof = test_pprof::Profile::decode_gzipped(&profile.to_pprof(
                SAMPLE_TYPE,
                SAMPLE_TYPE,
                None,
            ));
            assert!(pprof.has_function("allocate_for_profile"));
        }
    }
}

#[cfg(not(all(feature = "global-allocator", not(feature = "dhat-heap"), unix)))]
mod imp {
    use tower::BoxError;

    const UNSUPPORTED: &str =
        "heap profiling requires a Unix platform and the router's jemalloc global allocator";

    /// Never constructed: sampling can't start here.
    pub(crate) enum Sampling {}

    pub(crate) fn available() -> Result<(), BoxError> {
        Err(UNSUPPORTED.into())
    }

    pub(crate) fn start_sampling() -> Result<Sampling, BoxError> {
        Err(UNSUPPORTED.into())
    }

    pub(crate) fn is_sampling() -> Result<bool, BoxError> {
        Err(UNSUPPORTED.into())
    }

    pub(crate) async fn collect_pprof() -> Result<super::HeapProfile, BoxError> {
        Err(UNSUPPORTED.into())
    }
}
