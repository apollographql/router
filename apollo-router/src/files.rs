use std::path::Path;
use std::time::Duration;

use futures::channel::mpsc;
use futures::prelude::*;
use notify::RecursiveMode;
use notify::Watcher;

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
    let (mut watch_sender, watch_receiver) = mpsc::channel(1);
    let mut watcher =
        notify::recommended_watcher(move |res: Result<notify::Event, notify::Error>| match res {
            Ok(event) => {
                // We are only interested in  modify events.
                // We don't want to lose events and a slow consumer could make us
                // miss an event notification without a re-send strategy.
                // If we can't send the event because the channel is full, wait
                // for a short while and try again. Otherwise, we will panic
                // because it's a non-recoverable error.
                if let notify::event::EventKind::Modify(_) = event.kind {
                    loop {
                        match watch_sender.try_send(()) {
                            Ok(_) => break,
                            Err(err) => {
                                tracing::warn!(
                                    "could not process file watch notification. {}",
                                    err.to_string()
                                );
                                if err.is_full() {
                                    std::thread::sleep(Duration::from_millis(50));
                                } else {
                                    panic!("event channel failed: {}", err);
                                }
                            }
                        }
                    }
                }
            }
            Err(e) => eprintln!("event error: {:?}", e),
        })
        .unwrap_or_else(|_| panic!("could not create watch on: {:?}", path));
    watcher
        .watch(path, RecursiveMode::NonRecursive)
        .unwrap_or_else(|_| panic!("could not watch: {:?}", path));
    // Tell watchers once they should read the file once,
    // then listen to fs events.
    stream::once(future::ready(()))
        .chain(watch_receiver)
        .chain(stream::once(async move {
            // This exists to give the stream ownership of the hotwatcher.
            // Without it hotwatch will get dropped and the stream will terminate.
            // This code never actually gets run.
            //The ideal would be that hotwatch implements a stream and
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
    use std::io::SeekFrom;
    use std::io::Write;
    use std::path::PathBuf;

    use test_log::test;

    use super::*;

    #[test(tokio::test)]
    async fn basic_watch() {
        let (path, mut file) = create_temp_file();
        let mut watch = watch(&path);
        // This test can be very racy. Without synchronisation, all
        // we can hope is that if we wait long enough between each
        // write/flush then the future will become ready.
        // Signal telling us we are ready
        assert!(futures::poll!(watch.next()).is_ready());
        write_and_flush(&mut file, "Some data").await;
        assert!(futures::poll!(watch.next()).is_ready());
        write_and_flush(&mut file, "Some data").await;
        assert!(futures::poll!(watch.next()).is_ready())
    }

    #[cfg(test)]
    pub(crate) fn create_temp_file() -> (PathBuf, File) {
        let path = temp_dir().join(format!("{}", uuid::Uuid::new_v4()));
        let file = std::fs::File::create(&path).unwrap();
        (path, file)
    }

    #[cfg(test)]
    pub(crate) async fn write_and_flush(file: &mut File, contents: &str) {
        file.seek(SeekFrom::Start(0)).unwrap();
        file.set_len(0).unwrap();
        file.write_all(contents.as_bytes()).unwrap();
        file.flush().unwrap();
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}
