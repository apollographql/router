use std::alloc::{GlobalAlloc, Layout};
use std::cell::Cell;
use std::ffi::CStr;
use std::future::Future;
use std::pin::Pin;
use std::ptr::NonNull;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::task::{Context, Poll};

#[cfg(feature = "dhat-heap")]
use parking_lot::Mutex;

/// Thread-local allocation statistics that can be shared across threads.
///
/// Supports nested tracking where allocations in a child context are also tracked
/// in all parent contexts up to the root. Uses loop unwrapping rather than recursion
/// for performance.
///
/// Uses AtomicUsize for all fields to allow lock-free concurrent access from multiple threads
/// that share the same Arc<AllocationStats>. This is critical for performance in the global
/// allocator hot path where even an uncontended Mutex would add significant overhead.
#[derive(Debug)]
pub(crate) struct AllocationStats {
    /// Context name used for metric labeling
    name: &'static str,
    /// Parent context for nested tracking (None for root)
    parent: Option<Arc<AllocationStats>>,
    bytes_allocated: AtomicUsize,
    bytes_deallocated: AtomicUsize,
    bytes_zeroed: AtomicUsize,
    bytes_reallocated: AtomicUsize,
}

impl AllocationStats {
    /// Create a new root allocation stats context with the given name.
    fn new(name: &'static str) -> Self {
        Self {
            name,
            parent: None,
            bytes_allocated: AtomicUsize::new(0),
            bytes_deallocated: AtomicUsize::new(0),
            bytes_zeroed: AtomicUsize::new(0),
            bytes_reallocated: AtomicUsize::new(0),
        }
    }

    /// Create a new child allocation stats context that tracks to a parent.
    fn with_parent(name: &'static str, parent: Arc<AllocationStats>) -> Self {
        Self {
            name,
            parent: Some(parent),
            bytes_allocated: AtomicUsize::new(0),
            bytes_deallocated: AtomicUsize::new(0),
            bytes_zeroed: AtomicUsize::new(0),
            bytes_reallocated: AtomicUsize::new(0),
        }
    }

    /// Get the context name for this allocation stats.
    #[inline]
    pub(crate) fn name(&self) -> &'static str {
        self.name
    }

    /// Get the parent context, if any.
    #[inline]
    #[allow(dead_code)]
    pub(crate) fn parent(&self) -> Option<&Arc<AllocationStats>> {
        self.parent.as_ref()
    }

    /// Get the root context by traversing up the parent chain.
    /// Returns self if this is already a root context.
    #[inline]
    #[allow(dead_code)]
    pub(crate) fn root(&self) -> &Self {
        let mut current = self;
        while let Some(parent) = &current.parent {
            current = parent.as_ref();
        }
        current
    }

    /// Track allocation in this context and all parent contexts.
    /// Uses loop unwrapping instead of recursion for performance.
    #[inline]
    fn track_alloc(&self, size: usize) {
        let mut current = Some(self);
        while let Some(stats) = current {
            stats.bytes_allocated.fetch_add(size, Ordering::Relaxed);
            current = stats.parent.as_ref().map(|p| p.as_ref());
        }
    }

    /// Track deallocation in this context and all parent contexts.
    /// Uses loop unwrapping instead of recursion for performance.
    #[inline]
    fn track_dealloc(&self, size: usize) {
        let mut current = Some(self);
        while let Some(stats) = current {
            stats.bytes_deallocated.fetch_add(size, Ordering::Relaxed);
            current = stats.parent.as_ref().map(|p| p.as_ref());
        }
    }

    /// Track zeroed allocation in this context and all parent contexts.
    /// Uses loop unwrapping instead of recursion for performance.
    #[inline]
    fn track_zeroed(&self, size: usize) {
        let mut current = Some(self);
        while let Some(stats) = current {
            stats.bytes_zeroed.fetch_add(size, Ordering::Relaxed);
            current = stats.parent.as_ref().map(|p| p.as_ref());
        }
    }

    /// Track reallocation in this context and all parent contexts.
    /// Uses loop unwrapping instead of recursion for performance.
    #[inline]
    fn track_realloc(&self, size: usize) {
        let mut current = Some(self);
        while let Some(stats) = current {
            stats.bytes_reallocated.fetch_add(size, Ordering::Relaxed);
            current = stats.parent.as_ref().map(|p| p.as_ref());
        }
    }

    /// Get the current number of bytes allocated.
    #[inline]
    pub(crate) fn bytes_allocated(&self) -> usize {
        self.bytes_allocated.load(Ordering::Relaxed)
    }

    /// Get the current number of bytes deallocated.
    #[inline]
    pub(crate) fn bytes_deallocated(&self) -> usize {
        self.bytes_deallocated.load(Ordering::Relaxed)
    }

    /// Get the current number of bytes allocated with zeroing.
    #[inline]
    pub(crate) fn bytes_zeroed(&self) -> usize {
        self.bytes_zeroed.load(Ordering::Relaxed)
    }

    /// Get the current number of bytes reallocated.
    #[inline]
    pub(crate) fn bytes_reallocated(&self) -> usize {
        self.bytes_reallocated.load(Ordering::Relaxed)
    }

    /// Get the current net allocated bytes (allocated + zeroed - deallocated).
    #[inline]
    #[allow(dead_code)]
    pub(crate) fn net_allocated(&self) -> usize {
        let allocated = self.bytes_allocated();
        let zeroed = self.bytes_zeroed();
        let deallocated = self.bytes_deallocated();
        allocated.saturating_add(zeroed).saturating_sub(deallocated)
    }
}

