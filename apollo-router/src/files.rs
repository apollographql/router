use std::path::Path;
use std::path::PathBuf;
use std::time::Duration;

use futures::prelude::*;
use notify::Config;
use notify::EventKind;
use notify::PollWatcher;
use notify::RecursiveMode;
use notify::Watcher;
use notify::event::DataChange;
use notify::event::MetadataKind;
use notify::event::ModifyKind;
use tokio::sync::mpsc;
use tokio::sync::mpsc::error::TrySendError;

#[cfg(not(test))]
const DEFAULT_WATCH_DURATION: Duration = Duration::from_secs(3);

#[cfg(test)]
const DEFAULT_WATCH_DURATION: Duration = Duration::from_millis(100);

/// Creates a stream events whenever the file at the path has changes. The stream never terminates
/// and must be dropped to finish watching.
///
/// # Arguments
///
/// * `path`: The file to watch
///
/// returns: impl Stream<Item=()>
///
pub(crate) fn watch(path: &Path) -> impl Stream<Item = ()> + use<> {
    watch_with_duration(path, DEFAULT_WATCH_DURATION, |_| ())
}

/// What happened to a watcher event forwarded to the stream by [`enqueue`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Enqueued {
    /// The event is now pending on the stream.
    Sent,
    /// An event was already pending, so this one was dropped.
    AlreadyPending,
}

/// Forwards a watcher event to the stream without blocking the watcher thread.
fn enqueue(sender: &mpsc::Sender<()>) -> Enqueued {
    match sender.try_send(()) {
        Ok(()) => Enqueued::Sent,
        // If the channel is full, the receiver hasn't handled the pending event yet. When it
        // does, it re-reads the files from disk and picks up the latest contents, so it's fine
        // to drop this one: in effect, it's the same as if we had cancelled the pending event
        // and pushed a new one (#8336). There is a narrow race where a change that arrives
        // during the read itself could be missed until a subsequent edit triggers a new
        // notification.
        Err(TrySendError::Full(())) => Enqueued::AlreadyPending,
        Err(err) => panic!("event channel failed: {err}"),
    }
}

/// `on_enqueue` is called on the watcher thread with the result of each [`enqueue`].
fn watch_with_duration<H>(
    path: &Path,
    duration: Duration,
    on_enqueue: H,
) -> impl Stream<Item = ()> + use<H>
where
    H: Fn(Enqueued) + Send + 'static,
{
    // Due to the vagaries of file watching across multiple platforms, instead of watching the
    // supplied path (file), we are going to watch the parent (directory) of the path.
    let config_file_path = PathBuf::from(path);
    let watched_path = config_file_path.clone();

    let (watch_sender, watch_receiver) = mpsc::channel(1);
    let watch_receiver_stream = tokio_stream::wrappers::ReceiverStream::new(watch_receiver);
    // We can't use the recommended watcher, because there's just too much variation across
    // platforms and file systems. We use the Poll Watcher, which is implemented consistently
    // across all platforms. Less reactive than other mechanisms, but at least it's predictable
    // across all environments. We compare contents as well, which reduces false positives with
    // some additional processing burden.
    let config = Config::default()
        .with_poll_interval(duration)
        .with_compare_contents(true);
    let mut watcher = PollWatcher::new(
        move |res: Result<notify::Event, notify::Error>| match res {
            Ok(event) => {
                // The two kinds of events of interest to use are writes to the metadata of a
                // watched file and changes to the data of a watched file
                if matches!(
                    event.kind,
                    EventKind::Modify(ModifyKind::Metadata(MetadataKind::WriteTime))
                        | EventKind::Modify(ModifyKind::Data(DataChange::Any))
                ) && event.paths.contains(&watched_path)
                {
                    on_enqueue(enqueue(&watch_sender));
                }
            }
            Err(e) => tracing::error!("event error: {:?}", e),
        },
        config,
    )
    .unwrap_or_else(|_| panic!("could not create watch on: {config_file_path:?}"));
    watcher
        .watch(&config_file_path, RecursiveMode::NonRecursive)
        .unwrap_or_else(|_| panic!("could not watch: {config_file_path:?}"));
    // Tell watchers once they should read the file once,
    // then listen to fs events.
    stream::once(future::ready(()))
        .chain(watch_receiver_stream)
        .chain(stream::once(async move {
            // This exists to give the stream ownership of the hotwatcher.
            // Without it hotwatch will get dropped and the stream will terminate.
            // This code never actually gets run.
            // The ideal would be that hotwatch implements a stream and
            // therefore we don't need this hackery.
            drop(watcher);
        }))
        .boxed()
}

