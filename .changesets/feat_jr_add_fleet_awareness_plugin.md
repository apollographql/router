### Adds Fleet Awareness Plugin

Adds a new plugin that reports telemetry to Apollo on the configuration and deployment of the Router. Initially only
reports, memory and CPU usage but will be expanded to cover other non-intrusive measures in future. 🚀

As part of the above PluginPrivate has been extended with a new `activate` hook which is guaranteed to be called once
the OTEL meter has been refreshed. This ensures that code, particularly that concerned with gauges, will survive a hot
reload of the router.

By [@jonathanrainer](https://github.com/jonathanrainer), [@nmoutschen](https://github.com/nmoutschen), [@loshz](https://github.com/loshz)
in https://github.com/apollographql/router/pull/6151