// Thread-local to track the current task's allocation stats.
//
// ## Why Cell<Option<NonNull<T>>> instead of Cell<Option<Arc<T>>> or Mutex<Option<Arc<T>>>?
//
// We use a NonNull pointer instead of Arc because:
//
// 1. **Cell requires Copy**: Cell::get() requires T: Copy, but Arc<T> is not Copy
//    because it has a Drop implementation for reference counting.
//
// 2. **TLS destructors conflict with global allocators**: If we stored Option<Arc<T>>
//    in the thread-local, its Drop implementation would run when the thread exits.
//    This Drop could call the allocator (to deallocate the Arc), causing a fatal
//    reentrancy error: "the global allocator may not use TLS with destructors".
//
// 3. **Cell is faster than Mutex**: Cell has zero overhead (just a memory read/write),
//    while Mutex requires atomic operations and potential thread parking. Since we
//    access this on every allocation, performance is critical.
//
// ## Safety invariants:
//
// - The NonNull pointer is only valid while a MemoryTrackedFuture holding the corresponding
//   Arc is on the call stack (either in poll() or with_memory_tracking()).
// - We manually manage Arc reference counts when propagating across tasks.
// - The pointer always points to valid AllocationStats when Some.
thread_local! {
    static CURRENT_TASK_STATS: Cell<Option<NonNull<AllocationStats>>> = const { Cell::new(None) };
}

/// Future wrapper that tracks memory allocations for a task.
pub(crate) struct MemoryTrackedFuture<F> {
    inner: F,
    stats: Arc<AllocationStats>,
}

impl<F: Future> Future for MemoryTrackedFuture<F> {
    type Output = F::Output;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        // SAFETY: We're using get_unchecked_mut which is safe because:
        // 1. We never move the inner future - we only create a new Pin to it
        // 2. The MemoryTrackedFuture struct itself doesn't have any Drop glue that
        //    would be affected by mutation
        let this = unsafe { self.get_unchecked_mut() };

        // SAFETY: The inner future hasn't moved - it's still in the same memory location.
        // We're just creating a Pin pointing to it.
        let inner = unsafe { Pin::new_unchecked(&mut this.inner) };

        // Set the current task's stats for this thread (as NonNull pointer).
        // SAFETY: Arc::as_ptr gives us a non-null pointer without affecting the reference count.
        // The pointer remains valid because `this.stats` is alive for the duration of poll().
        let stats_ptr = unsafe { NonNull::new_unchecked(Arc::as_ptr(&this.stats) as *mut _) };
        let previous = CURRENT_TASK_STATS.with(|cell| cell.replace(Some(stats_ptr)));

        // Poll the inner future
        let result = inner.poll(cx);

        // Restore the previous stats (for nested tracking contexts)
        CURRENT_TASK_STATS.with(|cell| cell.set(previous));

        result
    }
}

/// Get the current task's allocation stats, if available.
/// Returns None if called outside a memory-tracked context.
///
/// This function clones the Arc (by reconstructing it from the raw pointer and incrementing
/// the reference count), so the caller owns a new reference to the stats.
#[must_use]
pub(crate) fn current() -> Option<Arc<AllocationStats>> {
    CURRENT_TASK_STATS.with(|cell| {
        cell.get().map(|ptr| {
            // SAFETY: The pointer is valid because it was set by MemoryTrackedFuture::poll()
            // or with_memory_tracking(), which are both on the call stack. We manually
            // increment the reference count and reconstruct an Arc to return a new owned reference.
            unsafe {
                Arc::increment_strong_count(ptr.as_ptr());
                Arc::from_raw(ptr.as_ptr())
            }
        })
    })
}