/// Creates a stream events whenever the path has changes. The stream never terminates
/// and must be dropped to finish watching.
///
/// # Arguments
///
/// * `path`: The path to watch
///
/// returns: impl Stream<Item=()>
///
pub(crate) fn watch_rhai(path: &Path) -> impl Stream<Item = ()> + use<> {
    watch_rhai_with_duration(path, DEFAULT_WATCH_DURATION, |_| ())
}

// We need different watcher configuration for Rhai source.
/// `on_enqueue` is called on the watcher thread with the result of each [`enqueue`].
fn watch_rhai_with_duration<H>(
    path: &Path,
    duration: Duration,
    on_enqueue: H,
) -> impl Stream<Item = ()> + use<H>
where
    H: Fn(Enqueued) + Send + 'static,
{
    // Due to the vagaries of file watching across multiple platforms, instead of watching the
    // supplied path (file), we are going to watch the parent (directory) of the path.
    let rhai_source_path = PathBuf::from(path);

    let (watch_sender, watch_receiver) = mpsc::channel(1);
    let watch_receiver_stream = tokio_stream::wrappers::ReceiverStream::new(watch_receiver);
    // We can't use the recommended watcher, because there's just too much variation across
    // platforms and file systems. We use the Poll Watcher, which is implemented consistently
    // across all platforms. Less reactive than other mechanisms, but at least it's predictable
    // across all environments. We compare contents as well, which reduces false positives with
    // some additional processing burden.
    let config = Config::default()
        .with_poll_interval(duration)
        .with_compare_contents(true);
    let mut watcher = PollWatcher::new(
        move |res: Result<notify::Event, notify::Error>| {
            match res {
                Ok(event) => {
                    // Let's limit the events we are interested in to:
                    //  - Modified files
                    //  - Created/Remove files
                    //  - with suffix "rhai"
                    if matches!(
                        event.kind,
                        EventKind::Modify(ModifyKind::Metadata(MetadataKind::WriteTime))
                            | EventKind::Modify(ModifyKind::Data(DataChange::Any))
                            | EventKind::Create(_)
                            | EventKind::Remove(_)
                    ) {
                        let mut proceed = false;
                        for path in &event.paths {
                            if path.extension().is_some_and(|ext| ext == "rhai") {
                                proceed = true;
                                break;
                            }
                        }

                        if proceed {
                            on_enqueue(enqueue(&watch_sender));
                        }
                    }
                }
                Err(e) => tracing::error!("rhai watching event error: {:?}", e),
            }
        },
        config,
    )
    .unwrap_or_else(|_| panic!("could not create watch on: {rhai_source_path:?}"));
    watcher
        .watch(&rhai_source_path, RecursiveMode::Recursive)
        .unwrap_or_else(|_| panic!("could not watch: {rhai_source_path:?}"));
    // Tell watchers once they should read the file once,
    // then listen to fs events.
    stream::once(future::ready(()))
        .chain(watch_receiver_stream)
        .chain(stream::once(async move {
            // This exists to give the stream ownership of the hotwatcher.
            // Without it hotwatch will get dropped and the stream will terminate.
            // This code never actually gets run.
            // The ideal would be that hotwatch implements a stream and
            // therefore we don't need this hackery.
            drop(watcher);
        }))
        .boxed()
}
#[cfg(test)]
pub(crate) mod tests {
    use std::env::temp_dir;
    use std::fs::File;
    use std::io::Seek;
    use std::io::Write;
    use std::path::PathBuf;

    use test_log::test;

    use super::*;

    /// How long to wait for the watcher to report an event before failing the test.
    const WATCHER_EVENT_BOUND: Duration = Duration::from_secs(10);

    /// Returns an `on_enqueue` hook for [`watch_with_duration`] / [`watch_rhai_with_duration`]
    /// and the receiver it reports to.
    fn enqueue_reports() -> (
        impl Fn(Enqueued) + Send + 'static,
        tokio::sync::mpsc::UnboundedReceiver<Enqueued>,
    ) {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        (
            move |enqueued| {
                let _ = tx.send(enqueued);
            },
            rx,
        )
    }

    /// Waits for the watcher to report its next event, without consuming the watch stream.
    async fn next_enqueue(
        reports: &mut tokio::sync::mpsc::UnboundedReceiver<Enqueued>,
    ) -> Enqueued {
        tokio::time::timeout(WATCHER_EVENT_BOUND, reports.recv())
            .await
            .expect("watcher did not report an event")
            .expect("watcher dropped")
    }

