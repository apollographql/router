#[cfg(feature = "dhat-heap")]
use parking_lot::Mutex;

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

union U {
    x: &'static u8,
    y: &'static libc::c_char,
}

#[allow(non_upper_case_globals)]
#[unsafe(export_name = "_rjem_malloc_conf")]
static malloc_conf: Option<&'static libc::c_char> = Some(unsafe {
    U {
        x: &b"prof:true,prof_active:false\0"[0],
    }
    .y
});
