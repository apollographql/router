#[cfg(test)]
mod traced_span_tests {
    use test_span::prelude::*;

    #[test_span]
    fn tracing_macro_works() {
        let res = do_sync_stuff();

        assert_eq!(res, 104);

        let res2 = do_sync_stuff();

        assert_eq!(res2, 104);

        let (spans, logs) = get_telemetry();

        assert!(logs.contains_message("here i am!"));
        assert!(logs.contains_value("number", RecordValue::Value(52.into())));
        assert!(logs.contains_message("here i am again!"));
        assert!(logs.contains_message("debug: here i am again!"),);

        insta::assert_json_snapshot!(logs);
        insta::assert_json_snapshot!(spans);

        assert_eq!(spans, get_spans());
        assert_eq!(logs, get_logs());
    }

    #[test_span]
    #[level(tracing::Level::INFO)]
    fn tracing_macro_works_with_other_level() {
        let res = do_sync_stuff();

        assert_eq!(res, 104);

        let res2 = do_sync_stuff();

        assert_eq!(res2, 104);

        let (spans, logs) = get_telemetry();

        assert!(logs.contains_message("here i am!"));
        assert!(logs.contains_value("number", RecordValue::Value(52.into())));
        assert!(
            !logs.contains_message("debug: here i am again!"),
            "DEBUG logs shouldn't appear since we explicitly asked for INFO and below.",
        );
        insta::assert_json_snapshot!(logs);
        insta::assert_json_snapshot!(spans);

        assert_eq!(spans, get_spans());
        assert_eq!(logs, get_logs());
    }

    #[test_span(tokio::test)]
    async fn async_tracing_macro_works() {
        let expected = (104, 104);
        let actual = futures::join!(do_async_stuff(), do_async_stuff());
        assert_eq!(expected, actual);

        let (spans, logs) = get_telemetry();

        assert!(logs.contains_message("here i am!"));
        assert!(logs.contains_value("number", RecordValue::Value(52.into())));
        assert!(logs.contains_message("in a separate context!"));

        insta::assert_json_snapshot!(logs);
        insta::assert_json_snapshot!(spans);

        assert_eq!(spans, get_spans());
        assert_eq!(logs, get_logs());
    }

    #[test]
    fn tracing_works() {
        test_span::init();

        let root_id = {
            let root_span = test_span::reexports::tracing::span!(::tracing::Level::DEBUG, "root");

            let root_id = root_span
                .id()
                .expect("couldn't get root span id; this cannot happen.");
            root_span.in_scope(|| {
                let res = do_sync_stuff();
                assert_eq!(res, 104);

                let res2 = do_sync_stuff();

                assert_eq!(res2, 104);
            });
            root_id
        };

        let get_telemetry = || test_span::get_telemetry_for_root(&root_id, &Level::DEBUG);

        let (spans, logs) = get_telemetry();

        assert!(logs.contains_message("here i am!"));
        assert!(logs.contains_value("number", RecordValue::Value(52.into())));

        insta::assert_json_snapshot!(logs);
        insta::assert_json_snapshot!(spans);
    }

    #[tokio::test]
    async fn async_tracing_works() {
        test_span::init();

        let root_id = {
            let root_span = test_span::reexports::tracing::span!(::tracing::Level::INFO, "root");
            let root_id = root_span
                .id()
                .expect("couldn't get root span id; this cannot happen.");
            async {
                let res = do_async_stuff().await;

                assert_eq!(res, 104);

                let res2 = do_async_stuff().await;

                assert_eq!(res2, 104);
            }
            .instrument(root_span)
            .await;
            root_id
        };
        let get_telemetry = || test_span::get_telemetry_for_root(&root_id, &tracing::Level::INFO);

        let (spans, logs) = get_telemetry();

        assert!(logs.contains_message("here i am!"));
        assert!(logs.contains_value("number", RecordValue::Value(52.into())));
        assert!(logs.contains_message("in a separate context!"));

        insta::assert_json_snapshot!(logs);
        insta::assert_json_snapshot!(spans);
    }

    #[test_span]
    fn get_all_logs_to_have_thread_support() {
        std::thread::spawn(|| {
            tracing::info!("will only show up in get_all_logs!");
            tracing::debug!("will only show up in DEBUG get_all_logs!");
        })
        .join()
        .unwrap();

        let test_run_logs = get_logs();
        assert!(!test_run_logs.contains_message("will only show up in get_all_logs!"));
        assert!(!test_run_logs.contains_message("will only show up in DEBUG get_all_logs!"));

        let all_logs = test_span::get_all_logs(&tracing::Level::INFO);
        assert!(all_logs.contains_message("will only show up in get_all_logs!"));
        assert!(!all_logs.contains_message("will only show up in DEBUG get_all_logs!"));

        let all_debug_logs = test_span::get_all_logs(&tracing::Level::DEBUG);
        assert!(all_debug_logs.contains_message("will only show up in get_all_logs!"));
        assert!(all_debug_logs.contains_message("will only show up in DEBUG get_all_logs!"));
    }

    #[tracing::instrument(name = "do_sync_stuff", level = "info")]
    fn do_sync_stuff() -> u8 {
        tracing::info!("here i am!");

        let number = do_sync_stuff_2(42);

        tracing::info!(number);

        number * 2
    }

    #[tracing::instrument(
        name = "do_sync_stuff2",
        target = "my_crate::an_other_target",
        level = "info"
    )]
    fn do_sync_stuff_2(number: u8) -> u8 {
        tracing::info!("here i am again!");

        tracing::debug!("debug: here i am again!");

        number + 10
    }

    #[tracing::instrument(name = "do_async_stuff", level = "info")]
    async fn do_async_stuff() -> u8 {
        tracing::info!("here i am!");
        let number = do_async_stuff_2(42).await;

        tokio::task::spawn_blocking(|| async { tracing::warn!("in a separate context!") })
            .await
            .unwrap()
            .await;
        tracing::info!(number);

        number * 2
    }

    #[tracing::instrument(
        name = "do_async_stuff2",
        target = "my_crate::an_other_target",
        level = "info"
    )]
    async fn do_async_stuff_2(number: u8) -> u8 {
        tracing::debug!("here i am again!");

        number + 10
    }
}
