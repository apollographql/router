#[cfg(test)]
mod traced_span_tests {
    use std::sync::Arc;
    use test_span::prelude::*;

    #[test_span]
    fn tracing_macro_works() {
        let res = do_sync_stuff();

        assert_eq!(res, 104);

        let res2 = do_sync_stuff();

        assert_eq!(res2, 104);

        let logs = get_logs();

        dbg!(&logs);

        assert!(logs.contains_message("here i am!"));
        assert!(logs.contains_value("number", RecordedValue::Value(52.into())));
        assert!(logs.contains_message("in a separate context!"));

        insta::assert_json_snapshot!(logs);
        insta::assert_json_snapshot!(get_span());
    }

    #[test_span(tokio::test)]
    async fn async_tracing_macro_works() {
        let expected = (104, 104);
        let actual = futures::join!(do_stuff(), do_stuff());
        assert_eq!(expected, actual);

        let logs = get_logs();

        assert!(logs.contains_message("here i am!"));
        assert!(logs.contains_value("number", RecordedValue::Value(52.into())));
        assert!(logs.contains_message("in a separate context!"));

        insta::assert_json_snapshot!(logs);
        insta::assert_json_snapshot!(get_span());
    }

    #[test]
    fn tracing_works() {
        let id_sequence = Default::default();
        let all_spans = Default::default();
        let logs = Default::default();

        let subscriber = tracing_subscriber::registry().with(Layer::new(
            Arc::clone(&id_sequence),
            Arc::clone(&all_spans),
            Arc::clone(&logs),
        ));

        let logs_clone = Arc::clone(&logs);
        let span_logs_clone = Arc::clone(&logs);
        let spans_clone = Arc::clone(&all_spans);
        let id_sequence_clone = Arc::clone(&id_sequence);

        let get_logs = move || logs_clone.lock().unwrap().contents();

        let get_span = move || {
            let all_spans = spans_clone.lock().unwrap().clone();
            let all_logs = span_logs_clone.lock().unwrap().clone();
            let id_sequence = id_sequence_clone.read().unwrap().clone();
            Span::from_records(id_sequence, all_logs, all_spans)
        };

        subscriber.init();

        let res = do_sync_stuff();

        assert_eq!(res, 104);

        let res2 = do_sync_stuff();

        assert_eq!(res2, 104);

        let logs = get_logs();

        assert!(logs.contains_message("here i am!"));
        assert!(logs.contains_value("number", RecordedValue::Value(52.into())));
        assert!(logs.contains_message("in a separate context!"));

        insta::assert_json_snapshot!(logs);
        insta::assert_json_snapshot!(get_span());
    }

    #[tokio::test]
    async fn async_tracing_works() {
        let id_sequence = Default::default();
        let all_spans = Default::default();
        let logs = Default::default();

        let subscriber = tracing_subscriber::registry().with(Layer::new(
            Arc::clone(&id_sequence),
            Arc::clone(&all_spans),
            Arc::clone(&logs),
        ));

        let logs_clone = Arc::clone(&logs);
        let span_logs_clone = Arc::clone(&logs);
        let spans_clone = Arc::clone(&all_spans);
        let id_sequence_clone = Arc::clone(&id_sequence);

        let get_logs = move || logs_clone.lock().unwrap().contents();

        let get_span = move || {
            let all_spans = spans_clone.lock().unwrap().clone();
            let all_logs = span_logs_clone.lock().unwrap().clone();
            let id_sequence = id_sequence_clone.read().unwrap().clone();
            Span::from_records(id_sequence, all_logs, all_spans)
        };

        async {
            let res = do_stuff().await;

            assert_eq!(res, 104);

            let res2 = do_stuff().await;

            assert_eq!(res2, 104);
        }
        .with_subscriber(subscriber)
        .await;

        let logs = get_logs();

        assert!(logs.contains_message("here i am!"));
        assert!(logs.contains_value("number", RecordedValue::Value(52.into())));
        assert!(logs.contains_message("in a separate context!"));

        insta::assert_json_snapshot!(logs);
        insta::assert_json_snapshot!(get_span());
    }

    #[tracing::instrument(level = "info")]
    fn do_sync_stuff() -> u8 {
        let number = do_sync_stuff_2(42);

        std::thread::spawn(|| tracing::warn!("in a separate context!"))
            .join()
            .unwrap();
        tracing::info!("here i am!");

        tracing::info!(number);

        number * 2
    }

    #[tracing::instrument(target = "my_crate::an_other_target", level = "info")]
    fn do_sync_stuff_2(number: u8) -> u8 {
        tracing::info!("here i am again!");

        number + 10
    }

    #[tracing::instrument(name = "do_stuff", level = "info")]
    async fn do_stuff() -> u8 {
        let number = do_stuff_2(42).await;

        tokio::task::spawn_blocking(|| async { tracing::warn!("in a separate context!") })
            .await
            .unwrap()
            .await;
        tracing::info!("here i am!");

        tracing::info!(number);

        number * 2
    }

    #[tracing::instrument(
        name = "do_stuff2",
        target = "my_crate::an_other_target",
        level = "info"
    )]
    async fn do_stuff_2(number: u8) -> u8 {
        tracing::info!("here i am again!");

        number + 10
    }
}