/// Run a synchronous closure with memory tracking.
/// If a parent context exists, creates a child context that tracks to the parent.
/// If no parent exists, creates a new root context with the given name.
/// This is useful for tracking allocations in synchronous code or threads.
#[allow(dead_code)]
pub(crate) fn with_memory_tracking<F, R>(name: &'static str, f: F) -> R
where
    F: FnOnce() -> R,
{
    // Check if there's a parent context, and create either a child or root stats
    let stats = CURRENT_TASK_STATS.with(|cell| {
        cell.get().map_or_else(
            // No parent context - create a new root
            || Arc::new(AllocationStats::new(name)),
            |ptr| {
                // Parent context exists - create a child that tracks to the parent
                // SAFETY: The pointer is valid because it's managed by a parent context.
                // We clone the Arc by manually incrementing the reference count.
                let parent = unsafe {
                    Arc::increment_strong_count(ptr.as_ptr());
                    Arc::from_raw(ptr.as_ptr())
                };
                Arc::new(AllocationStats::with_parent(name, parent))
            },
        )
    });

    with_explicit_memory_tracking(stats, f)
}

/// Run a synchronous closure with memory tracking using an explicit parent.
/// Creates a child context with the given name that tracks to the provided parent.
pub(crate) fn with_parented_memory_tracking<F, R>(
    name: &'static str,
    parent: Arc<AllocationStats>,
    f: F,
) -> R
where
    F: FnOnce() -> R,
{
    let stats = Arc::new(AllocationStats::with_parent(name, parent));
    with_explicit_memory_tracking(stats, f)
}

/// Internal function to run a closure with explicit allocation stats.
/// Sets the thread-local stats, runs the closure, and restores the previous stats.
fn with_explicit_memory_tracking<F, R>(stats: Arc<AllocationStats>, f: F) -> R
where
    F: FnOnce() -> R,
{
    // Set the current task's stats for this thread (as NonNull pointer)
    // SAFETY: Arc::as_ptr never returns null
    let stats_ptr = unsafe { NonNull::new_unchecked(Arc::as_ptr(&stats) as *mut _) };
    let previous = CURRENT_TASK_STATS.with(|cell| cell.replace(Some(stats_ptr)));

    // Run the closure
    let result = f();

    // Restore the previous stats
    CURRENT_TASK_STATS.with(|cell| cell.set(previous));

    result
}

/// Trait to add memory tracking to futures.
pub(crate) trait WithMemoryTracking: Future + Sized {
    /// Wraps this future to track memory allocations with a named context.
    /// If a parent context exists, creates a child context that tracks to the parent.
    /// If no parent exists, creates a new root context with the given name.
    fn with_memory_tracking(self, name: &'static str) -> MemoryTrackedFuture<Self>;
}

impl<F: Future> WithMemoryTracking for F {
    fn with_memory_tracking(self, name: &'static str) -> MemoryTrackedFuture<Self> {
        // Check if there's a parent context, and create either a child or root stats
        let stats = CURRENT_TASK_STATS.with(|cell| {
            cell.get().map_or_else(
                // No parent context - create a new root
                || Arc::new(AllocationStats::new(name)),
                |ptr| {
                    // Parent context exists - create a child that tracks to the parent
                    // SAFETY: The pointer is valid because it's managed by a parent MemoryTrackedFuture.
                    // We clone the Arc by manually incrementing the reference count.
                    let parent = unsafe {
                        Arc::increment_strong_count(ptr.as_ptr());
                        Arc::from_raw(ptr.as_ptr())
                    };
                    Arc::new(AllocationStats::with_parent(name, parent))
                },
            )
        });

        MemoryTrackedFuture { inner: self, stats }
    }
}

/// Custom allocator wrapper that delegates to tikv-jemalloc.
/// This allows for custom allocation tracking and instrumentation
/// while still using jemalloc as the underlying allocator.
///
/// The allocator hooks into allocation/deallocation to track memory usage
/// per-task via thread-local storage. This adds minimal overhead (~1-2ns per
/// allocation) compared to using jemalloc directly.
struct CustomAllocator {
    inner: tikv_jemallocator::Jemalloc,
}

impl CustomAllocator {
    const fn new() -> Self {
        Self {
            inner: tikv_jemallocator::Jemalloc,
        }
    }
}

// SAFETY: All methods below properly delegate to jemalloc and only add tracking
// on top. The tracking uses thread-locals with raw pointers to avoid TLS destructor
// issues (see CURRENT_TASK_STATS documentation above).
unsafe impl GlobalAlloc for CustomAllocator {
    #[inline]
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        unsafe {
            let ptr = self.inner.alloc(layout);
            if !ptr.is_null() {
                // Track to the current task's stats if available.
                // SAFETY: The pointer was set by MemoryTrackedFuture::poll() or
                // with_memory_tracking(), and is guaranteed to be valid during the
                // execution of the tracked future/closure. We only dereference it to
                // call track_alloc(), which is safe because AllocationStats uses
                // AtomicUsize internally (no Drop, no allocation).
                CURRENT_TASK_STATS.with(|cell| {
                    if let Some(stats_ptr) = cell.get() {
                        stats_ptr.as_ref().track_alloc(layout.size());
                    }
                });
            }
            ptr
        }
    }

    #[inline]
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe {
            self.inner.dealloc(ptr, layout);
            // Track to the current task's stats if available
            CURRENT_TASK_STATS.with(|cell| {
                if let Some(stats_ptr) = cell.get() {
                    stats_ptr.as_ref().track_dealloc(layout.size());
                }
            });
        }
    }

    #[inline]
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        unsafe {
            let ptr = self.inner.alloc_zeroed(layout);
            if !ptr.is_null() {
                // Track to the current task's stats if available
                CURRENT_TASK_STATS.with(|cell| {
                    if let Some(stats_ptr) = cell.get() {
                        stats_ptr.as_ref().track_zeroed(layout.size());
                    }
                });
            }
            ptr
        }
    }

    #[inline]
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        unsafe {
            let new_ptr = self.inner.realloc(ptr, layout, new_size);
            if !new_ptr.is_null() {
                // Track to the current task's stats if available
                CURRENT_TASK_STATS.with(|cell| {
                    if let Some(stats_ptr) = cell.get() {
                        stats_ptr.as_ref().track_realloc(new_size);
                    }
                });
            }
            new_ptr
        }
    }
}

