### Warn on low scheduled_delay for Apollo exporters ([PR #8260](https://github.com/apollographql/router/pull/8260))

Adds a warning if any of the scheduled_delay configs for the apollo metrics and traces are set to below 1s. In the next major Router version we intend to start enforcing this minimum by taking the lowest of the configured value and the minimum. We plan to monitor what customers are setting this value to and adjust the minimum accordingly.

By [@bonnici](https://github.com/bonnici) in https://github.com/apollographql/router/pull/8260
