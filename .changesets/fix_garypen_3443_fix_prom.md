### Fix prometheus statistics issues with _total_total names([Issue #3443](https://github.com/apollographql/router/issues/3443))

When producing prometheus statistics the otel crate (0.19.0) now automatically appends "_total" which is unhelpful.

This fix remove duplicated "_total_total" from our statistics.

By [@garypen](https://github.com/garypen) in https://github.com/apollographql/router/pull/3471