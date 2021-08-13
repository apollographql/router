use futures::channel::mpsc;
use futures::prelude::*;
use hotwatch::Hotwatch;
use std::path::PathBuf;
use std::time::Duration;

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
            if let hotwatch::Event::Write(_path) = event {
                if let Err(_err) = watch_sender.try_send(()) {
                    log::error!(
                        "Failed to process file watch notification. {}",
                        _err.to_string()
                    )
                }
            }
        })
        .expect("Failed to watch file.");
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
    use futures::prelude::*;
    use std::fs::File;
    use std::io::{Seek, SeekFrom, Write};

    use super::*;
    use std::env::temp_dir;

    #[ctor::ctor]
    fn init() {
        let _ = env_logger::builder().is_test(true).try_init();
    }

    #[cfg(not(target_os = "macos"))]
    #[tokio::test]
    async fn basic_watch() {
        let (path, mut file) = create_temp_file();
        let mut watch = watch(path, Some(Duration::from_millis(10)));
        watch.next().await;
        assert!(futures::poll!(watch.next()).is_pending());
        write_and_flush(&mut file, "Some data").await;
        watch.next().await;
        assert!(futures::poll!(watch.next()).is_pending());
        write_and_flush(&mut file, "Some data").await;
        watch.next().await;
        assert!(futures::poll!(watch.next()).is_pending())
    }

    #[cfg(test)]
    pub(crate) fn create_temp_file() -> (PathBuf, File) {
        let path = temp_dir().join(format!("{}", uuid::Uuid::new_v4()));
        let file = std::fs::File::create(path.to_owned()).unwrap();
        (path, file)
    }

    #[cfg(test)]
    #[must_use]
    pub(crate) async fn write_and_flush(file: &mut File, contents: &str) {
        file.set_len(0).unwrap();
        file.seek(SeekFrom::Start(0)).unwrap();
        file.write(contents.as_bytes()).unwrap();
        file.flush().unwrap();
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}