    #[test(tokio::test)]
    async fn basic_watch() {
        let (path, mut file) = create_temp_file();
        let mtime = file.metadata().unwrap().modified().unwrap();
        overwrite_in_place(&mut file, "Some data 0", mtime);
        let (on_enqueue, mut reports) = enqueue_reports();
        let mut watch = watch_with_duration(&path, Duration::from_millis(100), on_enqueue);
        // Signal telling us we are ready
        assert!(futures::poll!(watch.next()).is_ready());
        overwrite_in_place(&mut file, "Some data 1", mtime);
        assert_eq!(next_enqueue(&mut reports).await, Enqueued::Sent);
        assert!(futures::poll!(watch.next()).is_ready());
        overwrite_in_place(&mut file, "Some data 2", mtime);
        assert_eq!(next_enqueue(&mut reports).await, Enqueued::Sent);
        assert!(futures::poll!(watch.next()).is_ready())
    }

    #[test(tokio::test)]
    async fn clog_watch() {
        let (path, mut file) = create_temp_file();
        let mtime = file.metadata().unwrap().modified().unwrap();
        overwrite_in_place(&mut file, "Some data 0", mtime);
        let (on_enqueue, mut reports) = enqueue_reports();
        let mut watch = watch_with_duration(&path, Duration::from_millis(100), on_enqueue);
        assert!(futures::poll!(watch.next()).is_ready());
        overwrite_in_place(&mut file, "Some data 1", mtime);
        assert_eq!(next_enqueue(&mut reports).await, Enqueued::Sent);
        for contents in ["Some data 2", "Some data 3", "Some data 4"] {
            overwrite_in_place(&mut file, contents, mtime);
            assert_eq!(
                next_enqueue(&mut reports).await,
                Enqueued::AlreadyPending,
                "the watcher should drop events while one is pending"
            );
        }
        assert!(
            futures::poll!(watch.next()).is_ready(),
            "polling the future should notice the event"
        );
        assert!(
            !futures::poll!(watch.next()).is_ready(),
            "should only have one event for multiple updates"
        );
    }

    // Rapid rhai file changes while the state machine is busy must not block an
    // OS thread in a retry loop that can panic when the channel is eventually
    // closed.  The watcher must drop duplicate events instead of retrying so
    // that the callback always returns promptly.
    #[test(tokio::test)]
    async fn clog_watch_rhai() {
        let dir = tempfile::tempdir().unwrap();
        let rhai_path = dir.path().join("script.rhai");
        let mut file = std::fs::File::create(&rhai_path).unwrap();
        let mtime = file.metadata().unwrap().modified().unwrap();
        overwrite_in_place(&mut file, "// v0", mtime);
        let (on_enqueue, mut reports) = enqueue_reports();
        let mut watch =
            watch_rhai_with_duration(&rhai_path, Duration::from_millis(100), on_enqueue);
        assert!(futures::poll!(watch.next()).is_ready());
        overwrite_in_place(&mut file, "// v1", mtime);
        assert_eq!(next_enqueue(&mut reports).await, Enqueued::Sent);
        for contents in ["// v2", "// v3", "// v4"] {
            overwrite_in_place(&mut file, contents, mtime);
            assert_eq!(
                next_enqueue(&mut reports).await,
                Enqueued::AlreadyPending,
                "the watcher should drop events while one is pending"
            );
        }
        assert!(
            futures::poll!(watch.next()).is_ready(),
            "polling the future should notice the event"
        );
        assert!(
            !futures::poll!(watch.next()).is_ready(),
            "should only have one event for multiple updates"
        );
    }

    pub(crate) fn create_temp_file() -> (PathBuf, File) {
        let path = temp_dir().join(format!("{}", uuid::Uuid::new_v4()));
        let file = std::fs::File::create(&path).unwrap();
        (path, file)
    }

    pub(crate) async fn write_and_flush(file: &mut File, contents: &str) {
        file.rewind().unwrap();
        file.set_len(0).unwrap();
        file.write_all(contents.as_bytes()).unwrap();
        file.flush().unwrap();
        tokio::time::sleep(Duration::from_millis(500)).await;
    }

    /// Overwrites `file` so the poll watcher reports exactly one event for it.
    ///
    /// `contents` must differ from the previous contents in a single byte, with no change in
    /// length, so the watcher never reads a partial write: it only ever sees the old
    /// contents or the new ones. So the first contents must be written before watching. The watcher also reports a newer mtime (in whole seconds)
    /// as a change, so restoring `mtime` stops it reporting a write it has already hashed a
    /// second time, when it checked the mtime just before the write and hashed the contents
    /// just after.
    fn overwrite_in_place(file: &mut File, contents: &str, mtime: std::time::SystemTime) {
        file.rewind().unwrap();
        file.write_all(contents.as_bytes()).unwrap();
        file.flush().unwrap();
        file.set_modified(mtime).unwrap();
    }
}
