### Rhai script file watcher no longer spins or panics when change events pile up ([PR #9391](https://github.com/apollographql/router/pull/9391))

The rhai file watcher had a retry loop that kept trying to send on a full channel every 50ms when multiple filesystem events arrived in quick succession. This caused an OS thread to spin under reload pressure, and would panic if the channel receiver was closed before the retry loop succeeded.

The watcher now drops duplicate notifications when the channel is already full, matching the behavior introduced for the configuration file watcher in [PR #8336](https://github.com/apollographql/router/pull/8336). Because reloads always read the current file from disk at the time of the reload, a pending notification in the channel is sufficient to guarantee the latest contents will be picked up.

By [@carodewig](https://github.com/carodewig) in https://github.com/apollographql/router/pull/9391
