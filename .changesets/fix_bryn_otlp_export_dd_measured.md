### OTLP trace exporter does not correctly display resources in Datadog APM view ([PR #7344](https://github.com/apollographql/router/pull/7344))

Router 2.x Datadog APM view is now fixed when using `preview_datadog_agent_sampling`. This was underreporting requests due to missing `_dd.measured` attributes.

By [@BrynCooke](https://github.com/BrynCooke) in https://github.com/apollographql/router/pull/7344