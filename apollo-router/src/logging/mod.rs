#[macro_export]
/// This is a really simple macro to assert a snapshot of the logs.
/// To use it call `.with_subscriber(assert_snapshot_subscriber!())` in your test just before calling `await`.
/// This will assert a snapshot of the logs in pretty yaml format.
/// You can also use subscriber::with_default(assert_snapshot_subscriber!(), || { ... }) to assert the logs in non async code.
macro_rules! assert_snapshot_subscriber {
    () => {
        $crate::assert_snapshot_subscriber!(tracing_core::LevelFilter::INFO, {})
    };

    ($redactions:tt) => {
        $crate::assert_snapshot_subscriber!(tracing_core::LevelFilter::INFO, $redactions)
    };

    ($level:expr) => {
        $crate::assert_snapshot_subscriber!($level, {})
    };

    ($level:expr, $redactions:tt) => {
        $crate::logging::test::SnapshotSubscriber::create_subscriber($level, |yaml| {
            insta::with_settings!({sort_maps => true}, {
                // the tests here will force maps to sort
                let mut settings = insta::Settings::clone_current();
                settings.set_snapshot_suffix("logs");
                settings.set_sort_maps(true);
                settings.bind(|| {
                    insta::assert_yaml_snapshot!(yaml, $redactions);
                });
            });
        })
    };
}

#[cfg(test)]
pub(crate) mod test {
    use std::sync::Arc;
    use std::sync::Mutex;

    use serde_json::Value;
    use tracing_core::LevelFilter;
    use tracing_core::Subscriber;
    use tracing_subscriber::layer::SubscriberExt;

    use crate::plugins::telemetry::dynamic_attribute::DynSpanAttributeLayer;

    pub(crate) struct SnapshotSubscriber {
        buffer: Arc<Mutex<Vec<u8>>>,
        assertion: fn(serde_json::Value),
    }

    impl std::io::Write for SnapshotSubscriber {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            let buf_len = buf.len();
            self.buffer.lock().unwrap().append(&mut buf.to_vec());
            Ok(buf_len)
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl Drop for SnapshotSubscriber {
        fn drop(&mut self) {
            let log = String::from_utf8(self.buffer.lock().unwrap().to_vec()).unwrap();
            let parsed: Value = if log.is_empty() {
                serde_json::json!([])
            } else {
                let parsed_log: Vec<Value> = log
                    .lines()
                    .map(|line| {
                        let mut line: serde_json::Value = serde_json::from_str(line).unwrap();
                        // move the message field to the top level
                        let fields = line
                            .as_object_mut()
                            .unwrap()
                            .get_mut("fields")
                            .unwrap()
                            .as_object_mut()
                            .unwrap();
                        let message = fields.remove("message").unwrap_or_default();
                        line.as_object_mut()
                            .unwrap()
                            .insert("message".to_string(), message);
                        line
                    })
                    .collect();
                serde_json::json!(parsed_log)
            };

            (self.assertion)(parsed)
        }
    }

    impl SnapshotSubscriber {
        pub(crate) fn create_subscriber(
            level: LevelFilter,
            assertion: fn(Value),
        ) -> impl Subscriber {
            let collector = Self {
                buffer: Arc::new(Mutex::new(Vec::new())),
                assertion,
            };

            tracing_subscriber::registry::Registry::default()
                .with(level)
                .with(DynSpanAttributeLayer::new())
                .with(
                    tracing_subscriber::fmt::Layer::default()
                        .json()
                        .without_time()
                        .with_target(false)
                        .with_file(false)
                        .with_line_number(false)
                        .with_writer(Mutex::new(collector)),
                )
        }
    }
}
