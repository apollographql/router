#[cfg(feature = "dhat-heap")]
use parking_lot::Mutex;
use std::ffi::CStr;

#[cfg(all(feature = "global-allocator", not(feature = "dhat-heap"), unix))]
#[global_allocator]
static ALLOC: tikv_jemallocator::Jemalloc = tikv_jemallocator::Jemalloc;

// Note: the dhat-heap and dhat-ad-hoc features should not be both enabled. We name our functions
// and variables identically to prevent this from happening.

#[cfg(feature = "dhat-heap")]
#[global_allocator]
pub(crate) static ALLOC: dhat::Alloc = dhat::Alloc;

#[cfg(feature = "dhat-heap")]
pub(crate) static DHAT_HEAP_PROFILER: Mutex<Option<dhat::Profiler>> = Mutex::new(None);

#[cfg(feature = "dhat-ad-hoc")]
pub(crate) static DHAT_AD_HOC_PROFILER: Mutex<Option<dhat::Profiler>> = Mutex::new(None);

// Note: Constructor/Destructor functions may not play nicely with tracing, since they run after
// main completes, so don't use tracing, use println!() and eprintln!()..
#[cfg(feature = "dhat-heap")]
pub(crate) fn create_heap_profiler() {
    *DHAT_HEAP_PROFILER.lock() = Some(dhat::Profiler::new_heap());
    println!("heap profiler installed");
    unsafe { libc::atexit(drop_heap_profiler) };
}

#[cfg(feature = "dhat-heap")]
#[unsafe(no_mangle)]
extern "C" fn drop_heap_profiler() {
    if let Some(p) = DHAT_HEAP_PROFILER.lock().take() {
        drop(p);
    }
}

#[cfg(feature = "dhat-ad-hoc")]
pub(crate) fn create_ad_hoc_profiler() {
    *DHAT_AD_HOC_PROFILER.lock() = Some(dhat::Profiler::new_heap());
    println!("ad-hoc profiler installed");
    unsafe { libc::atexit(drop_ad_hoc_profiler) };
}

#[cfg(feature = "dhat-ad-hoc")]
#[unsafe(no_mangle)]
extern "C" fn drop_ad_hoc_profiler() {
    if let Some(p) = DHAT_AD_HOC_PROFILER.lock().take() {
        drop(p);
    }
}

// Enable jemalloc profiling with default settings if using jemalloc as the global allocator, however
// disable profiling by default to avoid overhead unless explicitly enabled at runtime.
#[allow(non_upper_case_globals)]
#[unsafe(export_name = "_rjem_malloc_conf")]
static malloc_conf: Option<&'static libc::c_char> = Some(unsafe {
    let data: &'static CStr = c"prof:true,prof_active:false";
    let ptr: *const i8 = data.as_ptr();
    let output: &'static libc::c_char = &*ptr;
    output
});

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::CStr;

    #[test]
    fn test_malloc_conf_is_valid_c_string() {
        // Test that malloc_conf produces a valid null-terminated C string
        if let Some(conf_ptr) = malloc_conf {
            // Safety: We know this should be a valid C string
            let c_str = unsafe { CStr::from_ptr(conf_ptr) };

            // Convert back to a Rust string to verify content
            let rust_str = c_str.to_str().expect("malloc_conf should be valid UTF-8");

            // Verify the expected content
            assert_eq!(rust_str, "prof:true,prof_active:false");

            // Verify it's null-terminated by checking the raw bytes
            let bytes = c_str.to_bytes_with_nul();
            assert!(
                bytes.ends_with(&[0u8]),
                "C string should be null-terminated"
            );
            assert_eq!(bytes, b"prof:true,prof_active:false\0");
        } else {
            panic!("malloc_conf should not be None");
        }
    }
}
