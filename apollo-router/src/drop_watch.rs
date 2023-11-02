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

#[cfg(test)]
mod test {
    use std::sync::atomic::{AtomicU8, Ordering};
    use std::sync::mpsc;
    use std::sync::Arc;

    use super::DropWatch;

    #[test]
    fn it_runs_and_waits_when_dropped() {
        let value = Arc::new(AtomicU8::new(0));
        let shared_value = value.clone();
        let (tx, rx) = mpsc::channel();

        let _ = DropWatch::run_and_wait(move || {
            shared_value.fetch_add(1, Ordering::Relaxed);
            tx.send(()).unwrap();
        });
        let _ = rx.recv();
        assert_eq!(value.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn it_runs_and_waits_when_alive() {
        let value = Arc::new(AtomicU8::new(0));
        let shared_value = value.clone();
        let (tx, rx) = mpsc::channel();

        let watcher = DropWatch::run_and_wait(move || {
            shared_value.fetch_add(1, Ordering::Relaxed);
            tx.send(()).unwrap();
        });
        let _ = rx.recv();
        assert_eq!(value.load(Ordering::Relaxed), 1);
        drop(watcher);
        assert_eq!(value.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn it_waits_and_runs_when_dropped() {
        let value = Arc::new(AtomicU8::new(0));
        let shared_value = value.clone();
        let (tx, rx) = mpsc::channel();

        let _ = DropWatch::wait_and_run(move || {
            shared_value.fetch_add(1, Ordering::Relaxed);
            tx.send(()).unwrap();
        });
        let _ = rx.recv();
        assert_eq!(value.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn it_waits_and_runs_when_alive() {
        let value = Arc::new(AtomicU8::new(0));
        let shared_value = value.clone();
        let (tx, rx) = mpsc::channel();

        let watcher = DropWatch::wait_and_run(move || {
            shared_value.fetch_add(1, Ordering::Relaxed);
            tx.send(()).unwrap();
        });
        assert_eq!(value.load(Ordering::Relaxed), 0);
        drop(watcher);
        let _ = rx.recv();
        assert_eq!(value.load(Ordering::Relaxed), 1);
    }
}
