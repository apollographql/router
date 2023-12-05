use std::path::Path;
use std::path::PathBuf;
use std::time::Duration;

use futures::prelude::*;
use notify::event::DataChange;
use notify::event::MetadataKind;
use notify::event::ModifyKind;
use notify::Config;
use notify::EventKind;
use notify::PollWatcher;
use notify::RecursiveMode;
use notify::Watcher;
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
pub(crate) fn watch(path: &Path) -> impl Stream<Item = ()> {
    watch_with_duration(path, DEFAULT_WATCH_DURATION)
}

fn watch_with_duration(path: &Path, duration: Duration) -> impl Stream<Item = ()> {
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
                    loop {
                        match watch_sender.try_send(()) {
                            Ok(_) => break,
                            Err(err) => {
                                tracing::warn!(
                                    "could not process file watch notification. {}",
                                    err.to_string()
                                );
                                if matches!(err, TrySendError::Full(_)) {
                                    std::thread::sleep(Duration::from_millis(50));
                                } else {
                                    panic!("event channel failed: {err}");
                                }
                            }
                        }
                    }
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

#[cfg(test)]
pub(crate) mod tests {
    use std::env::temp_dir;
    use std::fs::File;
    use std::io::Seek;
    use std::io::Write;
    use std::path::PathBuf;

    use test_log::test;

    use super::*;

    #[test(tokio::test)]
    async fn basic_watch() {
        let (path, mut file) = create_temp_file();
        let mut watch = watch_with_duration(&path, Duration::from_millis(100));
        // This test can be very racy. Without synchronisation, all
        // we can hope is that if we wait long enough between each
        // write/flush then the future will become ready.
        // Signal telling us we are ready
        assert!(futures::poll!(watch.next()).is_ready());
        write_and_flush(&mut file, "Some data 1").await;
        assert!(futures::poll!(watch.next()).is_ready());
        write_and_flush(&mut file, "Some data 2").await;
        assert!(futures::poll!(watch.next()).is_ready())
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
}
