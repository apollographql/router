### Add new configurable delivery pathway for high cardinality Apollo Studio metrics ([PR #7138](https://github.com/apollographql/router/pull/7138))

This change provides a secondary pathway for new "realtime" Studio metrics whose delivery interval is configurable due to their higher cardinality. These metrics will respect `telemetry.apollo.batch_processor.scheduled_delay` as configured on the realtime path.

All other Apollo metrics will maintain the previous hardcoded 60s send interval.

By [@rregitsky](https://github.com/rregitsky) in https://github.com/apollographql/router/pull/7138
