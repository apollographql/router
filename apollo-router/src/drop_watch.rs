//! Provide a [`DropWatch`] utility.
//!
//! Accept a FnOnce and carefully watch over it until we are dropped.
//!
//! Useful when we want to join threads which will ultimately be terminated
//! within an asynchronous context.

use std::mem::ManuallyDrop;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::thread::JoinHandle;

pub(crate) struct DropWatch {
    watcher_handle: ManuallyDrop<JoinHandle<()>>,
    park_flag: Arc<AtomicBool>,
}

impl DropWatch {
    /// Create a DropWatch which will execute the supplied closure then wait for notification
    pub(crate) fn run_and_wait<F: FnOnce() + Send + 'static>(watched: F) -> Self {
        let park_flag = Arc::new(AtomicBool::new(false));
        let watching_flag = park_flag.clone();
        let watcher_handle = std::thread::spawn(move || {
            watched();
            // Park the thread until this instance is dropped (see Drop impl)
            // We may actually unpark() before this code executes or exit from park() spuriously.
            // Use the watching_flag to control a loop which waits for the flag to be updated
            // from Drop.
            while !watching_flag.load(Ordering::Acquire) {
                std::thread::park();
            }
        });
        Self {
            park_flag,
            watcher_handle: ManuallyDrop::new(watcher_handle),
        }
    }

    /// Create a DropWatch which will wait for notification before executing the supplied closure
    pub(crate) fn wait_and_run<F: FnOnce() + Send + 'static>(watched: F) -> Self {
        let park_flag = Arc::new(AtomicBool::new(false));
        let watching_flag = park_flag.clone();
        let watcher_handle = std::thread::spawn(move || {
            // Park the thread until this instance is dropped (see Drop impl)
            // We may actually unpark() before this code executes or exit from park() spuriously.
            // Use the watching_flag to control a loop which waits for the flag to be updated
            // from Drop.
            while !watching_flag.load(Ordering::Acquire) {
                std::thread::park();
            }
            watched();
        });
        Self {
            park_flag,
            watcher_handle: ManuallyDrop::new(watcher_handle),
        }
    }
}

impl Drop for DropWatch {
    fn drop(&mut self) {
        // Safety: watcher_handle is taken, and must not be accessed again
        // Since this is in Drop::drop, it is not possible to access it afterwards.
        // https://doc.rust-lang.org/stable/std/mem/struct.ManuallyDrop.html#method.into_inner
        let wh = unsafe { ManuallyDrop::<JoinHandle<()>>::take(&mut self.watcher_handle) };
        self.park_flag.store(true, Ordering::Release);
        wh.thread().unpark();
        wh.join().expect("drop watcher thread terminating");
    }
}
