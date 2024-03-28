### Give spans their proper parent in the plugin stack ([Issue #4827](https://github.com/apollographql/router/issues/4827))

Due to the way plugin spans were created and applied, they would appear as siblings instead of being nested, which creates some issues when displaying traces and accounting for time spent in Datadog. Plugin spans are now correctly nested within each other.

By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/4877