#[cfg(all(feature = "global-allocator", not(feature = "dhat-heap"), unix))]
#[global_allocator]
static ALLOC: CustomAllocator = CustomAllocator::new();

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
    let ptr: *const libc::c_char = data.as_ptr();
    let output: &'static libc::c_char = &*ptr;
    output
});

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::CStr;
    use std::thread;
    use tokio::task;

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

    #[tokio::test]
    async fn test_async_memory_tracking() {
        // Test that allocations within a memory-tracked async context are tracked
        let result = async {
            let _v = Vec::<u8>::with_capacity(10000);
            current().expect("stats should be set")
        }
        .with_memory_tracking("test")
        .await;

        // Verify context name
        assert_eq!(result.name(), "test");

        // The allocator may allocate more than requested due to alignment, overhead, etc.
        // We check that at least the requested amount was allocated.
        assert!(
            result.bytes_allocated() >= 10000,
            "should track at least 10000 bytes, got {}",
            result.bytes_allocated()
        );

        // Net allocated should be 0 or close to 0 since the Vec was dropped
        assert!(
            result.net_allocated() < 100,
            "net allocated should be near 0 after Vec is dropped, got {}",
            result.net_allocated()
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn test_spawned_task_memory_tracking() {
        // Test that memory tracking creates child contexts that propagate to parent
        async {
            let parent_stats = current().expect("stats should be set in parent");
            assert_eq!(parent_stats.name(), "parent");

            // Wrap the future BEFORE spawning to create a child context
            let child_future = async {
                let child_stats = current().expect("stats should be set in child");
                assert_eq!(child_stats.name(), "child");
                let _v = Vec::<u8>::with_capacity(5000);
            }
            .with_memory_tracking("child");

            task::spawn(child_future).await.unwrap();

            let final_stats = current().expect("stats should still be set");

            // The child's allocations should be tracked to the parent as well
            assert!(
                final_stats.bytes_allocated() >= 5000,
                "child task allocations should be tracked in parent, got {}",
                final_stats.bytes_allocated()
            );

            assert!(
                Arc::ptr_eq(&parent_stats, &final_stats),
                "should be the same Arc"
            );
        }
        .with_memory_tracking("parent")
        .await;
    }

    #[test]
    fn test_sync_memory_tracking() {
        // Test that synchronous code can use with_memory_tracking for thread propagation
        let stats = with_memory_tracking("sync_test", || {
            let stats = current().expect("stats should be set");
            assert_eq!(stats.name(), "sync_test");

            {
                let _v = Vec::<u8>::with_capacity(8000);

                // The allocator may allocate more than requested due to alignment, overhead, etc.
                assert!(
                    stats.bytes_allocated() >= 8000,
                    "should track at least 8000 bytes, got {}",
                    stats.bytes_allocated()
                );
            }

            // Net should be near 0 after first Vec is dropped
            assert!(
                stats.net_allocated() < 100,
                "net allocated should be near 0 after Vec is dropped, got {}",
                stats.net_allocated()
            );

            let first_allocated = stats.bytes_allocated();

            // Test propagation to child thread with parented context
            let parent_stats = stats.clone();
            let handle = thread::spawn(move || {
                with_parented_memory_tracking("sync_test_child", parent_stats, || {
                    let child_stats = current().expect("child stats should be set");
                    assert_eq!(child_stats.name(), "sync_test_child");
                    let _v = Vec::<u8>::with_capacity(3000);
                })
            });
            handle.join().unwrap();

            // Should have tracked allocations from both contexts (parent propagation)
            assert!(
                stats.bytes_allocated() >= first_allocated + 3000,
                "should track allocations from both contexts, got {} (expected at least {})",
                stats.bytes_allocated(),
                first_allocated + 3000
            );

            stats
        });

        // Net should be near 0 after all Vecs are dropped
        // Allow up to 200 bytes for internal allocations (Arc overhead, thread infrastructure, etc.)
        assert!(
            stats.net_allocated() < 200,
            "net allocated should be near 0 after all Vecs are dropped, got {}",
            stats.net_allocated()
        );
    }

    #[tokio::test]
    async fn test_nested_memory_tracking() {
        // Test that nested contexts track allocations to all parent contexts
        async {
            let root_stats = current().expect("root stats should be set");
            assert_eq!(root_stats.name(), "root");
            let _root_vec = Vec::<u8>::with_capacity(1000);

            // Create a child context
            async {
                let child_stats = current().expect("child stats should be set");
                assert_eq!(child_stats.name(), "child");
                let _child_vec = Vec::<u8>::with_capacity(2000);

                // Child allocations should be in child stats
                assert!(
                    child_stats.bytes_allocated() >= 2000,
                    "child should track its own allocations, got {}",
                    child_stats.bytes_allocated()
                );

                // Create a grandchild context
                async {
                    let grandchild_stats = current().expect("grandchild stats should be set");
                    assert_eq!(grandchild_stats.name(), "grandchild");
                    let _grandchild_vec = Vec::<u8>::with_capacity(3000);

                    // Grandchild allocations should be in grandchild stats
                    assert!(
                        grandchild_stats.bytes_allocated() >= 3000,
                        "grandchild should track its own allocations, got {}",
                        grandchild_stats.bytes_allocated()
                    );
                }
                .with_memory_tracking("grandchild")
                .await;

                // After grandchild completes, child should have tracked grandchild's allocations
                assert!(
                    child_stats.bytes_allocated() >= 5000,
                    "child should track child + grandchild allocations, got {}",
                    child_stats.bytes_allocated()
                );
            }
            .with_memory_tracking("child")
            .await;

            // After child completes, root should have tracked all allocations
            assert!(
                root_stats.bytes_allocated() >= 6000,
                "root should track root + child + grandchild allocations, got {}",
                root_stats.bytes_allocated()
            );
        }
        .with_memory_tracking("root")
        .await;
    }

    #[test]
    fn test_dealloc_tracking() {
        let stats = with_memory_tracking("dealloc_test", || {
            let _v = Vec::<u8>::with_capacity(1000);
            current().expect("stats should be set")
        });

        assert_eq!(stats.bytes_deallocated(), 1000);
    }

    #[test]
    fn test_zeroed_tracking() {
        let stats = with_memory_tracking("zeroed_test", || {
            unsafe {
                let layout = Layout::new::<u64>();
                let ptr = std::alloc::alloc_zeroed(layout);

                std::alloc::dealloc(ptr, layout);
            }
            current().expect("stats should be set")
        });

        assert_eq!(stats.bytes_zeroed(), 8);
    }

    #[test]
    fn test_realloc_tracking() {
        let stats = with_memory_tracking("realloc_test", || {
            let layout = Layout::array::<u32>(4).unwrap();

            unsafe {
                let ptr = std::alloc::alloc(layout);

                let new_size = 8 * std::mem::size_of::<u32>();
                let new_ptr = std::alloc::realloc(ptr, layout, new_size);

                let final_layout = Layout::from_size_align(new_size, layout.align()).unwrap();
                std::alloc::dealloc(new_ptr, final_layout);
            }
            current().expect("stats should be set")
        });

        assert_eq!(stats.bytes_reallocated(), 32);
    }
}
