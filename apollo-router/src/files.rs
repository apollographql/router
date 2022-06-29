use std::path::PathBuf;
use std::time::Duration;

use futures::channel::mpsc;
use futures::prelude::*;
use hotwatch::Hotwatch;

/// Creates a stream events whenever the file at the path has changes. The stream never terminates
/// and must be dropped to finish watching.
///
/// # Arguments
///
/// * `path`: The file to watch
/// * `delay`: The delay before sending an event. This prevents duplicate events for large writes. Defaults to 2 seconds.
///
/// returns: impl Stream<Item=()>
///
pub(crate) fn watch(path: PathBuf, delay: Option<Duration>) -> impl Stream<Item = ()> {
    let (mut watch_sender, watch_receiver) = mpsc::channel(1);
    let mut watcher =
        Hotwatch::new_with_custom_delay(delay.unwrap_or_else(|| Duration::from_secs(2)))
            .expect("Failed to initialise file watching.");
    watcher
        .watch(path, move |event: hotwatch::Event| {
            tracing::debug!("file watcher: received event: {:?}", &event);
            match event {
                // https://github.com/notify-rs/notify/blob/ded07f442a96f33c6b7fefe3195396a33a28ddc3/src/lib.rs#L413
                //
                // `Create` events have a higher priority than `Write` and `Chmod`. These events will not be
                // emitted if they are detected before the `Create` event has been emitted.
                //
                // Hotwatch will sometimes send a Create event instead of a Write event,
                // if the write occured immediately after the file creation.
                hotwatch::Event::Write(_) | hotwatch::Event::Create(_) => {
                    if let Err(_err) = watch_sender.try_send(()) {
                        tracing::error!(
                            "Failed to process file watch notification. {}",
                            _err.to_string()
                        )
                    }
                }
                _ => {}
            }
        })
        .expect("Failed to watch file.");
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

    use test_log::test;

    use super::*;

    #[test(tokio::test)]
    async fn basic_watch() {
        let (path, mut file) = create_temp_file();
        let mut watch = watch(path, Some(Duration::from_millis(10)));
        // Signal telling us to read the file once, and then poll.
        assert!(futures::poll!(watch.next()).is_ready());

        assert!(futures::poll!(watch.next()).is_pending());
        write_and_flush(&mut file, "Some data").await;
        assert!(futures::poll!(watch.next()).is_pending());
        write_and_flush(&mut file, "Some data").await;
        assert!(futures::poll!(watch.next()).is_pending())
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
    }
}
