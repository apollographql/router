### Drop duplicate Rhai script watcher notifications when the change channel is full ([PR #9391](https://github.com/apollographql/router/pull/9391))

When many filesystem events arrived in quick succession, the Rhai script watcher could spin an OS thread or panic — the previous retry loop kept trying to send on a full channel, and would panic if the receiver closed before the retry succeeded.

The watcher now drops duplicate notifications when the channel is already full, matching the behavior introduced for the configuration file watcher in [PR #8336](https://github.com/apollographql/router/pull/8336). Reloads always re-read the current file from disk, so a single pending notification in the channel is sufficient to guarantee the latest contents will be picked up.

By [@carodewig](https://github.com/carodewig) in https://github.com/apollographql/router/pull/9391
