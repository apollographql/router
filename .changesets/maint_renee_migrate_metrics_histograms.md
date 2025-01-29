### Migrate histogram metrics to `{f,u}64_histogram!` ([PR #6356](https://github.com/apollographql/router/pull/6356))

Updates histogram metrics using the legacy `tracing::info!(histogram.*)` syntax to the new metrics macros.

By [@goto-bus-stop](https://github.com/goto-bus-stop) in https://github.com/apollographql/router/pull/6356